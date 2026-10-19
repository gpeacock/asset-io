//! Validated container layout.

use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, ReadSeek, ReadWriteSeek};
use crate::range::ByteRange;
use crate::segment::{Name, Role, Segment};
use crate::segment::{SegmentKind, DEFAULT_CHUNK_SIZE, MAX_SEGMENT_SIZE};
use crate::sink::{Chunk, Sink};
use crate::walk::Node;
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

/// Upper bound on segments per asset. Bounds memory use on hostile inputs
/// made of many tiny elements.
pub const MAX_SEGMENTS: usize = 1 << 22;

/// Index of a segment within its [`Structure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SegmentId(u32);

impl SegmentId {
    pub(crate) fn from_index(i: usize) -> Result<Self> {
        u32::try_from(i)
            .map(SegmentId)
            .map_err(|_| Error::LimitExceeded {
                what: "segment count",
                limit: u32::MAX as u64,
            })
    }

    /// Position in [`Structure::segments`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A run of payload bytes belonging to an [`Entity`], and the segment holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    range: ByteRange,
    segment: SegmentId,
}

impl Part {
    /// The payload bytes.
    pub fn range(&self) -> ByteRange {
        self.range
    }

    /// The segment that contains them.
    pub fn segment(&self) -> SegmentId {
        self.segment
    }
}

/// A logical metadata item (XMP, JUMBF, EXIF) assembled from one or more parts.
///
/// Parts are payload-only: framing that repeats per part (JPEG XT headers, PNG
/// chunk headers) is not included. The common single-part case does not allocate.
#[derive(Debug, Clone)]
pub struct Entity {
    kind: SegmentKind,
    label: Option<Cow<'static, str>>,
    jumbf_type: Option<[u8; 16]>,
    first: Part,
    rest: Vec<Part>,
}

impl Entity {
    /// The type of a JUMBF superbox, from its description box, when the container
    /// recorded it while parsing: JPEG does, as it carries JUMBF of any type.
    /// `None` for other kinds, for containers that reserve an element for one
    /// kind of store (PNG `caBX`, the BMFF C2PA box), and for data that is not a
    /// superbox. [`Structure::jumbf_type`] reads it from the stream when needed.
    /// This crate attaches no meaning to any type.
    pub fn jumbf_type(&self) -> Option<[u8; 16]> {
        self.jumbf_type
    }

    /// Container-defined role of the entity among others of its kind, for example
    /// the purpose of a BMFF C2PA box (`manifest`, `update`). `None` if the
    /// container does not distinguish them.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Classification.
    pub fn kind(&self) -> SegmentKind {
        self.kind
    }

    /// Parts in logical order.
    pub fn parts(&self) -> impl Iterator<Item = &Part> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }

    /// Number of parts.
    pub fn part_count(&self) -> usize {
        1 + self.rest.len()
    }

    /// Total payload bytes across all parts (saturating).
    pub fn len(&self) -> u64 {
        self.parts()
            .fold(0u64, |acc, p| acc.saturating_add(p.range.len()))
    }

    /// True if all parts are empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A region of the file with a role, in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Owning segment.
    pub segment: SegmentId,
    /// Header, payload or trailer.
    pub role: Role,
    /// Byte range.
    pub range: ByteRange,
}

/// The parsed layout of an asset.
///
/// Invariants, enforced by [`StructureBuilder`]:
/// - `segments` are in file order and tile `[0, total_size)` exactly;
/// - no segment is empty and each fits `header + trailer <= extent`;
/// - every entity part lies inside the segment it names.
///
/// A `Structure` holds geometry only. It is `Send + Sync` and can be shared
/// across threads for parallel reads.
#[derive(Debug)]
pub struct Structure {
    handler: Arc<dyn ContainerHandler>,
    media_type: Cow<'static, str>,
    total_size: u64,
    segments: Vec<Segment>,
    entities: Vec<Entity>,
}

impl Structure {
    /// The handler that parsed this asset.
    pub fn handler(&self) -> &Arc<dyn ContainerHandler> {
        &self.handler
    }

    /// Canonical id of the container (the first format its handler declares).
    pub fn container_id(&self) -> &str {
        self.handler.formats().first().copied().unwrap_or("")
    }

    /// Media type, for example `image/jpeg`.
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// How a box-based hash names an element of this asset, or `None` if the
    /// element is not a box. Container knowledge lives in the handler.
    pub fn box_name(&self, name: Name) -> Option<Cow<'static, str>> {
        self.handler.box_name(name)
    }

    /// Asset size in bytes.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// All segments in file order.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Look up a segment.
    pub fn segment(&self, id: SegmentId) -> Result<&Segment> {
        self.segments
            .get(id.index())
            .ok_or(Error::NotFound { what: "segment" })
    }

    /// Every element of the file in order, as a hasher sees it: each segment, with
    /// segments that [subdivide](Segment::subdivides) replaced by their finer
    /// elements. The elements tile the file exactly as the segments do.
    ///
    /// `f` receives the owning segment's id and the element. For most segments the
    /// element is the segment itself. Expanding a subdividing segment reads its
    /// payload once, streaming, with no allocation proportional to its size.
    /// The same call works for every container; callers never test the format.
    pub fn elements<R, F>(&self, src: &mut R, mut f: F) -> Result<()>
    where
        R: Read + Seek,
        F: FnMut(SegmentId, &Segment) -> Result<()>,
    {
        for (i, seg) in self.segments.iter().enumerate() {
            let id = SegmentId(i as u32);
            if seg.subdivides() {
                self.subdivide(src, seg, &mut |e| f(id, e))?;
            } else {
                f(id, seg)?;
            }
        }
        Ok(())
    }

    fn subdivide<R: Read + Seek>(
        &self,
        src: &mut R,
        seg: &Segment,
        emit: &mut dyn FnMut(&Segment) -> Result<()>,
    ) -> Result<()> {
        let src: &mut dyn ReadSeek = src;
        self.handler.subdivide(src, seg, emit)
    }

    /// Every header/payload/trailer run of the file, in order, covering all bytes once.
    pub fn regions(&self) -> impl Iterator<Item = Region> + '_ {
        self.segments.iter().enumerate().flat_map(|(i, seg)| {
            let segment = SegmentId(i as u32);
            seg.regions().map(move |(role, range)| Region {
                segment,
                role,
                range,
            })
        })
    }

    /// All entities of a kind.
    pub fn entities(&self, kind: SegmentKind) -> impl Iterator<Item = &Entity> + '_ {
        self.entities.iter().filter(move |e| e.kind == kind)
    }

    /// All entities of a kind carrying `label` (see [`Entity::label`]).
    pub fn entities_labelled<'a>(
        &'a self,
        kind: SegmentKind,
        label: &'a str,
    ) -> impl Iterator<Item = &'a Entity> + 'a {
        self.entities
            .iter()
            .filter(move |e| e.kind == kind && e.label() == Some(label))
    }

    /// Visit the container's element hierarchy in file order, parents first.
    ///
    /// Flat containers yield their segments. Nested ones (ISO BMFF) also yield the
    /// elements inside segments, found lazily by reading only their headers. Callers
    /// never test the format; the handler decides what the hierarchy is.
    pub fn walk<R, F>(&self, src: &mut R, mut f: F) -> Result<()>
    where
        R: Read + Seek,
        F: FnMut(&Node<'_>) -> Result<()>,
    {
        let src: &mut dyn ReadSeek = src;
        self.handler.walk(self, src, &mut f)
    }

    /// Every entity with its kind, in declaration order.
    pub(crate) fn entity_list(&self) -> impl Iterator<Item = (SegmentKind, &Entity)> + '_ {
        self.entities.iter().map(|e| (e.kind, e))
    }

    /// The entity of a kind: `Ok(None)` if absent, `Err(Ambiguous)` if several.
    pub fn entity(&self, kind: SegmentKind) -> Result<Option<&Entity>> {
        let mut it = self.entities(kind);
        let first = it.next();
        let extra = it.count();
        match (first, extra) {
            (None, _) => Ok(None),
            (Some(e), 0) => Ok(Some(e)),
            (Some(_), n) => Err(Error::Ambiguous {
                what: kind.as_str(),
                count: n + 1,
            }),
        }
    }

    /// Like [`entity`](Self::entity) but absence is an error.
    pub fn require_entity(&self, kind: SegmentKind) -> Result<&Entity> {
        self.entity(kind)?.ok_or(Error::NotFound {
            what: kind.as_str(),
        })
    }

    // ------------------------------------------------------------------
    // Bounded data access
    // ------------------------------------------------------------------

    fn check_readable(&self, range: ByteRange) -> Result<()> {
        if range.end() > self.total_size {
            return Err(Error::InvalidSegment {
                offset: range.offset(),
                reason: "range extends past end of asset".into(),
            });
        }
        Ok(())
    }

    /// Read a range into memory. Fails above [`MAX_SEGMENT_SIZE`]; use
    /// [`stream_range`](Self::stream_range) for large ranges.
    pub fn read_range<R: Read + Seek>(&self, src: &mut R, range: ByteRange) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.read_into(src, range, &mut out)?;
        Ok(out)
    }

    /// Append a range to `out`. The range is validated against the asset size
    /// before any allocation, and reads grow `out` only as data arrives.
    fn read_into<R: Read + Seek>(
        &self,
        src: &mut R,
        range: ByteRange,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        self.check_readable(range)?;
        let total = (out.len() as u64).saturating_add(range.len());
        if total > MAX_SEGMENT_SIZE {
            return Err(Error::LimitExceeded {
                what: "read size",
                limit: MAX_SEGMENT_SIZE,
            });
        }
        let start = out.len();
        out.try_reserve(range.len() as usize)
            .map_err(|_| Error::LimitExceeded {
                what: "allocation",
                limit: MAX_SEGMENT_SIZE,
            })?;
        src.seek(SeekFrom::Start(range.offset()))?;
        Read::take(&mut *src, range.len()).read_to_end(out)?;
        if (out.len() - start) as u64 != range.len() {
            return Err(Error::InvalidSegment {
                offset: range.offset(),
                reason: "unexpected end of data".into(),
            });
        }
        Ok(())
    }

    /// The superbox type UUID of a JUMBF entity (ISO/IEC 19566-5): the 16-byte
    /// type in its description box. Callers compare it with the types they care
    /// about; this crate attaches no meaning to any of them.
    ///
    /// Returns `Ok(None)` if the entity is not a JUMBF superbox with a
    /// description box in the first 32 bytes (including boxes using extended
    /// lengths, which this does not interpret). Reads at most 32 bytes.
    pub fn jumbf_type<R: Read + Seek>(
        &self,
        src: &mut R,
        entity: &Entity,
    ) -> Result<Option<[u8; 16]>> {
        if entity.kind() != SegmentKind::Jumbf {
            return Ok(None);
        }
        if let Some(t) = entity.jumbf_type() {
            return Ok(Some(t));
        }
        let first = entity.parts().next().ok_or(Error::NotFound {
            what: "entity part",
        })?;
        let r = first.range();
        if r.len() < 32 {
            return Ok(None);
        }
        let head = self.read_range(src, ByteRange::new(r.offset(), 32)?)?;
        if &head[4..8] != b"jumb" || &head[12..16] != b"jumd" {
            return Ok(None);
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&head[16..32]);
        Ok(Some(uuid))
    }

    /// The JUMBF store as a reader sees it, or `None` if the asset has none.
    ///
    /// Usually that is the one JUMBF entity. A file with an `original` store and an
    /// `update` (ISO BMFF) reads as the two combined, with the update's children
    /// after the original's. Several unrelated stores are
    /// [`Error::Ambiguous`]. Merkle boxes are not stores and are never returned.
    pub fn store<R: Read + Seek>(&self, src: &mut R) -> Result<Option<Vec<u8>>> {
        use crate::segment::labels::{ORIGINAL, UPDATE};
        let original = self.entities_labelled(SegmentKind::Jumbf, ORIGINAL).next();
        let update = self.entities_labelled(SegmentKind::Jumbf, UPDATE).next();
        if let (Some(o), Some(u)) = (original, update) {
            let o = self.read_entity(src, o)?;
            let u = self.read_entity(src, u)?;
            return crate::containers::jumbf::extend(&o, &u).map(Some);
        }
        match self.entity(SegmentKind::Jumbf)? {
            Some(e) => self.read_entity(src, e).map(Some),
            None => Ok(None),
        }
    }

    /// The asset's XMP packet, or `None` if it has none. Containers that store XMP
    /// in several pieces return it reassembled.
    pub fn xmp(&self, src: &mut dyn ReadSeek) -> Result<Option<Vec<u8>>> {
        self.handler.read_xmp(self, src)
    }

    /// Read an entity's parts, concatenated.
    pub fn read_entity<R: Read + Seek>(&self, src: &mut R, entity: &Entity) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for part in entity.parts() {
            self.read_into(src, part.range, &mut out)?;
        }
        Ok(out)
    }

    /// Stream a range through a caller-owned buffer; no allocation.
    pub fn stream_range<R, F>(
        &self,
        src: &mut R,
        range: ByteRange,
        buf: &mut [u8],
        mut f: F,
    ) -> Result<()>
    where
        R: Read + Seek,
        F: FnMut(&[u8]) -> Result<()>,
    {
        self.check_readable(range)?;
        if buf.is_empty() {
            return Err(Error::InvalidFormat("empty stream buffer".into()));
        }
        src.seek(SeekFrom::Start(range.offset()))?;
        let mut remaining = range.len();
        while remaining > 0 {
            let n = remaining.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..n])?;
            f(&buf[..n])?;
            remaining -= n as u64;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Zero-copy access over a byte slice (for example a memory map)
    // ------------------------------------------------------------------

    /// Borrow `range` from `data`, which must hold the whole asset starting at
    /// offset 0. No copy is made. `data` is only length-checked: it is the
    /// caller's job to pass the bytes this structure was parsed from.
    pub fn slice<'a>(&self, data: &'a [u8], range: ByteRange) -> Result<&'a [u8]> {
        self.check_readable(range)?;
        if (data.len() as u64) < self.total_size {
            return Err(Error::InvalidSegment {
                offset: data.len() as u64,
                reason: format!(
                    "buffer holds {} bytes, asset is {}",
                    data.len(),
                    self.total_size
                ),
            });
        }
        let bad = || Error::InvalidSegment {
            offset: range.offset(),
            reason: "range does not fit in memory".into(),
        };
        let start = usize::try_from(range.offset()).map_err(|_| bad())?;
        let end = usize::try_from(range.end()).map_err(|_| bad())?;
        data.get(start..end).ok_or_else(bad)
    }

    /// An entity's bytes from `data`. Borrowed, with no copy, when the entity is
    /// a single part; copied (up to [`MAX_SEGMENT_SIZE`]) when it spans several.
    pub fn slice_entity<'a>(&self, data: &'a [u8], entity: &Entity) -> Result<Cow<'a, [u8]>> {
        let mut parts = entity.parts();
        let Some(first) = parts.next() else {
            return Ok(Cow::Borrowed(&[]));
        };
        let head = self.slice(data, first.range)?;
        let Some(second) = parts.next() else {
            return Ok(Cow::Borrowed(head));
        };
        let mut out = Vec::new();
        for r in [first.range, second.range]
            .into_iter()
            .chain(parts.map(|p| p.range))
        {
            let b = self.slice(data, r)?;
            if (out.len() as u64).saturating_add(b.len() as u64) > MAX_SEGMENT_SIZE {
                return Err(Error::LimitExceeded {
                    what: "read size",
                    limit: MAX_SEGMENT_SIZE,
                });
            }
            out.try_reserve(b.len()).map_err(|_| Error::LimitExceeded {
                what: "allocation",
                limit: MAX_SEGMENT_SIZE,
            })?;
            out.extend_from_slice(b);
        }
        Ok(Cow::Owned(out))
    }

    // ------------------------------------------------------------------
    // Read-side sink
    // ------------------------------------------------------------------

    /// Deliver every byte of the asset to `sink`, in file order, tagged with its
    /// segment and role: the read-side counterpart of [`write`](Self::write). Runs
    /// the sink would skip ([`Sink::wants`]) are not read. Chunks are at most
    /// [`DEFAULT_CHUNK_SIZE`] bytes and borrow one reused buffer.
    pub fn read<R: Read + Seek>(&self, src: &mut R, sink: &mut dyn Sink) -> Result<()> {
        sink.begin(self)?;
        let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
        for r in self.regions() {
            if !sink.wants(self.segment(r.segment)?, r.role) {
                continue;
            }
            self.check_readable(r.range)?;
            src.seek(SeekFrom::Start(r.range.offset()))?;
            let mut offset = r.range.offset();
            let mut remaining = r.range.len();
            while remaining > 0 {
                let n = remaining.min(buf.len() as u64) as usize;
                src.read_exact(&mut buf[..n])?;
                sink.chunk(Chunk {
                    segment: r.segment,
                    role: r.role,
                    offset,
                    data: &buf[..n],
                })?;
                offset += n as u64;
                remaining -= n as u64;
            }
        }
        Ok(())
    }

    /// Like [`read`](Self::read) over bytes already in memory
    /// (a memory map, say). Nothing is copied: each region reaches the sink as one
    /// chunk borrowed from `data`. A sink that wants parallelism can split the
    /// chunks itself.
    pub fn read_slice(&self, data: &[u8], sink: &mut dyn Sink) -> Result<()> {
        sink.begin(self)?;
        for r in self.regions() {
            if !sink.wants(self.segment(r.segment)?, r.role) {
                continue;
            }
            sink.chunk(Chunk {
                segment: r.segment,
                role: r.role,
                offset: r.range.offset(),
                data: self.slice(data, r.range)?,
            })?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // In-place patching
    // ------------------------------------------------------------------

    /// Overwrite an entity's payload in an already-written stream.
    ///
    /// `data` must fit the entity's total payload capacity; the remainder is
    /// zero-filled so the layout is unchanged. Container checksums that cover
    /// the patched payload (PNG CRC) are recomputed. `out` must contain the
    /// asset starting at offset 0.
    pub fn patch<W: Read + Write + Seek>(
        &self,
        out: &mut W,
        entity: &Entity,
        data: &[u8],
    ) -> Result<()> {
        let capacity = entity.len();
        if data.len() as u64 > capacity {
            return Err(Error::LimitExceeded {
                what: "patch exceeds payload capacity",
                limit: capacity,
            });
        }
        let mut rest = data;
        for part in entity.parts() {
            let take = (rest.len() as u64).min(part.range.len()) as usize;
            out.seek(SeekFrom::Start(part.range.offset()))?;
            out.write_all(&rest[..take])?;
            write_zeros(out, part.range.len() - take as u64)?;
            rest = &rest[take..];
        }
        // Parts are in order; fix each segment's trailer once.
        let mut last: Option<SegmentId> = None;
        for part in entity.parts() {
            if last != Some(part.segment) {
                last = Some(part.segment);
                self.fix_trailer(out, part.segment)?;
            }
        }
        out.flush()?;
        Ok(())
    }

    /// Write this asset to `out` with `updates` applied, delivering every output
    /// byte to `sink`, and return the layout of what was written. `src` must be
    /// the stream this structure was parsed from. The container's handler does the
    /// work; callers never name the format.
    pub fn write(
        &self,
        src: &mut dyn ReadSeek,
        out: &mut dyn ReadWriteSeek,
        updates: &crate::Updates,
        sink: &mut dyn crate::sink::Sink,
    ) -> Result<Structure> {
        self.handler.write(self, src, out, updates, sink)
    }

    /// The layout `write` would produce for `updates`, without writing: where each
    /// entity would land, and the total size. `src` must be the stream this
    /// structure was parsed from.
    ///
    /// The result is the one `write` returns, so a caller can learn where a
    /// manifest will go (and size a hash exclusion for it) before producing any
    /// output. A container that cannot plan reports [`Error::UnsupportedFormat`].
    pub fn plan(&self, src: &mut dyn ReadSeek, updates: &crate::Updates) -> Result<Structure> {
        self.handler.plan(self, src, updates)
    }

    /// Fill a placeholder: overwrite an entity's payload, in place, with `data` of
    /// exactly the same length.
    ///
    /// This is how a signed manifest replaces the placeholder it was sized from.
    /// Bytes outside the payload (box sizes, offsets, everything a hash or an
    /// offset table was computed over) are not touched, so none of them can move.
    /// Unlike [`patch`](Self::patch), which zero-fills a shorter `data`, a length
    /// mismatch is an error: it means the placeholder was not sized correctly.
    ///
    /// Writing the store again with `set_jumbf` is not a substitute: that is free
    /// to change the size.
    pub fn patch_exact<W: Read + Write + Seek>(
        &self,
        out: &mut W,
        entity: &Entity,
        data: &[u8],
    ) -> Result<()> {
        if data.len() as u64 != entity.len() {
            return Err(Error::InvalidFormat(format!(
                "replacement is {} bytes, the placeholder is {}",
                data.len(),
                entity.len()
            )));
        }
        self.patch(out, entity, data)
    }

    fn fix_trailer<W: Read + Write + Seek>(&self, out: &mut W, id: SegmentId) -> Result<()> {
        let out: &mut dyn ReadWriteSeek = out;
        self.handler.fix_trailer(out, self, id)
    }
}

fn write_zeros<W: Write>(w: &mut W, mut n: u64) -> Result<()> {
    static ZEROS: [u8; 4096] = [0; 4096];
    while n > 0 {
        let k = n.min(ZEROS.len() as u64) as usize;
        w.write_all(&ZEROS[..k])?;
        n -= k as u64;
    }
    Ok(())
}

/// Builds a [`Structure`], rejecting anything that would violate its invariants.
#[derive(Debug)]
pub struct StructureBuilder {
    handler: Arc<dyn ContainerHandler>,
    media_type: Cow<'static, str>,
    total_size: u64,
    cursor: u64,
    segments: Vec<Segment>,
    entities: Vec<Entity>,
}

impl StructureBuilder {
    /// Start a layout for an asset of known size, owned by `handler`.
    pub fn new(
        handler: Arc<dyn ContainerHandler>,
        media_type: impl Into<Cow<'static, str>>,
        total_size: u64,
    ) -> Self {
        Self {
            handler,
            media_type: media_type.into(),
            total_size,
            cursor: 0,
            segments: Vec::new(),
            entities: Vec::new(),
        }
    }

    /// Offset where the next segment must start.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Append a segment. It must start at the cursor and end within the asset.
    pub fn push(&mut self, seg: Segment) -> Result<SegmentId> {
        if self.segments.len() >= MAX_SEGMENTS {
            return Err(Error::LimitExceeded {
                what: "segments",
                limit: MAX_SEGMENTS as u64,
            });
        }
        let ext = seg.extent();
        if ext.offset() != self.cursor {
            return Err(Error::InvalidSegment {
                offset: ext.offset(),
                reason: format!("segment must start at {} (gap or overlap)", self.cursor),
            });
        }
        if ext.end() > self.total_size {
            return Err(Error::InvalidSegment {
                offset: ext.offset(),
                reason: "segment extends past end of asset".into(),
            });
        }
        let id = SegmentId::from_index(self.segments.len())?;
        self.cursor = ext.end();
        self.segments.push(seg);
        Ok(id)
    }

    /// Append a segment at the cursor from its three lengths.
    pub fn push_span(
        &mut self,
        kind: SegmentKind,
        name: Name,
        header_len: u32,
        payload_len: u64,
        trailer_len: u16,
    ) -> Result<SegmentId> {
        let total = (header_len as u64)
            .checked_add(payload_len)
            .and_then(|v| v.checked_add(trailer_len as u64))
            .ok_or_else(|| Error::InvalidSegment {
                offset: self.cursor,
                reason: "segment length overflows".into(),
            })?;
        let seg = Segment::new(
            ByteRange::new(self.cursor, total)?,
            header_len,
            trailer_len,
            kind,
            name,
        )?;
        self.push(seg)
    }

    /// Segment already pushed.
    pub fn segment(&self, id: SegmentId) -> Result<&Segment> {
        self.segments
            .get(id.index())
            .ok_or(Error::NotFound { what: "segment" })
    }

    /// Record that a pushed segment contains finer elements that the container
    /// can enumerate by reading its payload.
    pub fn mark_subdivides(&mut self, id: SegmentId) -> Result<()> {
        let slot = self
            .segments
            .get_mut(id.index())
            .ok_or(Error::NotFound { what: "segment" })?;
        *slot = slot.with_subdivision();
        Ok(())
    }

    /// Declare an entity whose parts are the payloads of whole segments.
    pub fn entity_of_segments(&mut self, kind: SegmentKind, ids: &[SegmentId]) -> Result<()> {
        self.entity_of_segments_typed(kind, None, ids)
    }

    /// Like [`entity_of_segments`](Self::entity_of_segments), recording the
    /// [JUMBF type](Entity::jumbf_type) of the item.
    pub fn entity_of_segments_typed(
        &mut self,
        kind: SegmentKind,
        jumbf_type: Option<[u8; 16]>,
        ids: &[SegmentId],
    ) -> Result<()> {
        let mut parts = Vec::with_capacity(ids.len());
        for &id in ids {
            parts.push((id, self.segment(id)?.payload()));
        }
        self.entity_with_label(kind, None, jumbf_type, &parts)
    }

    /// Declare an entity from explicit `(segment, range)` parts. Each range must
    /// lie within its segment's extent.
    pub fn entity(&mut self, kind: SegmentKind, parts: &[(SegmentId, ByteRange)]) -> Result<()> {
        self.entity_with_label(kind, None, None, parts)
    }

    /// Like [`entity`](Self::entity), with a container-defined [label](Entity::label).
    pub fn entity_labelled(
        &mut self,
        kind: SegmentKind,
        label: impl Into<Cow<'static, str>>,
        parts: &[(SegmentId, ByteRange)],
    ) -> Result<()> {
        self.entity_with_label(kind, Some(label.into()), None, parts)
    }

    fn entity_with_label(
        &mut self,
        kind: SegmentKind,
        label: Option<Cow<'static, str>>,
        jumbf_type: Option<[u8; 16]>,
        parts: &[(SegmentId, ByteRange)],
    ) -> Result<()> {
        let mut it = parts.iter();
        let Some(&(sid, range)) = it.next() else {
            return Err(Error::InvalidFormat(
                "entity needs at least one part".into(),
            ));
        };
        let first = self.make_part(sid, range)?;
        let mut rest = Vec::new();
        for &(sid, range) in it {
            rest.push(self.make_part(sid, range)?);
        }
        self.entities.push(Entity {
            kind,
            label,
            jumbf_type,
            first,
            rest,
        });
        Ok(())
    }

    fn make_part(&self, segment: SegmentId, range: ByteRange) -> Result<Part> {
        let seg = self.segment(segment)?;
        if !seg.extent().contains(&range) {
            return Err(Error::InvalidSegment {
                offset: range.offset(),
                reason: "entity part lies outside its segment".into(),
            });
        }
        Ok(Part { range, segment })
    }

    /// Finish. The segments must cover the whole asset.
    pub fn finish(self) -> Result<Structure> {
        if self.cursor != self.total_size {
            return Err(Error::InvalidSegment {
                offset: self.cursor,
                reason: format!(
                    "segments cover {} of {} bytes",
                    self.cursor, self.total_size
                ),
            });
        }
        Ok(Structure {
            handler: self.handler,
            media_type: self.media_type,
            total_size: self.total_size,
            segments: self.segments,
            entities: self.entities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handler that only exists to own structures in these tests.
    #[derive(Debug)]
    struct TestHandler;

    impl ContainerHandler for TestHandler {
        fn formats(&self) -> &[&'static str] {
            &["test"]
        }
        fn parse(self: Arc<Self>, _: &mut dyn ReadSeek) -> Result<Structure> {
            Err(Error::UnsupportedFormat)
        }
        fn write(
            &self,
            _: &Structure,
            _: &mut dyn ReadSeek,
            _: &mut dyn ReadWriteSeek,
            _: &crate::Updates,
            _: &mut dyn crate::Sink,
        ) -> Result<Structure> {
            Err(Error::UnsupportedFormat)
        }
    }

    fn builder(total: u64) -> StructureBuilder {
        StructureBuilder::new(Arc::new(TestHandler), "application/x-test", total)
    }

    #[test]
    fn tiles_exactly() {
        let mut b = builder(30);
        b.push_span(SegmentKind::Header, Name::None, 10, 0, 0)
            .unwrap();
        let id = b
            .push_span(SegmentKind::Xmp, Name::FourCC(*b"iTXt"), 8, 8, 4)
            .unwrap();
        b.entity_of_segments(SegmentKind::Xmp, &[id]).unwrap();
        let s = b.finish().unwrap();
        let covered: u64 = s.regions().map(|r| r.range.len()).sum();
        assert_eq!(covered, 30);
        let e = s.require_entity(SegmentKind::Xmp).unwrap();
        assert_eq!(e.len(), 8);
        assert!(s.entity(SegmentKind::Jumbf).unwrap().is_none());
        assert!(matches!(
            s.require_entity(SegmentKind::Jumbf),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn rejects_gap_overrun_and_short_cover() {
        let mut b = builder(20);
        let gap = Segment::new(
            ByteRange::new(5, 5).unwrap(),
            0,
            0,
            SegmentKind::Other,
            Name::None,
        )
        .unwrap();
        assert!(b.push(gap).is_err());
        assert!(b
            .push_span(SegmentKind::Other, Name::None, 0, 21, 0)
            .is_err());
        b.push_span(SegmentKind::Other, Name::None, 0, 10, 0)
            .unwrap();
        assert!(b.finish().is_err());
    }

    #[test]
    fn ambiguous_entity() {
        let mut b = builder(8);
        let a = b
            .push_span(SegmentKind::Jumbf, Name::None, 0, 4, 0)
            .unwrap();
        let c = b
            .push_span(SegmentKind::Jumbf, Name::None, 0, 4, 0)
            .unwrap();
        b.entity_of_segments(SegmentKind::Jumbf, &[a]).unwrap();
        b.entity_of_segments(SegmentKind::Jumbf, &[c]).unwrap();
        let s = b.finish().unwrap();
        assert!(matches!(
            s.entity(SegmentKind::Jumbf),
            Err(Error::Ambiguous { count: 2, .. })
        ));
    }

    #[test]
    fn part_must_lie_in_segment() {
        let mut b = builder(10);
        let a = b.push_span(SegmentKind::Xmp, Name::None, 0, 5, 0).unwrap();
        let _ = b
            .push_span(SegmentKind::Other, Name::None, 0, 5, 0)
            .unwrap();
        let outside = ByteRange::new(4, 3).unwrap();
        assert!(b.entity(SegmentKind::Xmp, &[(a, outside)]).is_err());
    }

    #[test]
    fn read_range_is_bounded() {
        let mut b = builder(8);
        b.push_span(SegmentKind::Other, Name::None, 0, 8, 0)
            .unwrap();
        let s = b.finish().unwrap();
        let mut src = std::io::Cursor::new(vec![1u8, 2, 3, 4, 5, 6, 7, 8]);
        let got = s
            .read_range(&mut src, ByteRange::new(2, 3).unwrap())
            .unwrap();
        assert_eq!(got, vec![3, 4, 5]);
        assert!(s
            .read_range(&mut src, ByteRange::new(6, 3).unwrap())
            .is_err());
    }
}
