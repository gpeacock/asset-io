// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

// Unless required by applicable law or agreed to in writing,
// this software is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR REPRESENTATIONS OF ANY KIND, either express or
// implied. See the LICENSE-MIT and LICENSE-APACHE files for the
// specific language governing permissions and limitations under
// each license.

//! Incremental metadata revisions: the original bytes are never rewritten; a revision
//! is appended that points the catalog at new metadata streams.
//!
//! What a revision needs to know is read while parsing (the catalog, the attachment
//! name tree, the trailer, and how much of the parse budgets is left) and kept in the
//! structure's attachment as a [`Context`]. Planning is then a pure function of that
//! and the update, which is what lets it be a [`WritePlan`](crate::WritePlan).

use super::lexer::{
    delimiter, invalid, required, Dictionary, Reference, Value, MAX_DEPTH, MAX_NODES,
    MAX_SYNTAX_BYTES,
};
use super::{Parser, Xrefs, MAX_CLASSIC_ENTRIES, MAX_ENTRIES, MAX_OBJECTS, MAX_REVISIONS};
use crate::{BoxFuture, Error, MetadataUpdate, Result, SegmentKind, Updates, MAX_SEGMENT_SIZE};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::Arc;

/// Whether the document can be edited, and if so what an edit needs.
#[derive(Clone, Debug)]
pub(super) enum State {
    Ready(Arc<Context>),
    /// A feature this writer does not support (the reason).
    Blocked(&'static str),
    /// The editing structures are malformed: reading is fine, writing is not.
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

/// What is left of the parse budgets, so that a revision can be reserved for.
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
        Err(Error::InvalidFormat(message)) => Ok(State::Invalid(message)),
        Err(error) => Err(error),
    }
}

async fn dictionary(parser: &mut Parser<'_>, value: Option<Value>) -> Result<Dictionary> {
    match value {
        None | Some(Value::Null) => Ok(Dictionary::new()),
        Some(value) => parser.resolve(value).await?.dictionary(),
    }
}

/// Reject forms a metadata edit could invalidate: XFA, and populated signature
/// fields. (Ordinary unsigned AcroForms are fine.)
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

/// Boxed because a field's kids are fields (the recursion is bounded by [`MAX_DEPTH`]).
fn check_fields<'a, 'io>(
    parser: &'a mut Parser<'io>,
    fields: Value,
    inherited_type: Option<Value>,
    inherited_value: Option<Value>,
    depth: usize,
    remaining: &'a mut usize,
) -> BoxFuture<'a, Result<()>> {
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
            let field_type = own_type.or_else(|| inherited_type.clone());
            if field_type
                .as_ref()
                .is_some_and(|value| !matches!(value, Value::Name(_)))
            {
                return Err(invalid("AcroForm FT must be a name"));
            }
            let own_value = match field.remove(b"V".as_slice()) {
                Some(value) => Some(parser.resolve(value).await?),
                None => None,
            };
            let value = own_value.or_else(|| inherited_value.clone());
            if field_type.as_ref().is_some_and(|t| t.is_name(b"Sig"))
                && value
                    .as_ref()
                    .is_some_and(|value| !matches!(value, Value::Null))
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
    let used_entries: usize = parser
        .xrefs
        .iter()
        .map(|xref| match xref {
            Xrefs::Classic(entries) => entries.len(),
            Xrefs::Stream { ranges, .. } => ranges.iter().map(|&(_, count)| count as usize).sum(),
        })
        .sum();
    let used_classic: usize = parser
        .xrefs
        .iter()
        .map(|xref| match xref {
            Xrefs::Classic(entries) => entries.len(),
            _ => 0,
        })
        .sum();
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
            entries: MAX_ENTRIES - used_entries,
            classic: MAX_CLASSIC_ENTRIES - used_classic,
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

/// One piece of the appended revision, in order.
pub(super) enum Piece {
    /// Bytes of syntax.
    Syntax(Vec<u8>),
    /// The payload of a new metadata stream: the update's data of this kind.
    Stream(SegmentKind),
}

/// The appended revision, and what to know afterwards.
pub(super) struct Revision {
    pub(super) pieces: Vec<Piece>,
    pub(super) context: Context,
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

/// Builds the pieces of a revision, tracking where each lands.
struct Appender {
    pieces: Vec<Piece>,
    position: u64,
    objects: BTreeMap<Reference, u64>,
    next_id: u32,
    syntax_bytes: u64,
    syntax_nodes: usize,
}

impl Appender {
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

    fn stream(&mut self, kind: SegmentKind, length: usize) -> Result<Reference> {
        if length as u64 > MAX_SEGMENT_SIZE {
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
            format!("{} 0 obj\n<< {entries} /Length {length} >>\nstream\n", id.0).into_bytes(),
        )?;
        self.position = add_u64(self.position, length as u64)?;
        self.pieces.push(Piece::Stream(kind));
        self.syntax(b"\nendstream\nendobj\n".to_vec())?;
        Ok(id)
    }
}

/// Plan the revision that applies `updates` to a document `source_size` bytes long.
pub(super) fn revision(context: &Context, source_size: u64, updates: &Updates) -> Result<Revision> {
    let mut result = Appender {
        pieces: Vec::new(),
        position: source_size,
        objects: BTreeMap::new(),
        next_id: context.next_id,
        syntax_bytes: 0,
        syntax_nodes: 0,
    };
    let mut context = context.clone();
    if context.revisions >= MAX_REVISIONS {
        return Err(invalid("revision limit exceeded"));
    }
    // Classic xref offsets are ten decimal digits.
    if source_size >= 10_000_000_000 {
        return Err(unsupported(
            "PDF output exceeds classic cross-reference offset range",
        ));
    }
    result.syntax(b"\n".to_vec())?;
    match updates.xmp() {
        MetadataUpdate::Keep => {}
        MetadataUpdate::Remove => {
            context.catalog.remove(b"Metadata".as_slice());
        }
        MetadataUpdate::Set(data) => {
            let id = result.stream(SegmentKind::Xmp, data.len())?;
            context
                .catalog
                .insert(b"Metadata".to_vec(), Value::Reference(id));
        }
    }
    if !matches!(updates.jumbf(), MetadataUpdate::Keep) {
        context
            .associated
            .retain(|value| !matches!(value, Value::Reference(id) if context.active.contains(id)));
        context.pairs.retain(
            |_, value| !matches!(value, Value::Reference(id) if context.active.contains(id)),
        );
        context.active.clear();
        if let MetadataUpdate::Set(data) = updates.jumbf() {
            let stream = result.stream(SegmentKind::Jumbf, data.len())?;
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
    context.next_id = result.next_id;
    context.previous = xref;
    context.revisions += 1;
    Ok(Revision {
        pieces: result.pieces,
        context,
    })
}

impl State {
    /// Why an edit is refused, if it is.
    pub(super) fn refusal(&self) -> Option<Error> {
        match self {
            State::Ready(_) => None,
            State::Blocked(reason) => Some(unsupported(reason)),
            State::Invalid(message) => Some(Error::InvalidFormat(message.clone())),
        }
    }
}
