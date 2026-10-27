// Copyright 2023 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

#![doc = include_str!("README.md")]

mod write;

use crate::{
    BoxFuture, ByteRange, ContainerHandler, Error, FormatSpec, Io, ItemInfo, ItemKind, Name,
    Result, SegmentId, SegmentKind, Structure, StructureBuilder, Updates, WritePlan,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::sync::Arc;

/// TIFF tag holding a C2PA manifest store, of type UNDEFINED (7).
pub const C2PA_TAG: u16 = 0xcd41;
/// TIFF tag holding XMP, of type BYTE (1).
pub const XMP_TAG: u16 = 700;
/// The TIFF handler's container identifier.
pub const ID: &str = "tiff";

const MAX_DIRECTORIES: usize = 3000;
const MAX_ENTRIES: usize = 1 << 20;
const MAX_DEPTH: usize = 32;
const CHILD_TAGS: [u16; 4] = [330, 34665, 34853, 40965];

/// Byte order of a TIFF stream, including its IFD fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    fn get(self, data: &[u8]) -> u64 {
        match self {
            Self::Little => data.iter().rev().fold(0, |a, b| a << 8 | *b as u64),
            Self::Big => data.iter().fold(0, |a, b| a << 8 | *b as u64),
        }
    }

    fn put(self, value: u64, width: usize) -> Result<Vec<u8>> {
        if width < 8 && value >= 1u64 << (width * 8) {
            return Err(Error::InvalidFormat(
                "TIFF field exceeds its integer width".into(),
            ));
        }
        Ok(match self {
            Self::Little => value.to_le_bytes()[..width].to_vec(),
            Self::Big => value.to_be_bytes()[8 - width..].to_vec(),
        })
    }
}

/// One TIFF tag entry and the geometry of its stored value.
#[derive(Debug, Clone)]
pub struct Entry {
    tag: u16,
    field_type: u16,
    count: u64,
    range: ByteRange,
    value: Option<ByteRange>,
    raw: Vec<u8>,
}

impl Entry {
    pub fn tag(&self) -> u16 {
        self.tag
    }
    pub fn field_type(&self) -> u16 {
        self.field_type
    }
    pub fn count(&self) -> u64 {
        self.count
    }
    /// The complete 12-byte or 20-byte directory entry.
    pub fn range(&self) -> ByteRange {
        self.range
    }
    /// Stored value, including inline values. Unknown field types have no range.
    pub fn value_range(&self) -> Option<ByteRange> {
        self.value
    }
    /// The value-count field, independent of the value/offset field.
    ///
    /// Callers constructing TIFF C2PA data-hash exclusions need this field as well
    /// as the manifest store's bytes.
    pub fn count_range(&self) -> ByteRange {
        // The entry's validated geometry guarantees that this range fits.
        ByteRange::new(
            self.range.offset() + 4,
            if self.raw.len() == 20 { 8 } else { 4 },
        )
        .expect("validated TIFF entry")
    }
}

/// A reachable TIFF image, SubIFD, EXIF, GPS, or interoperability directory.
#[derive(Debug, Clone)]
pub struct Directory {
    range: ByteRange,
    entries: BTreeMap<u16, Entry>,
    next: u64,
    children: Vec<u64>,
}

impl Directory {
    pub fn range(&self) -> ByteRange {
        self.range
    }
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }
    pub fn entry(&self, tag: u16) -> Option<&Entry> {
        self.entries.get(&tag)
    }
    pub fn next_ifd(&self) -> u64 {
        self.next
    }
}

/// TIFF geometry retained with parsed, planned, and written structures.
#[derive(Debug, Clone)]
pub struct TiffLayout {
    order: ByteOrder,
    big: bool,
    pages: Vec<u64>,
    directories: BTreeMap<u64, Directory>,
    media: Vec<(ByteRange, u16)>,
}

impl TiffLayout {
    pub fn byte_order(&self) -> ByteOrder {
        self.order
    }
    pub fn is_big_tiff(&self) -> bool {
        self.big
    }
    /// Main IFD chain in logical page order, including a dedicated manifest IFD.
    pub fn pages(&self) -> impl Iterator<Item = &Directory> {
        self.pages.iter().map(|at| &self.directories[at])
    }
    /// All reachable directories, in physical file order.
    pub fn directories(&self) -> impl Iterator<Item = &Directory> {
        self.directories.values()
    }
    /// The last IFD's C2PA tag, falling back to the first IFD for older files.
    pub fn manifest_entry(&self) -> Option<&Entry> {
        self.pages()
            .last()
            .and_then(|d| d.entry(C2PA_TAG))
            .or_else(|| self.pages().next().and_then(|d| d.entry(C2PA_TAG)))
    }
    pub fn xmp_entry(&self) -> Option<&Entry> {
        self.pages().next().and_then(|d| d.entry(XMP_TAG))
    }
    fn width(&self) -> usize {
        if self.big {
            8
        } else {
            4
        }
    }
    fn count_width(&self) -> usize {
        if self.big {
            8
        } else {
            2
        }
    }
    fn entry_width(&self) -> usize {
        if self.big {
            20
        } else {
            12
        }
    }
    fn header_len(&self) -> u64 {
        if self.big {
            16
        } else {
            8
        }
    }
}

/// Inspect TIFF-specific tag and directory geometry without additional I/O.
pub fn layout(structure: &Structure) -> Result<&TiffLayout> {
    structure
        .attachment::<Arc<TiffLayout>>()
        .map(Arc::as_ref)
        .ok_or(Error::UnsupportedFormat)
}

/// Classic TIFF, BigTIFF, and DNG handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct TiffHandler;

pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(TiffHandler)
}

impl ContainerHandler for TiffHandler {
    fn id(&self) -> &'static str {
        ID
    }
    fn formats(&self) -> &[FormatSpec] {
        const FORMATS: &[FormatSpec] = &[
            FormatSpec::new("image/tiff", "tif").extension_aliases(&["tiff"]),
            FormatSpec::new("image/x-adobe-dng", "dng").media_type_aliases(&["image/dng"]),
        ];
        FORMATS
    }
    fn detect(&self, header: &[u8]) -> bool {
        header.starts_with(b"II\x2a\0")
            || header.starts_with(b"MM\0\x2a")
            || header.starts_with(b"II\x2b\0")
            || header.starts_with(b"MM\0\x2b")
    }
    fn parse_async(self: Arc<Self>, mut io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async move {
            let state = read_layout(&mut io).await?;
            structure(self, io.len(), Arc::new(state))
        })
    }
    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        source: &'f Structure,
        _io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move { write::plan(source, updates) })
    }
    fn compose(&self, kind: ItemKind, data: &[u8]) -> Result<Vec<u8>> {
        match kind {
            ItemKind::Jumbf | ItemKind::Xmp => Ok(data.to_vec()),
            _ => Err(Error::UnsupportedFormat),
        }
    }
}

pub fn parse<R: Read + Seek>(source: &mut R) -> Result<Structure> {
    handler().parse(source)
}

fn checked_range(at: u64, len: u64, total: u64) -> Result<ByteRange> {
    let range = ByteRange::new(at, len)?;
    if range.end() > total {
        return Err(Error::invalid_segment(at, "TIFF range outside the stream"));
    }
    Ok(range)
}

fn type_width(ty: u16) -> Option<u64> {
    match ty {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

async fn integer(io: &mut Io, at: u64, width: usize, order: ByteOrder) -> Result<u64> {
    let mut bytes = [0; 8];
    io.read_at(at, &mut bytes[..width]).await?;
    Ok(order.get(&bytes[..width]))
}

async fn read_directory(io: &mut Io, state: &TiffLayout, at: u64) -> Result<Directory> {
    if at < state.header_len() {
        return Err(Error::invalid_segment(at, "IFD overlaps the TIFF header"));
    }
    let count_width = state.count_width();
    checked_range(at, count_width as u64, io.len())?;
    let count = integer(io, at, count_width, state.order).await?;
    if count > 65535 {
        return Err(Error::LimitExceeded {
            what: "TIFF entries per IFD",
            limit: 65535,
        });
    }
    let length = count_width as u64 + count * state.entry_width() as u64 + state.width() as u64;
    let range = checked_range(at, length, io.len())?;
    let bytes = io.read_vec(at, length as usize).await?;
    let mut entries = BTreeMap::new();
    for (i, raw) in bytes[count_width..count_width + count as usize * state.entry_width()]
        .chunks_exact(state.entry_width())
        .enumerate()
    {
        let tag = state.order.get(&raw[..2]) as u16;
        let field_type = state.order.get(&raw[2..4]) as u16;
        let value_count = state.order.get(&raw[4..4 + state.width()]);
        let entry_at = at + count_width as u64 + i as u64 * state.entry_width() as u64;
        let value = match type_width(field_type) {
            Some(width) => {
                let len = value_count.checked_mul(width).ok_or_else(|| {
                    Error::invalid_segment(entry_at, "TIFF value length overflow")
                })?;
                let offset = if len <= state.width() as u64 {
                    entry_at + 4 + state.width() as u64
                } else {
                    state.order.get(&raw[4 + state.width()..])
                };
                Some(checked_range(offset, len, io.len())?)
            }
            None => None,
        };
        let entry = Entry {
            tag,
            field_type,
            count: value_count,
            range: ByteRange::new(entry_at, state.entry_width() as u64)?,
            value,
            raw: raw.to_vec(),
        };
        if entries.insert(tag, entry).is_some() {
            return Err(Error::invalid_segment(
                entry_at,
                "duplicate TIFF tag in IFD",
            ));
        }
    }
    Ok(Directory {
        range,
        entries,
        next: state.order.get(&bytes[bytes.len() - state.width()..]),
        children: Vec::new(),
    })
}

async fn values(io: &mut Io, state: &TiffLayout, entry: &Entry) -> Result<Vec<u64>> {
    let width = match entry.field_type {
        3 => 2,
        4 | 13 => 4,
        16 | 18 => 8,
        _ => {
            return Err(Error::invalid_segment(
                entry.range.offset(),
                "invalid TIFF offset/count type",
            ))
        }
    };
    if entry.count > MAX_ENTRIES as u64 {
        return Err(Error::LimitExceeded {
            what: "TIFF offset/count array",
            limit: MAX_ENTRIES as u64,
        });
    }
    let range = entry
        .value
        .ok_or_else(|| Error::invalid_segment(entry.range.offset(), "unknown TIFF value type"))?;
    let mut result = Vec::with_capacity(entry.count as usize);
    for i in 0..entry.count {
        result.push(integer(io, range.offset() + i * width as u64, width, state.order).await?);
    }
    Ok(result)
}

async fn read_layout(io: &mut Io) -> Result<TiffLayout> {
    checked_range(0, 8, io.len())?;
    let head = io.read_vec(0, 8).await?;
    let order = match &head[..2] {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return Err(Error::InvalidFormat("not a TIFF stream".into())),
    };
    let big = match order.get(&head[2..4]) {
        42 => false,
        43 if order.get(&head[4..6]) == 8 && order.get(&head[6..8]) == 0 => true,
        _ => {
            return Err(Error::InvalidFormat(
                "invalid TIFF or BigTIFF header".into(),
            ))
        }
    };
    let mut state = TiffLayout {
        order,
        big,
        pages: Vec::new(),
        directories: BTreeMap::new(),
        media: Vec::new(),
    };
    let first = if big {
        checked_range(0, 16, io.len())?;
        integer(io, 8, 8, order).await?
    } else {
        order.get(&head[4..8])
    };
    if first == 0 {
        return Err(Error::InvalidFormat("TIFF has no first IFD".into()));
    }
    let mut queue = vec![first];
    let mut edges = BTreeMap::new();
    let mut entries = 0;
    while let Some(at) = queue.pop() {
        if state.directories.contains_key(&at) {
            continue;
        }
        if state.directories.len() >= MAX_DIRECTORIES {
            return Err(Error::LimitExceeded {
                what: "TIFF directories",
                limit: MAX_DIRECTORIES as u64,
            });
        }
        let mut dir = read_directory(io, &state, at).await?;
        entries += dir.entries.len();
        if entries > MAX_ENTRIES {
            return Err(Error::LimitExceeded {
                what: "TIFF entries",
                limit: MAX_ENTRIES as u64,
            });
        }
        let mut children = Vec::new();
        for tag in CHILD_TAGS {
            if let Some(entry) = dir.entry(tag) {
                children.extend(
                    values(io, &state, entry)
                        .await?
                        .into_iter()
                        .filter(|v| *v != 0),
                );
            }
        }
        dir.children = children.clone();
        if dir.next != 0 {
            children.push(dir.next);
        }
        if children.len() > MAX_DIRECTORIES || queue.len() > MAX_ENTRIES {
            return Err(Error::LimitExceeded {
                what: "TIFF directory links",
                limit: MAX_ENTRIES as u64,
            });
        }
        for (offset_tag, size_tag) in [(273, 279), (324, 325), (513, 514)] {
            // Unpaired, unused strip/tile tags stay opaque: no image-data
            // length can be inferred from just one member of a pair.
            if let (Some(offsets), Some(sizes)) = (dir.entry(offset_tag), dir.entry(size_tag)) {
                if offsets.count != sizes.count {
                    return Err(Error::invalid_segment(
                        at,
                        "TIFF image offsets/counts do not match",
                    ));
                }
                let offsets = values(io, &state, offsets).await?;
                let sizes = values(io, &state, sizes).await?;
                if state.media.len().saturating_add(offsets.len()) > MAX_ENTRIES {
                    return Err(Error::LimitExceeded {
                        what: "TIFF image blocks",
                        limit: MAX_ENTRIES as u64,
                    });
                }
                for (offset, size) in offsets.into_iter().zip(sizes) {
                    if size != 0 {
                        state
                            .media
                            .push((checked_range(offset, size, io.len())?, offset_tag));
                    }
                }
            }
        }
        queue.extend(children.iter().copied());
        edges.insert(at, children);
        state.directories.insert(at, dir);
    }
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    let mut stack = vec![(first, false, 0)];
    while let Some((at, exit, depth)) = stack.pop() {
        if exit {
            active.remove(&at);
            done.insert(at);
            continue;
        }
        if active.contains(&at) {
            return Err(Error::invalid_segment(at, "cyclic TIFF directory links"));
        }
        if done.contains(&at) {
            continue;
        }
        if depth > MAX_DEPTH {
            return Err(Error::LimitExceeded {
                what: "TIFF directory nesting",
                limit: MAX_DEPTH as u64,
            });
        }
        active.insert(at);
        stack.push((at, true, depth));
        for child in &edges[&at] {
            // A next-IFD link is a sibling, not another nesting level.
            stack.push((
                *child,
                false,
                depth + usize::from(*child != state.directories[&at].next),
            ));
        }
    }
    let mut at = first;
    while at != 0 {
        state.pages.push(at);
        at = state.directories[&at].next;
    }
    state
        .media
        .sort_unstable_by_key(|(r, tag)| (r.offset(), r.len(), *tag));
    state.media.dedup();
    validate_layout(&state)?;
    Ok(state)
}

fn overlaps(a: ByteRange, b: ByteRange) -> bool {
    !a.is_empty() && !b.is_empty() && a.offset() < b.end() && b.offset() < a.end()
}

fn validate_layout(state: &TiffLayout) -> Result<()> {
    if state.directories.len() > MAX_DIRECTORIES {
        return Err(Error::LimitExceeded {
            what: "TIFF directories",
            limit: MAX_DIRECTORIES as u64,
        });
    }
    if state.directories().map(|d| d.entries.len()).sum::<usize>() > MAX_ENTRIES {
        return Err(Error::LimitExceeded {
            what: "TIFF entries",
            limit: MAX_ENTRIES as u64,
        });
    }
    let mut end = state.header_len();
    for dir in state.directories() {
        if dir.range.offset() < end {
            return Err(Error::invalid_segment(
                dir.range.offset(),
                "overlapping TIFF directories",
            ));
        }
        end = dir.range.end();
    }
    for (range, _) in &state.media {
        let directory = state
            .directories
            .range(..range.end())
            .next_back()
            .map(|(_, d)| d);
        if range.offset() < state.header_len()
            || directory.is_some_and(|d| overlaps(*range, d.range))
        {
            return Err(Error::invalid_segment(
                range.offset(),
                "TIFF image data aliases a directory or header",
            ));
        }
    }
    for (entry, expected) in [(state.manifest_entry(), 7), (state.xmp_entry(), 1)] {
        let Some(entry) = entry else { continue };
        if entry.field_type != expected {
            return Err(Error::invalid_segment(
                entry.range.offset(),
                "incorrect TIFF metadata field type",
            ));
        }
        let value = entry.value.ok_or_else(|| {
            Error::invalid_segment(entry.range.offset(), "missing metadata value")
        })?;
        if value.len() > state.width() as u64
            && (value.offset() < state.header_len()
                || state.directories().any(|d| overlaps(value, d.range))
                || state
                    .directories()
                    .flat_map(Directory::entries)
                    .any(|e| e.range != entry.range && e.value.is_some_and(|r| overlaps(value, r))))
        {
            return Err(Error::invalid_segment(
                value.offset(),
                "TIFF metadata aliases other TIFF fields",
            ));
        }
        if state.media.iter().any(|(r, _)| overlaps(value, *r)) {
            return Err(Error::invalid_segment(
                value.offset(),
                "TIFF metadata aliases image data",
            ));
        }
    }
    Ok(())
}

fn metadata(state: &TiffLayout) -> impl Iterator<Item = (&Entry, ItemInfo)> {
    [
        state
            .manifest_entry()
            .map(|e| (e, ItemInfo::jumbf().native_type(Name::Id(C2PA_TAG as u64)))),
        state
            .xmp_entry()
            .map(|e| (e, ItemInfo::xmp().native_type(Name::Id(XMP_TAG as u64)))),
    ]
    .into_iter()
    .flatten()
}

fn structure(
    handler: Arc<dyn ContainerHandler>,
    total: u64,
    state: Arc<TiffLayout>,
) -> Result<Structure> {
    // TIFF is pointer-based, so known ranges may be shared or inline. A boundary
    // sweep produces one gapless tiling without copying any referenced values.
    let mut spans = vec![(
        ByteRange::new(0, state.header_len())?,
        4,
        SegmentKind::Preamble,
        Name::FourCC(*b"TIFF"),
    )];
    spans.extend(
        state
            .directories()
            .map(|d| (d.range, 2, SegmentKind::Other, Name::FourCC(*b"IFD "))),
    );
    spans.extend(
        state
            .media
            .iter()
            .map(|(r, tag)| (*r, 1, SegmentKind::MediaData, Name::Id(*tag as u64))),
    );
    for (entry, _) in metadata(&state) {
        if let Some(range) = entry.value.filter(|r| r.len() > state.width() as u64) {
            spans.push((range, 3, SegmentKind::Metadata, Name::Id(entry.tag as u64)));
        }
    }
    let mut events = Vec::with_capacity(spans.len() * 2);
    for (i, (range, _, _, _)) in spans.iter().enumerate() {
        events.push((range.offset(), true, i));
        events.push((range.end(), false, i));
    }
    events.sort_unstable();
    let media_type = if state
        .pages()
        .next()
        .is_some_and(|d| d.entry(50706).is_some())
    {
        "image/x-adobe-dng"
    } else {
        "image/tiff"
    };
    let mut builder = StructureBuilder::new(handler, media_type, total);
    let mut active = BTreeSet::new();
    let mut at = 0;
    let mut placed = Vec::new();
    for (offset, add, i) in events
        .into_iter()
        .chain(std::iter::once((total, false, usize::MAX)))
    {
        if offset > at {
            let (kind, name) = active
                .last()
                .map(|(_, i): &(u8, usize)| (spans[*i].2, spans[*i].3))
                .unwrap_or((SegmentKind::Other, Name::None));
            let header = if kind == SegmentKind::Preamble {
                (offset - at) as u32
            } else {
                0
            };
            let id = builder.push_span(kind, name, header, offset - at - header as u64, 0)?;
            placed.push((id, ByteRange::new(at, offset - at)?));
            at = offset;
        }
        if i != usize::MAX {
            if add {
                active.insert((spans[i].1, i));
            } else {
                active.remove(&(spans[i].1, i));
            }
        }
    }
    for (entry, info) in metadata(&state) {
        let range = entry.value.ok_or(Error::NotFound {
            what: "TIFF metadata range",
        })?;
        let mut parts = Vec::new();
        let mut at = range.offset();
        let first = placed
            .partition_point(|(_, r)| r.offset() <= at)
            .saturating_sub(1);
        for (id, segment) in &placed[first..] {
            let end = range.end().min(segment.end());
            if at <= end {
                parts.push((*id, ByteRange::new(at, end - at)?));
                at = end;
            }
            if at == range.end() {
                break;
            }
        }
        builder.item(info, &parts)?;
    }
    builder.attach(state);
    builder.finish()
}
