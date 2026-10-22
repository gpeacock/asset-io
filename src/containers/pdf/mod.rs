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

//! PDF on the structure model: bounded discovery of the catalog's XMP and C2PA
//! stores, without loading the document.
//!
//! This is not a general PDF parser, renderer, sanitizer or C2PA validator. It follows
//! classic tables, cross-reference streams and `/Prev` chains from the final
//! `startxref`, and resolves the current catalog's `/Metadata` and document-level
//! `/AF` C2PA association (compressed object streams are decoded on demand). A stream
//! is a segment holding its payload as it is in the file; the segments between are
//! [`Other`](crate::SegmentKind::Other) bytes, so the structure tiles the file and a
//! hash can skip exactly a store.
//!
//! A Flate-compressed store is an entity whose bytes are the encoded ones;
//! [`Structure::xmp`](crate::Structure::xmp) and
//! [`Structure::store`](crate::Structure::store) decode them (through the handler's
//! [`decode_entity`](ContainerHandler::decode_entity)). See `PDF.md` for the rules and
//! the limits.

mod flate;
mod lexer;
mod write;

use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, FormatSpec};
use crate::plan::WritePlan;
use crate::plan_builder::PlanBuilder;
use crate::range::ByteRange;
use crate::segment::Role;
use crate::segment::{Name, SegmentKind, MAX_SEGMENT_SIZE};
use crate::structure::{Entity, SegmentId, Structure, StructureBuilder};
use crate::task::{BoxFuture, Io};
use crate::{MetadataUpdate, Updates};
use flate::{inflate, Decode};
use lexer::{
    invalid, required, unsigned, whitespace, Dictionary, Lex, Lexer, Reference, Stop, Value,
    MAX_DEPTH, MAX_SYNTAX_BYTES,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const MAX_ENTRIES: usize = 1_000_000;
const MAX_CLASSIC_ENTRIES: usize = 100_000;
const MAX_REVISIONS: usize = 128;
const MAX_OBJECTS: usize = 4096;
const MAX_STRUCTURAL_STREAM: usize = 8 * 1024 * 1024;
const MAX_DECODE_WORK: usize = 32 * 1024 * 1024;
const MAX_XREF_STORAGE: usize = 16 * 1024 * 1024;
const MAX_DECODED_METADATA: usize = 64 * 1024 * 1024;
const MAX_OBJECT_STREAM_ENTRIES: u64 = 100_000;
/// The final `startxref` and `%%EOF` must be this close to the end.
const TAIL: u64 = 1024;
/// `%PDF-1.x` and a line end.
const HEADER_LEN: u64 = 9;

/// The PDF container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct PdfHandler;

/// The PDF handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(PdfHandler)
}

/// The PDF container's [`ContainerHandler::id`].
pub const ID: &str = "pdf";

/// What a structure keeps for this handler.
#[derive(Clone, Debug)]
struct PdfState {
    /// How each compressed metadata stream is encoded, by segment index.
    decodes: BTreeMap<usize, Decode>,
    /// Whether the document can be edited, and what an edit needs to know.
    edit: write::State,
}

impl PdfState {
    fn of(structure: &Structure) -> Result<&PdfState> {
        structure
            .attachment::<PdfState>()
            .ok_or_else(|| invalid("structure was not parsed by the PDF handler"))
    }

    fn decode_of(structure: &Structure, entity: &Entity) -> Result<Option<Decode>> {
        let part = entity
            .parts()
            .next()
            .ok_or_else(|| invalid("entity has no parts"))?;
        Ok(Self::of(structure)?
            .decodes
            .get(&part.segment().index())
            .copied())
    }
}

impl ContainerHandler for PdfHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[FormatSpec::new("application/pdf", "pdf")];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.len() >= HEADER_LEN as usize
            && &header[..5] == b"%PDF-"
            && ((header[5] == b'1' && header[6] == b'.' && matches!(header[7], b'0'..=b'7'))
                || &header[5..8] == b"2.0")
            && matches!(header[8], b'\r' | b'\n')
    }

    fn parse_async(self: Arc<Self>, io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(parse_with(self, io))
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        src_structure: &'f Structure,
        io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move { plan(src_structure, io.len(), updates) })
    }

    fn check_patch(&self, structure: &Structure, entity: &Entity, len: usize) -> Result<()> {
        let state = PdfState::of(structure)?;
        if let Some(refusal) = state.edit.refusal() {
            return Err(refusal);
        }
        if PdfState::decode_of(structure, entity)?.is_some() {
            return Err(Error::UnsupportedFeature(
                "cannot patch compressed PDF metadata in place",
            ));
        }
        if len as u64 != entity.len() {
            return Err(invalid(
                "PDF in-place updates require exact-length payloads",
            ));
        }
        Ok(())
    }

    fn decode_entity(
        &self,
        structure: &Structure,
        entity: &Entity,
        stored: Vec<u8>,
    ) -> Result<Vec<u8>> {
        match PdfState::decode_of(structure, entity)? {
            None => Ok(stored),
            Some(decode) => {
                let data = inflate(&stored, MAX_DECODED_METADATA, None)?;
                decode.undo_predictor(data)
            }
        }
    }
}

/// Parse a PDF into a validated structure.
pub fn parse<R: std::io::Read + std::io::Seek>(src: &mut R) -> Result<Structure> {
    handler().parse(src)
}

// ---------------------------------------------------------------------------
// Cross-reference data
// ---------------------------------------------------------------------------

struct Object {
    value: Value,
    /// Where the stream data starts, if the object is a stream.
    stream: Option<u64>,
}

#[derive(Clone, Copy)]
enum Entry {
    Free,
    Normal { offset: u64, generation: u16 },
    Compressed { container: u32, index: u32 },
}

enum Xrefs {
    Classic(BTreeMap<u32, Entry>),
    Stream {
        ranges: Vec<(u32, u32)>,
        widths: [usize; 3],
        data: Vec<u8>,
    },
}

impl Xrefs {
    fn entry(&self, id: u32) -> Result<Option<Entry>> {
        match self {
            Self::Classic(entries) => Ok(entries.get(&id).copied()),
            Self::Stream {
                ranges,
                widths,
                data,
            } => {
                let mut index = 0usize;
                for &(start, count) in ranges {
                    if id >= start && id - start < count {
                        index += (id - start) as usize;
                        let row = &data[index * widths.iter().sum::<usize>()..];
                        let mut position = 0;
                        let mut fields = [0u64; 3];
                        for (i, &width) in widths.iter().enumerate() {
                            fields[i] = if i == 0 && width == 0 { 1 } else { 0 };
                            for &byte in &row[position..position + width] {
                                fields[i] = (fields[i] << 8) | u64::from(byte);
                            }
                            position += width;
                        }
                        return Ok(Some(match fields[0] {
                            0 => Entry::Free,
                            1 => Entry::Normal {
                                offset: fields[1],
                                generation: u16::try_from(fields[2])
                                    .map_err(|_| invalid("generation overflow"))?,
                            },
                            2 => Entry::Compressed {
                                container: u32::try_from(fields[1])
                                    .map_err(|_| invalid("object stream number overflow"))?,
                                index: u32::try_from(fields[2])
                                    .map_err(|_| invalid("object stream index overflow"))?,
                            },
                            _ => return Err(invalid("unknown cross-reference entry type")),
                        }));
                    }
                    index += count as usize;
                }
                Ok(None)
            }
        }
    }
}

struct ObjectStream {
    id: u32,
    data: Vec<u8>,
    objects: Vec<(u32, usize)>,
    first: usize,
}

/// What the catalog's metadata entries say.
struct CatalogMetadata {
    catalog: Dictionary,
    associated: Vec<Value>,
    active: Option<BTreeSet<Reference>>,
    ranges: Vec<(ByteRange, SegmentKind, Option<Decode>)>,
}

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

/// Reads objects through an [`Io`], a window at a time. The syntax itself is parsed by
/// [`Lexer`] over bytes in memory; this holds the budgets and the cross-reference
/// chain and decides which bytes to read.
struct Parser<'io> {
    io: &'io mut Io,
    size: u64,
    budget: u64,
    nodes: usize,
    objects: usize,
    xrefs: Vec<Xrefs>,
    decoding_left: usize,
    resolving: BTreeSet<Reference>,
    object_stream: Option<ObjectStream>,
    trailer: Dictionary,
    next_id: u32,
}

impl<'io> Parser<'io> {
    fn new(io: &'io mut Io, size: u64) -> Self {
        Self {
            io,
            size,
            budget: MAX_SYNTAX_BYTES,
            nodes: 0,
            objects: 0,
            xrefs: Vec::new(),
            decoding_left: MAX_DECODE_WORK,
            resolving: BTreeSet::new(),
            object_stream: None,
            trailer: Dictionary::new(),
            next_id: 0,
        }
    }

    /// Run `f` over the file from `offset`, with a window that grows until `f` has all
    /// the bytes it needs. `f` may run more than once, so it must not keep state.
    async fn lex<T>(&mut self, offset: u64, f: impl Fn(&mut Lexer<'_>) -> Lex<T>) -> Result<T> {
        if offset > self.size {
            return Err(invalid("offset outside file"));
        }
        let remaining = self.size - offset;
        let mut want = 4096u64;
        loop {
            let len = remaining.min(want);
            let data = self.io.read_vec(offset, len as usize).await?;
            let mut lexer = Lexer::new(&data, offset, len == remaining, self.budget, self.nodes);
            match f(&mut lexer) {
                Ok(value) => {
                    self.budget = lexer.budget;
                    self.nodes = lexer.nodes;
                    return Ok(value);
                }
                Err(Stop::Fail(e)) => return Err(e),
                Err(Stop::More) => {
                    if want >= MAX_SYNTAX_BYTES {
                        return Err(invalid("syntax work limit exceeded"));
                    }
                    want = (want * 4).min(MAX_SYNTAX_BYTES);
                }
            }
        }
    }

    /// Read the cross-reference chain from `offset`, returning the document's root.
    async fn xrefs(&mut self, mut offset: u64) -> Result<Reference> {
        let mut root = None;
        let mut entry_count = 0usize;
        let mut classic_count = 0usize;
        let mut storage = 0usize;
        for _ in 0..MAX_REVISIONS {
            let file_size = self.size;
            let (entries_so_far, classic_so_far) = (entry_count, classic_count);
            let classic = self
                .lex(offset, |lx| {
                    if lx.token()? != b"xref" {
                        return Ok(None);
                    }
                    let (mut entry_count, mut classic_count) = (entries_so_far, classic_so_far);
                    let mut entries = BTreeMap::new();
                    loop {
                        lx.skip_space()?;
                        if lx.peek()? == Some(b't') {
                            break;
                        }
                        let first = lx.number()?;
                        let count = lx.number()?;
                        let count = usize::try_from(count)
                            .map_err(|_| Stop::Fail(invalid("xref count overflow")))?;
                        entry_count = entry_count
                            .checked_add(count)
                            .filter(|n| *n <= MAX_ENTRIES)
                            .ok_or_else(|| {
                                Stop::Fail(invalid("cross-reference entry limit exceeded"))
                            })?;
                        // Classic tables have much higher per-entry overhead than
                        // compact streams.
                        classic_count = classic_count
                            .checked_add(count)
                            .filter(|n| *n <= MAX_CLASSIC_ENTRIES)
                            .ok_or_else(|| {
                                Stop::Fail(invalid("classic cross-reference entry limit exceeded"))
                            })?;
                        let end = first
                            .checked_add(count as u64)
                            .filter(|end| *end <= u64::from(u32::MAX))
                            .ok_or_else(|| Stop::Fail(invalid("xref object number overflow")))?;
                        for id in first..end {
                            let object_offset = lx.number()?;
                            let generation = u16::try_from(lx.number()?)
                                .map_err(|_| Stop::Fail(invalid("generation overflow")))?;
                            let entry = match lx.token()?.as_slice() {
                                b"n" if object_offset < file_size => Entry::Normal {
                                    offset: object_offset,
                                    generation,
                                },
                                b"f" => Entry::Free,
                                _ => {
                                    return Err(Stop::Fail(invalid("invalid xref entry or offset")))
                                }
                            };
                            if entries.insert(id as u32, entry).is_some() {
                                return Err(Stop::Fail(invalid("overlapping xref subsections")));
                            }
                        }
                    }
                    lx.expect(b"trailer")?;
                    let highest = entries.last_key_value().map(|(&id, _)| id);
                    let trailer = lx.value(0)?.dictionary()?;
                    Ok(Some((
                        trailer,
                        Xrefs::Classic(entries),
                        highest,
                        entry_count,
                        classic_count,
                    )))
                })
                .await?;
            let (mut trailer, section, highest) = match classic {
                Some((trailer, section, highest, entries, classic_entries)) => {
                    entry_count = entries;
                    classic_count = classic_entries;
                    (trailer, section, highest)
                }
                None => {
                    let object = self.raw_object(offset, None).await?;
                    let mut dict = object.value.dictionary()?;
                    if !required(&mut dict, b"Type")?.is_name(b"XRef") {
                        return Err(invalid(
                            "startxref does not reference a cross-reference table",
                        ));
                    }
                    let size = required(&mut dict, b"Size")?.integer()?;
                    if size > u64::from(u32::MAX) {
                        return Err(invalid("invalid trailer Size"));
                    }
                    let Value::Array(w) = required(&mut dict, b"W")? else {
                        return Err(invalid("invalid xref widths"));
                    };
                    if w.len() != 3 {
                        return Err(invalid("xref requires three field widths"));
                    }
                    let mut widths = [0usize; 3];
                    for (target, value) in widths.iter_mut().zip(w) {
                        let width = value.integer()?;
                        if width > 8 {
                            return Err(invalid("xref field width exceeds 8 bytes"));
                        }
                        *target = width as usize;
                    }
                    let row_size: usize = widths.iter().sum();
                    if row_size == 0 {
                        return Err(invalid("empty xref rows"));
                    }
                    let indices = match dict.remove(b"Index".as_slice()) {
                        Some(Value::Array(indices)) => indices,
                        None => vec![Value::Integer(0), Value::Integer(size)],
                        _ => return Err(invalid("invalid xref Index")),
                    };
                    if indices.len() % 2 != 0 {
                        return Err(invalid("odd xref Index length"));
                    }
                    let mut ranges = Vec::new();
                    let mut end = 0;
                    let mut rows = 0usize;
                    let mut indices = indices.into_iter();
                    while let Some(first) = indices.next() {
                        let first = first.integer()?;
                        let count = indices
                            .next()
                            .ok_or_else(|| invalid("missing xref count"))?
                            .integer()?;
                        let next = first
                            .checked_add(count)
                            .filter(|n| *n <= size)
                            .ok_or_else(|| invalid("xref Index outside Size"))?;
                        if first < end {
                            return Err(invalid("unordered or overlapping xref Index"));
                        }
                        end = next;
                        rows = rows
                            .checked_add(
                                usize::try_from(count)
                                    .map_err(|_| invalid("xref count overflow"))?,
                            )
                            .filter(|n| *n <= MAX_ENTRIES)
                            .ok_or_else(|| invalid("cross-reference entry limit exceeded"))?;
                        ranges.push((first as u32, count as u32));
                    }
                    entry_count = entry_count
                        .checked_add(rows)
                        .filter(|n| *n <= MAX_ENTRIES)
                        .ok_or_else(|| invalid("cross-reference entry limit exceeded"))?;
                    let decoded_size = rows
                        .checked_mul(row_size)
                        .filter(|n| *n <= MAX_STRUCTURAL_STREAM)
                        .ok_or_else(|| invalid("xref decoded size limit exceeded"))?;
                    let decode = Decode::from_dictionary(&mut dict)?;
                    let expected = if let Some(config) = decode.filter(|c| c.predictor != 1) {
                        if decoded_size % config.columns as usize != 0 {
                            return Err(invalid("xref predictor row mismatch"));
                        }
                        decoded_size
                            .checked_add(decoded_size / config.columns as usize)
                            .ok_or_else(|| invalid("xref predictor size overflow"))?
                    } else {
                        decoded_size
                    };
                    let data = self
                        .structural_stream(object.stream, &mut dict, decode, Some(expected))
                        .await?;
                    if data.len() != decoded_size {
                        return Err(invalid("xref decoded length mismatch"));
                    }
                    storage = storage
                        .checked_add(data.len())
                        .filter(|n| *n <= MAX_XREF_STORAGE)
                        .ok_or_else(|| invalid("xref storage limit exceeded"))?;
                    dict.insert(b"Size".to_vec(), Value::Integer(size));
                    let highest = ranges
                        .last()
                        .and_then(|&(first, count)| count.checked_sub(1).map(|n| first + n));
                    (
                        dict,
                        Xrefs::Stream {
                            ranges,
                            widths,
                            data,
                        },
                        highest,
                    )
                }
            };
            let size = required(&mut trailer, b"Size")?.integer()?;
            if size > u64::from(u32::MAX) || highest.is_some_and(|id| u64::from(id) >= size) {
                return Err(invalid("invalid trailer Size"));
            }
            self.next_id = self.next_id.max(size as u32);
            if self.xrefs.is_empty() {
                self.trailer = trailer.clone();
            } else {
                for key in [b"Info".as_slice(), b"ID"] {
                    if let Some(value) = trailer.get(key) {
                        self.trailer
                            .entry(key.to_vec())
                            .or_insert_with(|| value.clone());
                    }
                }
            }
            if trailer
                .get(b"Encrypt".as_slice())
                .is_some_and(|v| !matches!(v, Value::Null))
            {
                return Err(Error::UnsupportedFeature("encrypted PDF"));
            }
            if trailer.contains_key(b"XRefStm".as_slice()) {
                return Err(Error::UnsupportedFeature(
                    "PDF hybrid cross-reference streams",
                ));
            }
            if let Some(value) = trailer.remove(b"Root".as_slice()) {
                let Value::Reference(reference) = value else {
                    return Err(invalid("Root must be indirect"));
                };
                if root.is_none() {
                    root = Some(reference);
                }
            }
            self.xrefs.push(section);
            match trailer.remove(b"Prev".as_slice()) {
                None => return root.ok_or_else(|| invalid("missing document Root")),
                Some(value) => {
                    let previous = value.integer()?;
                    if previous >= offset {
                        return Err(invalid("cyclic or forward Prev chain"));
                    }
                    offset = previous;
                }
            }
        }
        Err(invalid("revision limit exceeded"))
    }

    /// Resolve an indirect reference to its object. Boxed because an object stream is
    /// itself an object (the recursion is bounded by [`MAX_DEPTH`]).
    fn object(&mut self, reference: Reference) -> BoxFuture<'_, Result<Object>> {
        Box::pin(async move {
            self.objects += 1;
            if self.objects > MAX_OBJECTS {
                return Err(invalid("object resolution limit exceeded"));
            }
            if self.resolving.len() >= MAX_DEPTH || !self.resolving.insert(reference) {
                return Err(invalid("cyclic or excessively nested object resolution"));
            }
            let result = match self.entry(reference.0) {
                Ok(Entry::Normal { offset, generation }) if generation == reference.1 => {
                    self.raw_object(offset, Some(reference)).await
                }
                Ok(Entry::Compressed { container, index }) if reference.1 == 0 => {
                    self.compressed_object(reference.0, container, index).await
                }
                Ok(_) => Err(invalid("stale or free object reference")),
                Err(e) => Err(e),
            };
            self.resolving.remove(&reference);
            result
        })
    }

    fn entry(&self, id: u32) -> Result<Entry> {
        for section in &self.xrefs {
            if let Some(entry) = section.entry(id)? {
                return Ok(entry);
            }
        }
        Err(invalid("unresolved object"))
    }

    async fn raw_object(&mut self, offset: u64, reference: Option<Reference>) -> Result<Object> {
        self.lex(offset, |lx| {
            let number = lx.number()?;
            let generation = lx.number()?;
            if reference.is_some_and(|r| number != u64::from(r.0) || generation != u64::from(r.1)) {
                return Err(Stop::Fail(invalid("xref/object identity mismatch")));
            }
            lx.expect(b"obj")?;
            let value = lx.value(0)?;
            let ending = lx.token()?;
            let stream = if ending == b"stream" {
                if !matches!(value, Value::Dictionary(_)) {
                    return Err(Stop::Fail(invalid("stream without dictionary")));
                }
                match lx.byte()? {
                    b'\n' => {}
                    b'\r' => {
                        if lx.peek()? == Some(b'\n') {
                            lx.byte()?;
                        }
                    }
                    _ => {
                        return Err(Stop::Fail(invalid("stream keyword requires newline")));
                    }
                }
                Some(lx.offset())
            } else if ending == b"endobj" {
                None
            } else {
                return Err(Stop::Fail(invalid("missing endobj")));
            };
            Ok(Object { value, stream })
        })
        .await
    }

    /// Where the stream's data is, checked against the file and its `endstream`.
    async fn stream_range(
        &mut self,
        offset: Option<u64>,
        dict: &mut Dictionary,
        direct_length: bool,
    ) -> Result<ByteRange> {
        let offset = offset.ok_or_else(|| invalid("expected stream"))?;
        let value = required(dict, b"Length")?;
        let length = if direct_length {
            value.integer()?
        } else {
            self.resolve(value).await?.integer()?
        };
        let end = offset
            .checked_add(length)
            .filter(|end| *end < self.size)
            .ok_or_else(|| invalid("stream length outside file"))?;
        self.lex(end, |lx| {
            lx.expect(b"endstream")?;
            lx.expect(b"endobj")
        })
        .await?;
        ByteRange::new(offset, length)
    }

    /// A stream the parser itself needs (cross-reference or object stream), decoded
    /// within the structural budgets.
    async fn structural_stream(
        &mut self,
        offset: Option<u64>,
        dict: &mut Dictionary,
        decode: Option<Decode>,
        expected: Option<usize>,
    ) -> Result<Vec<u8>> {
        let range = self.stream_range(offset, dict, expected.is_some()).await?;
        let encoded =
            usize::try_from(range.len()).map_err(|_| invalid("stream length overflow"))?;
        if encoded > MAX_STRUCTURAL_STREAM || encoded > self.decoding_left {
            return Err(invalid(
                "encoded structural stream size/work limit exceeded",
            ));
        }
        self.decoding_left -= encoded;
        let limit = MAX_STRUCTURAL_STREAM.min(self.decoding_left);
        let limit = expected.map_or(limit, |size| size.min(limit));
        let raw = self.io.read_vec(range.offset(), encoded).await?;
        if let Some(config) = decode {
            let data = inflate(&raw, limit, expected)?;
            self.decoding_left -= data.len();
            config.undo_predictor(data)
        } else {
            if encoded > limit {
                return Err(invalid("decoded structural stream work limit exceeded"));
            }
            self.decoding_left -= raw.len();
            Ok(raw)
        }
    }

    async fn compressed_object(&mut self, id: u32, container: u32, index: u32) -> Result<Object> {
        if self
            .object_stream
            .as_ref()
            .is_none_or(|stream| stream.id != container)
        {
            if !matches!(self.entry(container)?, Entry::Normal { .. }) {
                return Err(invalid("object stream container is not an ordinary object"));
            }
            let object = self.object((container, 0)).await?;
            let mut dict = object.value.dictionary()?;
            if !required(&mut dict, b"Type")?.is_name(b"ObjStm") {
                return Err(invalid("compressed object container is not ObjStm"));
            }
            let count = required(&mut dict, b"N")?.integer()?;
            let first = required(&mut dict, b"First")?.integer()?;
            if count > MAX_OBJECT_STREAM_ENTRIES {
                return Err(invalid("object stream entry limit exceeded"));
            }
            let decode = Decode::from_dictionary(&mut dict)?;
            let data = self
                .structural_stream(object.stream, &mut dict, decode, None)
                .await?;
            let first = usize::try_from(first)
                .ok()
                .filter(|n| *n <= data.len())
                .ok_or_else(|| invalid("object stream First outside payload"))?;
            let mut lexer = Lexer::new(&data[..first], 0, true, self.budget, self.nodes);
            let mut objects = Vec::new();
            let mut ids = BTreeSet::new();
            let header = (|| -> Lex<()> {
                for _ in 0..count {
                    let object_id = u32::try_from(lexer.number()?)
                        .map_err(|_| Stop::Fail(invalid("object number overflow")))?;
                    let offset = usize::try_from(lexer.number()?)
                        .map_err(|_| Stop::Fail(invalid("object offset overflow")))?;
                    if offset >= data.len() - first
                        || objects
                            .last()
                            .is_some_and(|&(_, previous)| offset <= previous)
                        || !ids.insert(object_id)
                    {
                        return Err(Stop::Fail(invalid(
                            "invalid object stream offsets or duplicate objects",
                        )));
                    }
                    objects.push((object_id, offset));
                }
                lexer.skip_space()
            })();
            finish(header)?;
            if lexer.at != first {
                return Err(invalid("object stream header length mismatch"));
            }
            self.budget = lexer.budget;
            self.object_stream = Some(ObjectStream {
                id: container,
                data,
                objects,
                first,
            });
        }
        let stream = self
            .object_stream
            .as_ref()
            .ok_or_else(|| invalid("missing object stream"))?;
        let &(object_id, start) = stream
            .objects
            .get(index as usize)
            .ok_or_else(|| invalid("compressed object index outside stream"))?;
        if id != object_id {
            return Err(invalid("compressed object identity mismatch"));
        }
        let end = stream
            .objects
            .get(index as usize + 1)
            .map(|&(_, offset)| offset)
            .unwrap_or(stream.data.len() - stream.first);
        let slice = &stream.data[stream.first + start..stream.first + end];
        let mut lexer = Lexer::new(slice, 0, true, self.budget, self.nodes);
        let value = finish(lexer.value(0).and_then(|value| {
            lexer.skip_space()?;
            Ok(value)
        }))?;
        if lexer.at != slice.len() {
            return Err(invalid("trailing syntax in compressed object"));
        }
        self.budget = lexer.budget;
        self.nodes = lexer.nodes;
        Ok(Object {
            value,
            stream: None,
        })
    }

    async fn resolve(&mut self, mut value: Value) -> Result<Value> {
        let mut seen = BTreeSet::new();
        loop {
            let Value::Reference(reference) = value else {
                return Ok(value);
            };
            if seen.len() == MAX_DEPTH {
                return Err(invalid("indirect reference depth limit exceeded"));
            }
            if !seen.insert(reference) {
                return Err(invalid("cyclic indirect reference"));
            }
            let object = self.object(reference).await?;
            if object.stream.is_some() {
                return Err(invalid("unexpected stream reference"));
            }
            value = object.value;
        }
    }

    /// A metadata stream of the catalog: where its data is and how it is encoded.
    async fn stream(
        &mut self,
        value: Value,
        kind: SegmentKind,
    ) -> Result<(ByteRange, Option<Decode>)> {
        let Value::Reference(reference) = value else {
            return Err(invalid("stream must be indirect"));
        };
        let object = self.object(reference).await?;
        let mut dict = object.value.dictionary()?;
        let expected_type: &[u8] = if kind == SegmentKind::Xmp {
            b"Metadata"
        } else {
            b"EmbeddedFile"
        };
        if !required(&mut dict, b"Type")?.is_name(expected_type) {
            return Err(invalid("unexpected metadata stream type"));
        }
        if kind == SegmentKind::Xmp && !required(&mut dict, b"Subtype")?.is_name(b"XML") {
            return Err(invalid("XMP stream must have XML subtype"));
        }
        let decode = Decode::from_dictionary(&mut dict)?;
        let range = self.stream_range(object.stream, &mut dict, false).await?;
        if range.len() > MAX_SEGMENT_SIZE {
            return Err(invalid("metadata stream exceeds 256 MiB limit"));
        }
        Ok((range, decode))
    }

    /// The current catalog's XMP stream and document-level C2PA store.
    async fn metadata(&mut self, root: Reference) -> Result<CatalogMetadata> {
        let catalog_value = self.resolve(Value::Reference(root)).await?;
        let catalog = catalog_value.dictionary()?;
        if !catalog
            .get(b"Type".as_slice())
            .is_some_and(|value| value.is_name(b"Catalog"))
        {
            return Err(invalid("Root is not a Catalog"));
        }
        let mut ranges = Vec::new();
        let mut associated = Vec::new();
        let mut active = Some(BTreeSet::new());
        if let Some(value) = catalog.get(b"Metadata".as_slice()).cloned() {
            if !matches!(value, Value::Null) {
                let (range, decode) = self.stream(value, SegmentKind::Xmp).await?;
                ranges.push((range, SegmentKind::Xmp, decode));
            }
        }
        if let Some(value) = catalog.get(b"AF".as_slice()).cloned() {
            let values = match self.resolve(value.clone()).await? {
                Value::Array(values) => values,
                Value::Null => Vec::new(),
                Value::Dictionary(_) => vec![value],
                _ => return Err(invalid("invalid Catalog AF")),
            };
            for value in &values {
                let mut spec = self.resolve(value.clone()).await?.dictionary()?;
                let Some(relationship) = spec.remove(b"AFRelationship".as_slice()) else {
                    continue;
                };
                let relationship = self.resolve(relationship).await?;
                if !matches!(relationship, Value::Name(_)) {
                    return Err(invalid("AFRelationship must be a name"));
                }
                if !relationship.is_name(b"C2PA_Manifest") {
                    continue;
                }
                match value {
                    Value::Reference(reference) => {
                        if let Some(active) = &mut active {
                            active.insert(*reference);
                        }
                    }
                    _ => active = None,
                }
                if ranges
                    .iter()
                    .any(|(_, kind, _)| *kind == SegmentKind::Jumbf)
                {
                    return Err(invalid("multiple document-level C2PA manifests"));
                }
                if !required(&mut spec, b"Subtype")?.is_name(b"application/c2pa") {
                    return Err(invalid("C2PA file specification has wrong subtype"));
                }
                let ef_value = required(&mut spec, b"EF")?;
                let mut ef = self.resolve(ef_value).await?.dictionary()?;
                let stream = match (ef.remove(b"F".as_slice()), ef.remove(b"UF".as_slice())) {
                    (Some(Value::Reference(f)), Some(Value::Reference(uf))) if f == uf => {
                        Value::Reference(f)
                    }
                    (Some(_), Some(_)) => {
                        return Err(invalid("conflicting embedded file references"))
                    }
                    (Some(value), None) | (None, Some(value)) => value,
                    (None, None) => return Err(invalid("missing C2PA embedded file")),
                };
                let (range, decode) = self.stream(stream, SegmentKind::Jumbf).await?;
                ranges.push((range, SegmentKind::Jumbf, decode));
            }
            associated = values;
        }
        Ok(CatalogMetadata {
            catalog,
            associated,
            active,
            ranges,
        })
    }
}

/// A result from a lexer over bytes that are all there: running out is an error.
fn finish<T>(result: Lex<T>) -> Result<T> {
    match result {
        Ok(v) => Ok(v),
        Err(Stop::Fail(e)) => Err(e),
        Err(Stop::More) => Err(invalid("unexpected end of data")),
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

async fn parse_with(handler: Arc<dyn ContainerHandler>, mut io: Io) -> Result<Structure> {
    let size = io.len();
    if size < HEADER_LEN {
        return Err(invalid("invalid header"));
    }
    let mut header = [0u8; HEADER_LEN as usize];
    io.read_at(0, &mut header).await?;
    if !PdfHandler.detect(&header) {
        return Err(invalid("invalid header"));
    }
    let offset = startxref(&mut io, size).await?;

    let mut parser = Parser::new(&mut io, size);
    let root = parser.xrefs(offset).await?;
    let found = parser.metadata(root).await?;
    let edit = write::context(
        &mut parser,
        root,
        offset,
        found.catalog,
        found.associated,
        found.active,
    )
    .await?;
    let mut ranges = found.ranges;
    ranges.sort_by_key(|(range, _, _)| range.offset());

    let mut b = StructureBuilder::new(handler, "application/pdf", size);
    b.push_span(SegmentKind::Header, Name::None, HEADER_LEN as u32, 0, 0)?;
    let mut end = HEADER_LEN;
    let mut state = PdfState {
        decodes: BTreeMap::new(),
        edit,
    };
    for (range, kind, decode) in ranges {
        // An empty stream holds nothing to expose.
        if range.is_empty() {
            continue;
        }
        if range.offset() < end {
            return Err(invalid("overlapping metadata streams"));
        }
        if range.offset() > end {
            b.push_span(SegmentKind::Other, Name::None, 0, range.offset() - end, 0)?;
        }
        let id = b.push_item(kind, Name::None, 0, range.len(), 0)?;
        if let Some(decode) = decode {
            state.decodes.insert(id.index(), decode);
        }
        end = range.end();
    }
    if end < size {
        b.push_span(SegmentKind::Other, Name::None, 0, size - end, 0)?;
    }
    b.attach(state);
    b.finish()
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Part of a run of text between the segments: kept source bytes or new ones.
enum Text<'a> {
    Copy(ByteRange),
    Put(Cow<'a, [u8]>),
}

/// Emit the run as one segment of other bytes, which is how parsing the output sees
/// them: whatever lies between the metadata streams.
fn flush<'a>(b: &mut PlanBuilder<'_, 'a>, run: &mut Vec<Text<'a>>) -> Result<()> {
    let len: u64 = run
        .iter()
        .map(|t| match t {
            Text::Copy(r) => r.len(),
            Text::Put(d) => d.len() as u64,
        })
        .sum();
    if len == 0 {
        run.clear();
        return Ok(());
    }
    let id = b.shape(SegmentKind::Other, Name::None, 0, len, 0)?;
    for piece in run.drain(..) {
        match piece {
            Text::Copy(r) if !r.is_empty() => {
                b.copy(id, Role::Payload, r);
            }
            Text::Put(d) if !d.is_empty() => {
                b.put(id, Role::Payload, d);
            }
            _ => {}
        }
    }
    Ok(())
}

/// A write is the whole source, unchanged, then an appended revision. In the output,
/// a metadata stream the catalog no longer points at is just bytes, so those are
/// merged into the text around them, as parsing the output sees it.
fn plan<'a>(src: &Structure, source_len: u64, updates: &'a Updates) -> Result<WritePlan<'a>> {
    if source_len != src.total_size() {
        return Err(invalid("source size changed since parsing"));
    }
    let state = PdfState::of(src)?;
    let segments = src.segments();
    let mut b = PlanBuilder::new(src);
    let noop = matches!(updates.xmp(), MetadataUpdate::Keep)
        && matches!(updates.jumbf(), MetadataUpdate::Keep);
    if noop {
        for i in 0..segments.len() {
            b.copy_segment(SegmentId::from_index(i)?)?;
        }
        b.carry_entities(|_| true);
        b.attach(state.clone());
        return b.finish();
    }
    if let Some(refusal) = state.edit.refusal() {
        return Err(refusal);
    }
    let write::State::Ready(context) = &state.edit else {
        return Err(invalid("missing editable PDF context"));
    };
    let revision = write::revision(context, src.total_size(), updates)?;

    let keeps = |kind: SegmentKind| {
        matches!(
            if kind == SegmentKind::Xmp {
                updates.xmp()
            } else {
                updates.jumbf()
            },
            MetadataUpdate::Keep
        )
    };
    let mut run: Vec<Text<'a>> = Vec::new();
    let mut decodes = BTreeMap::new();
    for (i, seg) in segments.iter().enumerate() {
        let id = SegmentId::from_index(i)?;
        match seg.kind() {
            SegmentKind::Header => {
                b.copy_segment(id)?;
            }
            kind @ (SegmentKind::Xmp | SegmentKind::Jumbf) if keeps(kind) => {
                flush(&mut b, &mut run)?;
                let dest = b.copy_segment(id)?;
                if let Some(decode) = state.decodes.get(&i) {
                    decodes.insert(dest.index(), *decode);
                }
            }
            _ => run.push(Text::Copy(seg.extent())),
        }
    }
    for piece in revision.pieces {
        match piece {
            write::Piece::Syntax(bytes) => run.push(Text::Put(Cow::Owned(bytes))),
            write::Piece::Stream(kind) => {
                let data = match if kind == SegmentKind::Xmp {
                    updates.xmp()
                } else {
                    updates.jumbf()
                } {
                    MetadataUpdate::Set(data) => data.as_slice(),
                    _ => return Err(invalid("write plan payload missing")),
                };
                // An empty stream holds nothing to expose.
                if !data.is_empty() {
                    flush(&mut b, &mut run)?;
                    let id = b.put_segment(kind, Name::None, &[][..], data, &[][..])?;
                    b.entity_for(id)?;
                }
            }
        }
    }
    flush(&mut b, &mut run)?;
    b.carry_entities(|_| true);
    b.attach(PdfState {
        decodes,
        edit: write::State::Ready(Arc::new(revision.context)),
    });
    b.finish()
}

/// The offset the final `startxref` gives, which must be in the last [`TAIL`] bytes.
async fn startxref(io: &mut Io, size: u64) -> Result<u64> {
    let tail_size = size.min(TAIL);
    let tail = io.read_vec(size - tail_size, tail_size as usize).await?;
    let tail = tail.trim_ascii_end();
    let tail = tail
        .strip_suffix(b"%%EOF")
        .ok_or_else(|| invalid("missing final EOF marker"))?;
    let start = tail
        .windows(9)
        .rposition(|window| window == b"startxref")
        .ok_or_else(|| invalid("missing startxref in final 1024 bytes"))?;
    let number = &tail[start + 9..];
    if !number.first().is_some_and(|b| whitespace(*b)) {
        return Err(invalid("invalid startxref"));
    }
    unsigned(number.trim_ascii())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_fixture_exposes_its_xmp() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/pdf_metadata.pdf"
        ))
        .unwrap();
        let s = parse(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(s.media_type(), "application/pdf");
        let xmp = s.xmp(&mut Cursor::new(&bytes)).unwrap();
        assert!(xmp.is_some_and(|x| !x.is_empty()));
    }
}
