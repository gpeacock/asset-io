//! Incremental metadata revisions; original file bytes are never rewritten.

use super::*;
use crate::sans_io::io::{PDst, Src};
use crate::updates::MetadataUpdate;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) enum State {
    Ready(Arc<Context>),
    Blocked(&'static str),
    Invalid(String),
}

#[derive(Clone, Debug)]
pub(super) struct Context {
    catalog: Dictionary,
    names: Dictionary,
    tree: Dictionary,
    pairs: BTreeMap<Vec<u8>, Value>,
    associated: Vec<Value>,
    active: BTreeSet<Reference>,
    root: Reference,
    next_id: u32,
    previous: u64,
    trailer: Dictionary,
    revisions: usize,
    budget: Budget,
}

#[derive(Clone, Debug)]
struct Budget {
    syntax: u64,
    nodes: usize,
    objects: usize,
    entries: usize,
    classic: usize,
}

fn unsupported(reason: &'static str) -> Error {
    Error::UnsupportedFeature(reason)
}

pub(super) async fn context(
    parser: &mut Parser<'_>,
    root: Reference,
    previous: u64,
    catalog: Dictionary,
    associated: Vec<Value>,
    active: Option<BTreeSet<Reference>>,
) -> Result<State> {
    match prepare(parser, root, previous, catalog, associated, active).await {
        Ok(context) => Ok(State::Ready(Arc::new(context))),
        Err(Error::UnsupportedFeature(reason)) => Ok(State::Blocked(reason)),
        Err(Error::InvalidFormat(message)) => {
            log::warn!("PDF editing context is unavailable: {message}");
            Ok(State::Invalid(message))
        }
        Err(error) => Err(error),
    }
}

async fn dictionary(parser: &mut Parser<'_>, value: Option<Value>) -> Result<Dictionary> {
    match value {
        None | Some(Value::Null) => Ok(Dictionary::new()),
        Some(value) => parser.resolve(value).await?.dictionary(),
    }
}

async fn check_form(parser: &mut Parser<'_>, value: Value) -> Result<()> {
    let form = parser.resolve(value).await?;
    if matches!(form, Value::Null) {
        return Ok(());
    }
    let mut form = form.dictionary()?;
    if form
        .get(b"XFA".as_slice())
        .is_some_and(|value| !matches!(value, Value::Null))
    {
        return Err(unsupported(
            "PDF metadata writing does not support XFA forms",
        ));
    }
    let fields = required(&mut form, b"Fields")?;
    let mut remaining = MAX_OBJECTS;
    check_fields(parser, fields, None, None, 0, &mut remaining).await
}

fn check_fields<'b>(
    parser: &'b mut Parser<'_>,
    fields: Value,
    inherited_type: Option<&'b Value>,
    inherited_value: Option<&'b Value>,
    depth: usize,
    remaining: &'b mut usize,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'b>> {
    Box::pin(async move {
        if depth >= MAX_DEPTH {
            return Err(invalid("AcroForm field depth limit exceeded"));
        }
        let Value::Array(fields) = parser.resolve(fields).await? else {
            return Err(invalid("AcroForm Fields/Kids must be an array"));
        };
        for field in fields {
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| invalid("AcroForm field count limit exceeded"))?;
            let mut field = parser.resolve(field).await?.dictionary()?;
            // Roots cannot inherit from an unvisited parent outside the field tree.
            if depth == 0
                && field
                    .get(b"Parent".as_slice())
                    .is_some_and(|value| !matches!(value, Value::Null))
            {
                return Err(invalid("AcroForm root field has a Parent"));
            }
            let own_type = match field.remove(b"FT".as_slice()) {
                Some(value) => Some(parser.resolve(value).await?),
                None => None,
            };
            let field_type = own_type.as_ref().or(inherited_type);
            if field_type.is_some_and(|value| !matches!(value, Value::Name(_))) {
                return Err(invalid("AcroForm FT must be a name"));
            }
            let own_value = match field.remove(b"V".as_slice()) {
                Some(value) => Some(parser.resolve(value).await?),
                None => None,
            };
            let value = own_value.as_ref().or(inherited_value);
            if field_type.is_some_and(|value| value.is_name(b"Sig"))
                && value.is_some_and(|value| !matches!(value, Value::Null))
            {
                return Err(unsupported(
                    "PDF metadata writing does not support populated signature fields",
                ));
            }
            if let Some(kids) = field.remove(b"Kids".as_slice()) {
                check_fields(parser, kids, field_type, value, depth + 1, remaining).await?;
            }
        }
        Ok(())
    })
}

async fn prepare(
    parser: &mut Parser<'_>,
    root: Reference,
    previous: u64,
    catalog: Dictionary,
    associated: Vec<Value>,
    active: Option<BTreeSet<Reference>>,
) -> Result<Context> {
    if catalog
        .get(b"Perms".as_slice())
        .is_some_and(|value| !matches!(value, Value::Null))
    {
        return Err(unsupported(
            "PDF metadata writing does not support catalog permissions/certification (Perms)",
        ));
    }
    if let Some(form) = catalog.get(b"AcroForm".as_slice()) {
        check_form(parser, form.clone()).await?;
    }
    let active = active
        .ok_or_else(|| unsupported("writing PDF C2PA requires indirect file specifications"))?;
    let mut names = dictionary(parser, catalog.get(b"Names".as_slice()).cloned()).await?;
    let mut tree = dictionary(parser, names.remove(b"EmbeddedFiles".as_slice())).await?;
    if tree.contains_key(b"Kids".as_slice()) {
        return Err(unsupported(
            "writing PDF EmbeddedFiles name trees with Kids is not supported",
        ));
    }
    let values = match tree.remove(b"Names".as_slice()) {
        None => Vec::new(),
        Some(value) => match parser.resolve(value).await? {
            Value::Array(values) => values,
            _ => return Err(invalid("EmbeddedFiles Names must be an array")),
        },
    };
    if values.len() % 2 != 0 {
        return Err(invalid("odd EmbeddedFiles name array"));
    }
    let mut pairs = BTreeMap::new();
    let mut values = values.into_iter();
    let mut previous_name = None;
    while let Some(key) = values.next() {
        let Value::Bytes(key) = parser.resolve(key).await? else {
            return Err(unsupported("PDF attachment names must be strings"));
        };
        if previous_name
            .as_ref()
            .is_some_and(|previous| previous >= &key)
        {
            return Err(invalid("unordered or duplicate EmbeddedFiles names"));
        }
        previous_name = Some(key.clone());
        pairs.insert(
            key,
            values
                .next()
                .ok_or_else(|| invalid("missing attachment value"))?,
        );
    }
    for reference in &active {
        if !pairs
            .values()
            .any(|value| matches!(value, Value::Reference(id) if id == reference))
        {
            return Err(unsupported(
                "writing PDF C2PA requires registration in a flat EmbeddedFiles name tree",
            ));
        }
    }
    // A new standalone leaf has no parent requiring Limits; keep other unknown entries.
    tree.remove(b"Limits".as_slice());
    let mut trailer = parser.trailer.clone();
    for key in [
        b"Type".as_slice(),
        b"W",
        b"Index",
        b"Length",
        b"Filter",
        b"DecodeParms",
        b"Prev",
        b"Size",
    ] {
        trailer.remove(key);
    }
    Ok(Context {
        catalog,
        names,
        tree,
        pairs,
        associated,
        active,
        root,
        next_id: parser.next_id,
        previous,
        trailer,
        revisions: parser.xrefs.len(),
        budget: Budget {
            syntax: parser.budget,
            nodes: MAX_NODES - parser.nodes,
            objects: MAX_OBJECTS - parser.objects,
            entries: MAX_ENTRIES
                - parser
                    .xrefs
                    .iter()
                    .map(|xref| match xref {
                        Xrefs::Classic(entries) => entries.len(),
                        Xrefs::Stream { ranges, .. } => {
                            ranges.iter().map(|&(_, count)| count as usize).sum()
                        }
                    })
                    .sum::<usize>(),
            classic: 100_000
                - parser
                    .xrefs
                    .iter()
                    .map(|xref| match xref {
                        Xrefs::Classic(entries) => entries.len(),
                        _ => 0,
                    })
                    .sum::<usize>(),
        },
    })
}

fn serialize(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Integer(value) => write!(output, "{value}")?,
        Value::Reference((id, generation)) => write!(output, "{id} {generation} R")?,
        Value::Name(name) => {
            output.push(b'/');
            for &byte in name {
                if byte.is_ascii_graphic() && !delimiter(byte) && byte != b'#' {
                    output.push(byte);
                } else {
                    write!(output, "#{byte:02X}")?;
                }
            }
        }
        Value::Bytes(bytes) => {
            output.push(b'<');
            for byte in bytes {
                write!(output, "{byte:02X}")?;
            }
            output.push(b'>');
        }
        Value::Scalar(bytes) => output.extend_from_slice(bytes),
        Value::Null => output.extend_from_slice(b"null"),
        Value::Array(values) => {
            output.push(b'[');
            for value in values {
                serialize(value, output)?;
                output.push(b' ');
            }
            output.push(b']');
        }
        Value::Dictionary(dictionary) => {
            output.extend_from_slice(b"<<");
            for (name, value) in dictionary {
                output.push(b' ');
                serialize(&Value::Name(name.clone()), output)?;
                output.push(b' ');
                serialize(value, output)?;
            }
            output.extend_from_slice(b" >>");
        }
    }
    if output.len() > MAX_SYNTAX_BYTES as usize {
        return Err(invalid("PDF write syntax size limit exceeded"));
    }
    Ok(())
}

enum Piece {
    Syntax(Vec<u8>),
    Payload(SegmentKind),
}

fn unloaded(segment: &Segment) -> Segment {
    Segment {
        ranges: segment.ranges.clone(),
        kind: segment.kind,
        path: segment.path.clone(),
        data: crate::segment::LazyData::NotLoaded,
        metadata: segment.metadata.clone(),
    }
}

pub(super) struct Plan {
    pieces: Vec<Piece>,
    pub structure: Structure,
    position: u64,
    objects: BTreeMap<Reference, u64>,
    next_id: u32,
    syntax_bytes: u64,
    syntax_nodes: usize,
}

fn nodes(value: &Value) -> usize {
    1 + match value {
        Value::Array(values) => values.iter().map(nodes).sum(),
        Value::Dictionary(dict) => dict.values().map(|value| 1 + nodes(value)).sum(),
        _ => 0,
    }
}

fn add_u64(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| invalid("PDF output offset overflow"))
}

impl Plan {
    fn syntax(&mut self, bytes: Vec<u8>) -> Result<()> {
        self.syntax_bytes = add_u64(self.syntax_bytes, bytes.len() as u64)?;
        self.position = add_u64(self.position, bytes.len() as u64)?;
        self.pieces.push(Piece::Syntax(bytes));
        Ok(())
    }

    fn allocate(&mut self) -> Result<Reference> {
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| invalid("PDF object number overflow"))?;
        Ok((id, 0))
    }

    fn object(&mut self, id: Reference, value: &Value) -> Result<()> {
        self.syntax_nodes += nodes(value);
        self.objects.insert(id, self.position);
        let mut bytes = format!("{} {} obj\n", id.0, id.1).into_bytes();
        serialize(value, &mut bytes)?;
        bytes.extend_from_slice(b"\nendobj\n");
        self.syntax(bytes)
    }

    fn stream(&mut self, kind: SegmentKind, data: &[u8]) -> Result<(Reference, ByteRange)> {
        if data.len() as u64 > crate::segment::MAX_SEGMENT_SIZE {
            return Err(invalid("metadata exceeds 256 MiB limit"));
        }
        let id = self.allocate()?;
        self.syntax_nodes += 7;
        self.objects.insert(id, self.position);
        let entries = if kind == SegmentKind::Xmp {
            "/Type /Metadata /Subtype /XML"
        } else {
            "/Type /EmbeddedFile /Subtype /application#2Fc2pa"
        };
        self.syntax(
            format!(
                "{} 0 obj\n<< {entries} /Length {} >>\nstream\n",
                id.0,
                data.len()
            )
            .into_bytes(),
        )?;
        let range = ByteRange::new(self.position, data.len() as u64);
        self.position = add_u64(self.position, range.size)?;
        self.pieces.push(Piece::Payload(kind));
        self.syntax(b"\nendstream\nendobj\n".to_vec())?;
        Ok((id, range))
    }
}

pub(super) fn plan(source: &Structure, updates: &Updates) -> Result<Plan> {
    if updates.processing.effective_chunk_size() == 0 {
        return Err(invalid("PDF processing chunk size must be nonzero"));
    }
    let state = source
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get::<State>())
        .ok_or_else(|| invalid("missing PDF editing context"))?;
    let noop = matches!(updates.xmp, MetadataUpdate::Keep)
        && matches!(updates.jumbf, MetadataUpdate::Keep);
    let context = match state {
        State::Ready(context) => Some(context),
        State::Blocked(reason) if !noop => return Err(unsupported(reason)),
        State::Invalid(message) if !noop => return Err(Error::InvalidFormat(message.clone())),
        State::Blocked(_) | State::Invalid(_) => None,
    };
    let mut result = Plan {
        pieces: Vec::new(),
        structure: Structure::new(ContainerKind::Pdf, MediaType::Pdf),
        position: source.total_size,
        objects: BTreeMap::new(),
        next_id: context.map_or(0, |context| context.next_id),
        syntax_bytes: 0,
        syntax_nodes: 0,
    };
    if noop {
        for segment in &source.segments {
            result.structure.add_segment(unloaded(segment));
        }
        result.structure.total_size = source.total_size;
        result.structure.metadata = source.metadata.clone();
        return Ok(result);
    }
    let mut context = context
        .ok_or_else(|| invalid("missing editable PDF context"))?
        .as_ref()
        .clone();
    if context.revisions >= MAX_REVISIONS {
        return Err(invalid("revision limit exceeded"));
    }
    // Classic xref offsets are ten decimal digits.
    if source.total_size >= 10_000_000_000 {
        return Err(unsupported(
            "PDF output exceeds classic cross-reference offset range",
        ));
    }
    result.syntax(b"\n".to_vec())?;
    let mut active_ranges = Vec::new();
    for segment in &source.segments {
        let keep = match segment.kind {
            SegmentKind::Xmp => matches!(updates.xmp, MetadataUpdate::Keep),
            SegmentKind::Jumbf => matches!(updates.jumbf, MetadataUpdate::Keep),
            _ => false,
        };
        if keep {
            active_ranges.push(unloaded(segment));
        }
    }
    match &updates.xmp {
        MetadataUpdate::Keep => {}
        MetadataUpdate::Remove => {
            context.catalog.remove(b"Metadata".as_slice());
        }
        MetadataUpdate::Set(data) => {
            let (id, range) = result.stream(SegmentKind::Xmp, data)?;
            context
                .catalog
                .insert(b"Metadata".to_vec(), Value::Reference(id));
            active_ranges.push(Segment::new(
                range.offset,
                range.size,
                SegmentKind::Xmp,
                Some("PDF/xmp".into()),
            ));
        }
    }
    if !matches!(updates.jumbf, MetadataUpdate::Keep) {
        context
            .associated
            .retain(|value| !matches!(value, Value::Reference(id) if context.active.contains(id)));
        context.pairs.retain(
            |_, value| !matches!(value, Value::Reference(id) if context.active.contains(id)),
        );
        context.active.clear();
        if let MetadataUpdate::Set(data) = &updates.jumbf {
            let (stream, range) = result.stream(SegmentKind::Jumbf, data)?;
            let spec = result.allocate()?;
            let file_name = b"Content Credentials".to_vec();
            if context.pairs.contains_key(&file_name) {
                return Err(unsupported(
                    "PDF attachment name Content Credentials is already in use",
                ));
            }
            let dictionary = Dictionary::from([
                (b"Type".to_vec(), Value::Name(b"Filespec".to_vec())),
                (b"F".to_vec(), Value::Bytes(b"manifest.c2pa".to_vec())),
                (b"UF".to_vec(), Value::Bytes(b"manifest.c2pa".to_vec())),
                (
                    b"Subtype".to_vec(),
                    Value::Name(b"application/c2pa".to_vec()),
                ),
                (
                    b"AFRelationship".to_vec(),
                    Value::Name(b"C2PA_Manifest".to_vec()),
                ),
                (
                    b"EF".to_vec(),
                    Value::Dictionary(Dictionary::from([(
                        b"F".to_vec(),
                        Value::Reference(stream),
                    )])),
                ),
            ]);
            result.object(spec, &Value::Dictionary(dictionary))?;
            context.associated.push(Value::Reference(spec));
            context.active.insert(spec);
            context.pairs.insert(file_name, Value::Reference(spec));
            if !context.catalog.get(b"Version".as_slice()).is_some_and(
                |value| matches!(value, Value::Name(version) if version.as_slice() >= b"1.7"),
            ) {
                context
                    .catalog
                    .insert(b"Version".to_vec(), Value::Name(b"1.7".to_vec()));
            }
            active_ranges.push(Segment::new(
                range.offset,
                range.size,
                SegmentKind::Jumbf,
                Some("PDF/jumbf".into()),
            ));
        }
        if context.associated.is_empty() {
            context.catalog.remove(b"AF".as_slice());
        } else {
            context
                .catalog
                .insert(b"AF".to_vec(), Value::Array(context.associated.clone()));
        }
        let names = context
            .pairs
            .iter()
            .flat_map(|(name, value)| [Value::Bytes(name.clone()), value.clone()])
            .collect();
        context.tree.insert(b"Names".to_vec(), Value::Array(names));
        let tree_id = result.allocate()?;
        result.object(tree_id, &Value::Dictionary(context.tree.clone()))?;
        let mut names = context.names.clone();
        names.insert(b"EmbeddedFiles".to_vec(), Value::Reference(tree_id));
        let names_id = result.allocate()?;
        result.object(names_id, &Value::Dictionary(names))?;
        context
            .catalog
            .insert(b"Names".to_vec(), Value::Reference(names_id));
    }
    result.object(context.root, &Value::Dictionary(context.catalog.clone()))?;
    let xref = result.position;
    let mut bytes = b"xref\n".to_vec();
    for (&(id, generation), &offset) in &result.objects {
        if offset >= 10_000_000_000 {
            return Err(unsupported(
                "PDF output exceeds classic cross-reference offset range",
            ));
        }
        writeln!(bytes, "{id} 1\n{offset:010} {generation:05} n ")?;
    }
    let mut trailer = context.trailer.clone();
    trailer.insert(b"Root".to_vec(), Value::Reference(context.root));
    trailer.insert(b"Size".to_vec(), Value::Integer(u64::from(result.next_id)));
    trailer.insert(b"Prev".to_vec(), Value::Integer(context.previous));
    bytes.extend_from_slice(b"trailer\n");
    let trailer = Value::Dictionary(trailer);
    result.syntax_nodes += nodes(&trailer);
    serialize(&trailer, &mut bytes)?;
    write!(bytes, "\nstartxref\n{xref}\n%%EOF\n")?;
    result.syntax(bytes)?;
    // Reserve conservatively for a reparse: numeric lookahead may consume a
    // syntax token up to three times, and old revisions still consume budgets.
    let budget = &mut context.budget;
    budget.syntax = budget
        .syntax
        .checked_sub(result.syntax_bytes.saturating_mul(3))
        .ok_or_else(|| invalid("incremental revision exceeds syntax work budget"))?;
    budget.nodes = budget
        .nodes
        .checked_sub(result.syntax_nodes)
        .ok_or_else(|| invalid("incremental revision exceeds syntax node budget"))?;
    budget.objects = budget
        .objects
        .checked_sub(result.objects.len())
        .ok_or_else(|| invalid("incremental revision exceeds object resolution budget"))?;
    budget.entries = budget
        .entries
        .checked_sub(result.objects.len())
        .ok_or_else(|| invalid("incremental revision exceeds cross-reference entry budget"))?;
    budget.classic = budget
        .classic
        .checked_sub(result.objects.len())
        .ok_or_else(|| {
            invalid("incremental revision exceeds classic cross-reference entry budget")
        })?;
    active_ranges.sort_by_key(|segment| segment.location().offset);
    result
        .structure
        .add_segment(Segment::new(0, 9, SegmentKind::Header, Some("PDF".into())));
    let mut end = 9;
    for segment in active_ranges {
        let range = segment.location();
        if range.offset < end {
            return Err(invalid("overlapping output metadata"));
        }
        if range.offset > end {
            result.structure.add_segment(Segment::new(
                end,
                range.offset - end,
                if end == 0 {
                    SegmentKind::Header
                } else {
                    SegmentKind::Other
                },
                None,
            ));
        }
        result.structure.add_segment(segment);
        end = range.end_offset();
    }
    if end < result.position {
        result.structure.add_segment(Segment::new(
            end,
            result.position - end,
            if end == 0 {
                SegmentKind::Header
            } else {
                SegmentKind::Other
            },
            None,
        ));
    }
    result.structure.total_size = result.position;
    context.next_id = result.next_id;
    context.previous = xref;
    context.revisions += 1;
    result.structure.metadata = Some(crate::SegmentMetadata::new(State::Ready(Arc::new(context))));
    Ok(result)
}

fn payload(updates: &Updates, kind: SegmentKind) -> Result<&[u8]> {
    match if kind == SegmentKind::Xmp {
        &updates.xmp
    } else {
        &updates.jumbf
    } {
        MetadataUpdate::Set(data) => Ok(data),
        _ => Err(invalid("write plan payload missing")),
    }
}

async fn emit(
    pw: &mut PDst<'_>,
    planned: &Structure,
    updates: &Updates,
    offset: &mut u64,
    index: &mut usize,
    began: &mut bool,
    mut bytes: &[u8],
) -> Result<()> {
    while !bytes.is_empty() {
        let segment = planned
            .segments
            .get(*index)
            .ok_or_else(|| invalid("write exceeds planned PDF segments"))?;
        if !*began {
            pw.begin_segment(segment).await?;
            *began = true;
        }
        let range = segment.location();
        if *offset == range.end_offset() {
            *index += 1;
            *began = false;
            continue;
        }
        let count = bytes
            .len()
            .min(usize::try_from(range.end_offset() - *offset).unwrap_or(usize::MAX));
        pw.set_exclude_mode(updates.processing.exclude_segments.contains(&segment.kind));
        pw.write_all(&bytes[..count]).await?;
        *offset += count as u64;
        bytes = &bytes[count..];
    }
    Ok(())
}

pub(super) async fn write_async(
    source_structure: &Structure,
    src: &mut Src,
    pw: &mut PDst<'_>,
    updates: &Updates,
) -> Result<()> {
    let plan = plan(source_structure, updates)?;
    if src.len() != source_structure.total_size {
        return Err(invalid("source size changed since parsing"));
    }
    let mut offset = 0u64;
    let mut index = 0usize;
    let mut began = false;
    src.seek(SeekFrom::Start(0))?;
    let mut buffer = [0u8; crate::segment::DEFAULT_CHUNK_SIZE];
    let chunk_size = updates.processing.effective_chunk_size().min(buffer.len());
    let mut remaining = source_structure.total_size;
    while remaining != 0 {
        let count = remaining.min(chunk_size as u64) as usize;
        src.read_exact_direct(&mut buffer[..count]).await?;
        emit(
            pw,
            &plan.structure,
            updates,
            &mut offset,
            &mut index,
            &mut began,
            &buffer[..count],
        )
        .await?;
        remaining -= count as u64;
    }
    for piece in plan.pieces {
        let bytes = match &piece {
            Piece::Syntax(bytes) => bytes.as_slice(),
            Piece::Payload(kind) => payload(updates, *kind)?,
        };
        for chunk in bytes.chunks(chunk_size) {
            emit(
                pw,
                &plan.structure,
                updates,
                &mut offset,
                &mut index,
                &mut began,
                chunk,
            )
            .await?;
        }
    }
    if offset != plan.position {
        return Err(invalid("PDF output layout mismatch"));
    }
    Ok(())
}

/// Plan an exact-length in-place payload replacement: returns `(offset, bytes)` to overwrite.
pub(crate) fn patch_plan(
    structure: &Structure,
    kind: SegmentKind,
    data: Vec<u8>,
) -> Result<(u64, Vec<u8>)> {
    let state = structure
        .metadata
        .as_ref()
        .and_then(|value| value.get::<State>())
        .ok_or_else(|| invalid("missing PDF editing context"))?;
    if let State::Blocked(reason) = state {
        return Err(unsupported(reason));
    }
    if let State::Invalid(message) = state {
        return Err(Error::InvalidFormat(message.clone()));
    }
    let index = match kind {
        SegmentKind::Jumbf => structure.c2pa_jumbf_index(),
        SegmentKind::Xmp => structure.xmp_index(),
        _ => {
            return Err(unsupported(
                "PDF in-place updates support only XMP and C2PA",
            ))
        }
    }
    .ok_or_else(|| invalid("missing PDF metadata segment"))?;
    let segment = &structure.segments[index];
    if segment.metadata.is_some() {
        return Err(unsupported("cannot patch compressed PDF metadata in place"));
    }
    let range = segment.location();
    if range.size != data.len() as u64 {
        return Err(invalid(
            "PDF in-place updates require exact-length payloads",
        ));
    }
    Ok((range.offset, data))
}

// Replacing the path only after a successful write also permits source == destination,
// including hard links, without truncating the source while it is being streamed.
pub(crate) fn write_path<T>(
    path: &std::path::Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<T>,
) -> Result<T> {
    use std::fs::{self, OpenOptions};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    struct Pending {
        path: std::path::PathBuf,
        active: bool,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            if !self.active {
                return;
            }
            if let Err(error) = fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(
                        "Could not remove PDF temporary output {}: {error}",
                        self.path.display()
                    );
                }
            }
        }
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    for _ in 0..32 {
        let temporary = parent.join(format!(
            ".asset-io-pdf-{}-{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let mut pending = Pending {
            path: temporary,
            active: true,
        };
        let result = (|| {
            let result = write(&mut file)?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            file.sync_all()?;
            Ok::<_, Error>(result)
        })();
        drop(file);
        let result = result?;
        fs::rename(&pending.path, path)?;
        pending.active = false;
        return Ok(result);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve PDF temporary output",
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializer_preserves_all_value_types_and_decodes_pdf_strings() {
        let bytes = b"<< /A [(a\\n\\r\\t\\b\\f\\(\\)\\\\\\053\\777\\\r\nb\r\nc) <a> <00FF> /a#2Fb +10 -.125 .5 true false null 1 2 R] >>";
        let mut parser = Parser::memory(bytes);
        let original = super::super::test_block_on(parser.value(0)).unwrap();
        let Value::Dictionary(dictionary) = &original else {
            panic!()
        };
        let Value::Array(array) = &dictionary[b"A".as_slice()] else {
            panic!()
        };
        assert_eq!(
            array[0],
            Value::Bytes(b"a\n\r\t\x08\x0c()\\+\xffb\nc".to_vec())
        );
        assert_eq!(array[1], Value::Bytes(vec![0xa0]));
        let mut encoded = Vec::new();
        serialize(&original, &mut encoded).unwrap();
        let mut parser = Parser::memory(&encoded);
        let reparsed = super::super::test_block_on(parser.value(0)).unwrap();
        assert_eq!(original, reparsed);
    }

    #[test]
    fn failed_path_write_preserves_destination_and_removes_partial_temporary_output() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("output.pdf");
        std::fs::write(&destination, b"original").unwrap();
        let result: Result<()> = write_path(&destination, |file| {
            file.write_all(b"partial")?;
            Err(Error::UserCanceled)
        });
        assert!(matches!(result, Err(Error::UserCanceled)));
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
