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

//! RIFF (WebP, WAV, AVI) on the structure model.
//!
//! A RIFF file is `RIFF size form` followed by chunks, each `id size data [pad]`
//! with little-endian 32-bit sizes and a zero pad byte after odd-sized data. The
//! form type says what the file is, so no format hint is needed: `WEBP` is
//! `image/webp`, `WAVE` is `audio/wav`, `AVI ` is `video/avi`.
//!
//! # Segments
//!
//! A top-level `RIFF` chunk (the first, and any `AVIX` extension that follows it in
//! a large AVI) is a 12-byte header segment with no payload; its children follow
//! as sibling segments. Every other chunk is one segment: the 8-byte `id size` is
//! the header, the data is the payload and the pad byte, if any, is the trailer.
//! `LIST` chunks are not entered: nothing C2PA needs lives inside them.
//!
//! The `C2PA` chunk is a [`SegmentKind::Jumbf`] entity and the `XMP ` chunk a
//! [`SegmentKind::Xmp`] entity, both only when they are children of the first
//! `RIFF` chunk. In WebP the `EXIF` chunk is a [`SegmentKind::Exif`] entity. WAV and
//! AVI keep their metadata in `LIST INFO` and similar, which is not EXIF.
//!
//! # Writing
//!
//! `set_jumbf` and `set_xmp` replace the existing chunk in place, or append one
//! after the last child of the first `RIFF` chunk (XMP before C2PA). Only that
//! chunk's size field changes; any `AVIX` chunks are copied as they are. A size
//! that no longer fits 32 bits is an error. For WebP, XMP needs the XMP flag in the
//! `VP8X` chunk: it is set (or cleared when the XMP is removed), and a `VP8X` chunk
//! is created from the `VP8` or `VP8L` image size if the file has none.

use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, FormatSpec};
use crate::plan::WritePlan;
use crate::segment::SegmentKind;
use crate::segment::{Name, Role, Segment};
use crate::sink::Sink;
use crate::structure::{SegmentId, Structure, StructureBuilder};
use crate::task::{BoxFuture, Io, Task};
use crate::{MetadataUpdate, Updates};
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, OnceLock};

const RIFF: [u8; 4] = *b"RIFF";
const LIST: [u8; 4] = *b"LIST";
const MOVI: [u8; 4] = *b"movi";
const C2PA: [u8; 4] = *b"C2PA";
const XMP: [u8; 4] = *b"XMP ";
const EXIF: [u8; 4] = *b"EXIF";
const DATA: [u8; 4] = *b"data";
const VP8X: [u8; 4] = *b"VP8X";
const VP8: [u8; 4] = *b"VP8 ";
const VP8L: [u8; 4] = *b"VP8L";
const ALPH: [u8; 4] = *b"ALPH";
const ANMF: [u8; 4] = *b"ANMF";

/// Form types, the four bytes after the size of a `RIFF` chunk.
const FORM_WEBP: [u8; 4] = *b"WEBP";
const FORM_WAVE: [u8; 4] = *b"WAVE";
const FORM_AVI: [u8; 4] = *b"AVI ";

/// Flag in the first four (little-endian) bytes of `VP8X` data.
const VP8X_XMP_FLAG: u32 = 4;
/// `VP8X` data is flags(4), canvas width - 1 (3), canvas height - 1 (3).
const VP8X_LEN: u64 = 10;

/// Form types handled, and the media type each means.
fn media_type_of(form: &[u8]) -> Option<&'static str> {
    match <[u8; 4]>::try_from(form).ok()? {
        FORM_WEBP => Some("image/webp"),
        FORM_WAVE => Some("audio/wav"),
        FORM_AVI => Some("video/avi"),
        _ => None,
    }
}

/// The RIFF container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct RiffHandler;

/// The shared RIFF handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    static H: OnceLock<Arc<RiffHandler>> = OnceLock::new();
    H.get_or_init(|| Arc::new(RiffHandler)).clone()
}

/// The RIFF container's [`ContainerHandler::id`].
pub const ID: &str = "riff";

impl ContainerHandler for RiffHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[
            FormatSpec::new("image/webp", "webp").media_type_aliases(&["image/x-webp"]),
            FormatSpec::new("audio/wav", "wav").media_type_aliases(&[
                "audio/wave",
                "audio/x-wav",
                "audio/vnd.wave",
            ]),
            FormatSpec::new("video/avi", "avi").media_type_aliases(&[
                "application/x-troff-msvideo",
                "video/msvideo",
                "video/x-msvideo",
            ]),
        ];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.len() >= 12 && header.starts_with(&RIFF) && media_type_of(&header[8..12]).is_some()
    }

    fn parse_async(self: Arc<Self>, io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(parse_with(self, io))
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        src_structure: &'f Structure,
        mut io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move { plan(src_structure, &mut io, updates).await })
    }
}

/// Parse a RIFF file into a validated structure.
pub fn parse<R: Read + Seek>(src: &mut R) -> Result<Structure> {
    let total = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    Task::new(total, |io| parse_with(handler(), io)).run(src)
}

fn bad(offset: u64, reason: &str) -> Error {
    Error::InvalidSegment {
        offset,
        reason: reason.into(),
    }
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

async fn parse_with(handler: Arc<dyn ContainerHandler>, mut io: Io) -> Result<Structure> {
    let total = io.len();
    if total < 12 {
        return Err(Error::InvalidFormat("RIFF too small".into()));
    }
    let mut head = [0u8; 12];
    io.read_at(0, &mut head).await?;
    if head[..4] != RIFF {
        return Err(Error::InvalidFormat("Not a RIFF file".into()));
    }
    let media = media_type_of(&head[8..12]).ok_or(Error::UnsupportedFormat)?;
    let webp = head[8..12] == FORM_WEBP;

    let mut b = StructureBuilder::new(handler, media, total);
    let mut at = 0u64;
    let mut first = true;
    while at < total {
        let remaining = total - at;
        if remaining < 8 {
            // Too short to be a chunk: keep it as unnamed trailing data.
            b.push_span(SegmentKind::Other, Name::None, 0, remaining, 0)?;
            break;
        }
        let mut h = [0u8; 8];
        io.read_at(at, &mut h).await?;
        let id = [h[0], h[1], h[2], h[3]];
        let size = le32(&h[4..8]) as u64;

        if id == RIFF {
            if remaining < 12 {
                return Err(bad(at, "RIFF chunk has no form type"));
            }
            let end = at + 8 + size;
            if size < 4 {
                return Err(bad(at, "RIFF chunk too small for its form type"));
            }
            if end > total {
                return Err(bad(at, "RIFF chunk declared size exceeds file size"));
            }
            b.push_span(SegmentKind::Header, Name::FourCC(RIFF), 12, 0, 0)?;
            parse_children(&mut b, &mut io, at + 12, end, first, webp).await?;
            at = end;
            first = false;
        } else {
            // Something other than a RIFF chunk at top level: one opaque chunk.
            let next = parse_chunk(&mut b, &mut io, at, total, false, false).await?;
            at = next;
        }
    }
    b.finish()
}

async fn parse_children(
    b: &mut StructureBuilder,
    io: &mut Io,
    start: u64,
    end: u64,
    first: bool,
    webp: bool,
) -> Result<()> {
    let mut at = start;
    while at < end {
        at = parse_chunk(b, io, at, end, first, webp).await?;
    }
    Ok(())
}

/// One chunk starting at `at` inside a region ending at `end`. Returns the offset
/// after it (and its pad byte, when present).
async fn parse_chunk(
    b: &mut StructureBuilder,
    io: &mut Io,
    at: u64,
    end: u64,
    classify: bool,
    webp: bool,
) -> Result<u64> {
    if end - at < 8 {
        return Err(bad(at, "truncated chunk header"));
    }
    let mut h = [0u8; 8];
    io.read_at(at, &mut h).await?;
    let id = [h[0], h[1], h[2], h[3]];
    let size = le32(&h[4..8]) as u64;
    let data_end = at + 8 + size;
    if data_end > end {
        return Err(bad(at, "chunk extends past the end of its container"));
    }
    // An odd-sized chunk has a pad byte unless it is the very last thing.
    let pad = u16::from(size & 1 == 1 && data_end < end);

    let kind = if !classify {
        SegmentKind::Other
    } else {
        match id {
            C2PA => SegmentKind::Jumbf,
            XMP => SegmentKind::Xmp,
            EXIF if webp => SegmentKind::Exif,
            DATA | VP8 | VP8L | ALPH | ANMF => SegmentKind::ImageData,
            LIST if size >= 4 => {
                let mut t = [0u8; 4];
                io.read_at(at + 8, &mut t).await?;
                if t == MOVI {
                    SegmentKind::ImageData
                } else {
                    SegmentKind::Other
                }
            }
            _ => SegmentKind::Other,
        }
    };
    let sid = b.push_span(kind, Name::FourCC(id), 8, size, pad)?;
    if matches!(
        kind,
        SegmentKind::Jumbf | SegmentKind::Xmp | SegmentKind::Exif
    ) {
        b.entity_of_segments(kind, &[sid])?;
    }
    Ok(data_end + pad as u64)
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

enum Op<'a> {
    /// Copy a source segment unchanged.
    Copy(SegmentId),
    /// The first `RIFF` header, with its new size.
    Riff { size: u32, form: [u8; 4] },
    /// A `VP8X` chunk with new flags.
    Flags { id: SegmentId, flags: u32 },
    /// A new chunk (with its pad byte, when odd).
    Chunk {
        kind: SegmentKind,
        id: [u8; 4],
        payload: Cow<'a, [u8]>,
    },
    /// The wrapped op followed by the pad byte its chunk lacked because it was last.
    WithPad(Box<Op<'a>>),
}

struct Dims {
    kind: SegmentKind,
    name: Name,
    header_len: u32,
    payload_len: u64,
    trailer_len: u16,
}

impl Dims {
    fn len(&self) -> u64 {
        self.header_len as u64 + self.payload_len + self.trailer_len as u64
    }
}

fn dims(op: &Op<'_>, src: &Structure) -> Result<Dims> {
    Ok(match op {
        Op::Copy(id) | Op::Flags { id, .. } => {
            let s = src.segment(*id)?;
            Dims {
                kind: s.kind(),
                name: s.name(),
                header_len: s.header().len() as u32,
                payload_len: s.payload().len(),
                trailer_len: s.trailer().len() as u16,
            }
        }
        Op::Riff { .. } => Dims {
            kind: SegmentKind::Header,
            name: Name::FourCC(RIFF),
            header_len: 12,
            payload_len: 0,
            trailer_len: 0,
        },
        Op::Chunk { kind, id, payload } => Dims {
            kind: *kind,
            name: Name::FourCC(*id),
            header_len: 8,
            payload_len: payload.len() as u64,
            trailer_len: (payload.len() & 1) as u16,
        },
        Op::WithPad(inner) => {
            let mut d = dims(inner, src)?;
            d.trailer_len += 1;
            d
        }
    })
}

fn new_chunk(kind: SegmentKind, id: [u8; 4], payload: Cow<'_, [u8]>) -> Result<Op<'_>> {
    if payload.len() as u64 > u32::MAX as u64 {
        return Err(Error::LimitExceeded {
            what: "RIFF chunk size",
            limit: u32::MAX as u64,
        });
    }
    Ok(Op::Chunk { kind, id, payload })
}

/// Canvas size of a WebP from its `VP8L` or `VP8 ` chunk.
async fn webp_size(io: &mut Io, segs: &[Segment]) -> Result<(u32, u32)> {
    let find = |id: [u8; 4]| segs.iter().find(|s| s.name() == Name::FourCC(id));
    if let Some(s) = find(VP8L) {
        let mut d = [0u8; 5];
        if s.payload().len() < d.len() as u64 {
            return Err(bad(s.extent().offset(), "VP8L chunk too small"));
        }
        io.read_at(s.payload().offset(), &mut d).await?;
        let first = u16::from_le_bytes([d[1], d[2]]) as u32;
        let second = u16::from_le_bytes([d[3], d[4]]) as u32;
        let width = 1 + (first & 0x3fff);
        let height = 1 + (((second & 0xfff) << 2) | ((first & 0xc000) >> 14));
        return Ok((width, height));
    }
    if let Some(s) = find(VP8) {
        let mut d = [0u8; 10];
        if s.payload().len() < d.len() as u64 {
            return Err(bad(s.extent().offset(), "VP8 chunk too small"));
        }
        io.read_at(s.payload().offset(), &mut d).await?;
        let width = (u16::from_le_bytes([d[6], d[7]]) & 0x3fff) as u32;
        let height = (u16::from_le_bytes([d[8], d[9]]) & 0x3fff) as u32;
        return Ok((width, height));
    }
    Err(Error::InvalidFormat(
        "WebP has no VP8 or VP8L chunk to take its size from".into(),
    ))
}

fn new_vp8x<'a>(width: u32, height: u32, flags: u32) -> Result<Op<'a>> {
    let (w, h) = match (width.checked_sub(1), height.checked_sub(1)) {
        (Some(w), Some(h)) if w < 1 << 24 && h < 1 << 24 => (w, h),
        _ => return Err(Error::InvalidFormat("WebP size does not fit VP8X".into())),
    };
    let mut d = Vec::with_capacity(VP8X_LEN as usize);
    d.extend_from_slice(&flags.to_le_bytes());
    d.extend_from_slice(&w.to_le_bytes()[..3]);
    d.extend_from_slice(&h.to_le_bytes()[..3]);
    new_chunk(SegmentKind::Other, VP8X, Cow::Owned(d))
}

/// Whether `op` is a source chunk of odd size that was last and so has no pad.
fn lacks_pad(op: &Op<'_>, src: &Structure) -> Result<bool> {
    Ok(match op {
        Op::Copy(id) | Op::Flags { id, .. } => {
            let s = src.segment(*id)?;
            s.payload().len() & 1 == 1 && s.trailer().is_empty()
        }
        _ => false,
    })
}

/// Decide what the output contains: the single source of truth for both the
/// destination structure and the bytes written.
async fn plan<'a>(src: &Structure, io: &mut Io, updates: &'a Updates) -> Result<WritePlan<'a>> {
    let segs = src.segments();
    let mut head = [0u8; 12];
    io.read_at(0, &mut head).await?;
    let riff_end = 8 + le32(&head[4..8]) as u64;
    let form = [head[8], head[9], head[10], head[11]];
    // Children of the first RIFF chunk are segments 1.. up to where it ends.
    let kids_end = segs
        .iter()
        .position(|s| s.extent().offset() >= riff_end)
        .unwrap_or(segs.len());
    let webp = src.media_type() == "image/webp";

    let mut ops: Vec<Op<'a>> = Vec::with_capacity(segs.len() + 3);
    ops.push(Op::Riff { size: 0, form });
    let (mut xmp_done, mut jumbf_done, mut saw_vp8x) = (false, false, false);

    for (i, seg) in segs.iter().enumerate().take(kids_end).skip(1) {
        let id = SegmentId::from_index(i)?;
        match seg.kind() {
            SegmentKind::Xmp => match &updates.xmp {
                MetadataUpdate::Keep => {
                    ops.push(Op::Copy(id));
                    xmp_done = true;
                }
                MetadataUpdate::Set(d) if !xmp_done => {
                    ops.push(new_chunk(SegmentKind::Xmp, XMP, Cow::Borrowed(d))?);
                    xmp_done = true;
                }
                _ => {}
            },
            SegmentKind::Jumbf => match &updates.jumbf {
                MetadataUpdate::Keep => {
                    ops.push(Op::Copy(id));
                    jumbf_done = true;
                }
                MetadataUpdate::Set(d) if !jumbf_done => {
                    ops.push(new_chunk(SegmentKind::Jumbf, C2PA, Cow::Borrowed(d))?);
                    jumbf_done = true;
                }
                _ => {}
            },
            _ if webp && seg.name() == Name::FourCC(VP8X) => {
                saw_vp8x = true;
                let want = match &updates.xmp {
                    MetadataUpdate::Keep => None,
                    MetadataUpdate::Set(_) => Some(true),
                    MetadataUpdate::Remove => Some(false),
                };
                let mut op = Op::Copy(id);
                if let Some(want) = want {
                    if seg.payload().len() < 4 {
                        return Err(bad(seg.extent().offset(), "VP8X chunk too small"));
                    }
                    let mut f = [0u8; 4];
                    io.read_at(seg.payload().offset(), &mut f).await?;
                    let flags = u32::from_le_bytes(f);
                    let new = if want {
                        flags | VP8X_XMP_FLAG
                    } else {
                        flags & !VP8X_XMP_FLAG
                    };
                    if new != flags {
                        op = Op::Flags { id, flags: new };
                    }
                }
                ops.push(op);
            }
            _ => ops.push(Op::Copy(id)),
        }
    }

    // What was not replaced in place goes at the end, XMP first, with the pad byte
    // the last source chunk may be missing.
    let mut tail: Vec<Op<'a>> = Vec::new();
    if !xmp_done {
        if let MetadataUpdate::Set(d) = &updates.xmp {
            tail.push(new_chunk(SegmentKind::Xmp, XMP, Cow::Borrowed(d))?);
        }
    }
    if !jumbf_done {
        if let MetadataUpdate::Set(d) = &updates.jumbf {
            tail.push(new_chunk(SegmentKind::Jumbf, C2PA, Cow::Borrowed(d))?);
        }
    }
    if !tail.is_empty() {
        if let Some(last) = ops.pop() {
            ops.push(if lacks_pad(&last, src)? {
                Op::WithPad(Box::new(last))
            } else {
                last
            });
        }
        ops.extend(tail);
    }

    // WebP needs VP8X to say the file has XMP.
    if webp && matches!(updates.xmp, MetadataUpdate::Set(_)) && !saw_vp8x {
        let (w, h) = webp_size(io, &segs[1..kids_end]).await?;
        ops.insert(1, new_vp8x(w, h, VP8X_XMP_FLAG)?);
    }

    // Size of the first RIFF chunk.
    let mut kids = 4u64; // form type
    for op in &ops[1..] {
        kids = kids
            .checked_add(dims(op, src)?.len())
            .ok_or(Error::LimitExceeded {
                what: "output size",
                limit: u64::MAX,
            })?;
    }
    let size = u32::try_from(kids).map_err(|_| Error::LimitExceeded {
        what: "RIFF chunk size",
        limit: u32::MAX as u64,
    })?;
    ops[0] = Op::Riff { size, form };

    // Whatever follows the first RIFF chunk (AVIX, ...) is copied.
    for (i, _) in segs.iter().enumerate().skip(kids_end) {
        ops.push(Op::Copy(SegmentId::from_index(i)?));
    }

    // Destination structure.
    let mut all = Vec::with_capacity(ops.len());
    let mut total = 0u64;
    for op in &ops {
        let d = dims(op, src)?;
        total = total.checked_add(d.len()).ok_or(Error::LimitExceeded {
            what: "output size",
            limit: u64::MAX,
        })?;
        all.push(d);
    }
    let mut b = StructureBuilder::new(src.handler().clone(), src.media_type().to_owned(), total);
    for d in &all {
        let id = b.push_span(d.kind, d.name, d.header_len, d.payload_len, d.trailer_len)?;
        if matches!(
            d.kind,
            SegmentKind::Xmp | SegmentKind::Jumbf | SegmentKind::Exif
        ) {
            b.entity_of_segments(d.kind, &[id])?;
        }
    }
    let mut plan = WritePlan::new(b.finish()?);
    for (i, op) in ops.iter().enumerate() {
        add_steps(&mut plan, src, SegmentId::from_index(i)?, op)?;
    }
    Ok(plan)
}

/// The steps that write `op` as the output segment `dest`.
fn add_steps<'a>(
    plan: &mut WritePlan<'a>,
    src: &Structure,
    dest: SegmentId,
    op: &Op<'a>,
) -> Result<()> {
    match op {
        Op::Copy(sid) => {
            plan.copy_segment(dest, src.segment(*sid)?);
        }
        Op::Riff { size, form } => {
            let mut h = [0u8; 12];
            h[..4].copy_from_slice(&RIFF);
            h[4..8].copy_from_slice(&size.to_le_bytes());
            h[8..].copy_from_slice(form);
            plan.put(dest, Role::Header, h.to_vec());
        }
        Op::Flags { id, flags } => {
            let s = src.segment(*id)?;
            plan.copy(dest, Role::Header, s.header());
            plan.put(dest, Role::Payload, flags.to_le_bytes().to_vec());
            plan.copy(
                dest,
                Role::Payload,
                s.payload().sub(4, s.payload().len() - 4)?,
            );
            plan.copy(dest, Role::Trailer, s.trailer());
        }
        Op::Chunk { id, payload, .. } => {
            let mut h = [0u8; 8];
            h[..4].copy_from_slice(id);
            h[4..].copy_from_slice(&(payload.len() as u32).to_le_bytes()); // bounded by new_chunk
            plan.put(dest, Role::Header, h.to_vec());
            plan.put(dest, Role::Payload, payload.clone());
            if payload.len() & 1 == 1 {
                plan.put(dest, Role::Trailer, vec![0]);
            }
        }
        Op::WithPad(inner) => {
            add_steps(plan, src, dest, inner)?;
            plan.put(dest, Role::Trailer, vec![0]);
        }
    }
    Ok(())
}

/// Write `src` with `updates` applied, delivering every output byte to `sink`.
///
/// `out` is written from its current start; offsets reported to the sink are
/// relative to the first byte written. Returns the structure of the output.
pub fn write<R, W, S>(
    src_structure: &Structure,
    src: &mut R,
    out: &mut W,
    updates: &Updates,
    sink: &mut S,
) -> Result<Structure>
where
    R: Read + Seek + ?Sized,
    W: Write + ?Sized,
    S: Sink + ?Sized,
{
    let plan = Task::new(src_structure.total_size(), |mut io| async move {
        plan(src_structure, &mut io, updates).await
    })
    .run(&mut *src)?;
    plan.execute(src, out, sink)?;
    Ok(plan.into_structure())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chunk, NullSink};
    use std::io::Cursor;

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn riff(form: &[u8; 4], kids: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = kids.concat();
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        v.extend_from_slice(form);
        v.extend_from_slice(&body);
        v
    }

    fn wav(extra: &[Vec<u8>]) -> Vec<u8> {
        let mut kids = vec![chunk(b"fmt ", &[1; 16]), chunk(b"data", b"abcde")];
        kids.extend_from_slice(extra);
        riff(b"WAVE", &kids)
    }

    fn rewrite(src: &[u8], u: &Updates) -> Result<Vec<u8>> {
        let s = parse(&mut Cursor::new(src))?;
        let mut out = Cursor::new(Vec::new());
        let dest = s
            .handler()
            .write(&s, &mut Cursor::new(src), &mut out, u, &mut NullSink)?;
        let out = out.into_inner();
        let again = parse(&mut Cursor::new(&out))?;
        assert_eq!(dest.segments(), again.segments());
        assert_eq!(dest.total_size(), out.len() as u64);
        // The RIFF size always matches the file.
        assert_eq!(le32(&out[4..8]) as usize, out.len() - 8);
        Ok(out)
    }

    fn payload(bytes: &[u8], kind: SegmentKind) -> Option<Vec<u8>> {
        let s = parse(&mut Cursor::new(bytes)).unwrap();
        let found = s
            .entities(kind)
            .next()
            .map(|e| s.read_entity(&mut Cursor::new(bytes), e).unwrap());
        found
    }

    fn vp8x_flags(bytes: &[u8]) -> Option<u32> {
        let s = parse(&mut Cursor::new(bytes)).unwrap();
        let seg = s
            .segments()
            .iter()
            .find(|g| g.name() == Name::FourCC(VP8X))?;
        let at = seg.payload().offset() as usize;
        Some(le32(&bytes[at..at + 4]))
    }

    #[test]
    fn media_types_follow_the_form_type() {
        for (form, mt) in [
            (b"WEBP", "image/webp"),
            (b"WAVE", "audio/wav"),
            (b"AVI ", "video/avi"),
        ] {
            let f = riff(form, &[chunk(b"junk", b"x")]);
            let s = parse(&mut Cursor::new(&f)).unwrap();
            assert_eq!(s.media_type(), mt);
            assert!(RiffHandler.detect(&f));
        }
        let other = riff(b"ACON", &[]);
        assert!(!RiffHandler.detect(&other));
        assert!(matches!(
            parse(&mut Cursor::new(&other)),
            Err(Error::UnsupportedFormat)
        ));
    }

    #[test]
    fn tiles_and_finds_entities() {
        let f = wav(&[chunk(b"XMP ", b"<x/>"), chunk(b"C2PA", b"STORE")]);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let covered: u64 = s.regions().map(|r| r.range.len()).sum();
        assert_eq!(covered, f.len() as u64);
        // RIFF header, fmt, data, XMP, C2PA.
        assert_eq!(s.segments().len(), 5);
        assert_eq!(s.segments()[0].header().len(), 12);
        // Odd data carries its pad as the trailer.
        assert_eq!(s.segments()[2].payload().len(), 5);
        assert_eq!(s.segments()[2].trailer().len(), 1);
        assert_eq!(s.segments()[2].kind(), SegmentKind::ImageData);
        assert_eq!(payload(&f, SegmentKind::Jumbf).unwrap(), b"STORE");
        assert_eq!(payload(&f, SegmentKind::Xmp).unwrap(), b"<x/>");
    }

    #[test]
    fn exif_is_an_entity_in_webp_only() {
        let exif = chunk(b"EXIF", b"II*\0\x08\0\0\0");
        let webp = riff(
            b"WEBP",
            &[chunk(b"VP8L", &[0x2f, 0, 0, 0, 0, 0]), exif.clone()],
        );
        let s = parse(&mut Cursor::new(&webp)).unwrap();
        assert_eq!(
            payload(&webp, SegmentKind::Exif).unwrap(),
            b"II*\0\x08\0\0\0"
        );
        assert_eq!(rewrite(&webp, &Updates::new()).unwrap(), webp);
        assert_eq!(s.entities(SegmentKind::Exif).count(), 1);

        let wav = wav(&[exif]);
        assert!(payload(&wav, SegmentKind::Exif).is_none());
    }

    #[test]
    fn keep_is_byte_identical() {
        let f = wav(&[chunk(b"XMP ", b"<x/>"), chunk(b"C2PA", b"STORE")]);
        assert_eq!(rewrite(&f, &Updates::new()).unwrap(), f);
    }

    #[test]
    fn a_manifest_is_appended_replaced_and_removed() {
        let f = wav(&[]);
        let one = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
        assert_eq!(payload(&one, SegmentKind::Jumbf).unwrap(), b"STORE");
        // Appended at the end, after everything that was there.
        let s = parse(&mut Cursor::new(&one)).unwrap();
        assert_eq!(s.segments().last().unwrap().name(), Name::FourCC(C2PA));

        // Replaced in place, in any size.
        let longer = rewrite(&one, &Updates::new().set_jumbf(vec![7u8; 100])).unwrap();
        assert_eq!(
            payload(&longer, SegmentKind::Jumbf).unwrap(),
            vec![7u8; 100]
        );
        let shorter = rewrite(&one, &Updates::new().set_jumbf(b"S".to_vec())).unwrap();
        assert_eq!(payload(&shorter, SegmentKind::Jumbf).unwrap(), b"S");

        // Removing restores the original.
        assert_eq!(rewrite(&one, &Updates::new().remove_jumbf()).unwrap(), f);
    }

    #[test]
    fn xmp_goes_before_the_manifest() {
        let f = wav(&[]);
        let u = Updates::new()
            .set_jumbf(b"STORE".to_vec())
            .set_xmp(b"<x/>".to_vec());
        let out = rewrite(&f, &u).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        let names: Vec<_> = s.segments().iter().map(|g| g.name()).collect();
        assert_eq!(
            &names[names.len() - 2..],
            &[Name::FourCC(XMP), Name::FourCC(C2PA)]
        );
        assert_eq!(payload(&out, SegmentKind::Xmp).unwrap(), b"<x/>");
        assert_eq!(rewrite(&out, &Updates::remove_all()).unwrap(), f);
    }

    #[test]
    fn a_missing_final_pad_is_supplied_before_appending() {
        // An odd last chunk with no pad byte, as some writers leave it.
        let mut f = wav(&[chunk(b"junk", b"abc")]);
        f.pop();
        let n = f.len() as u32 - 8;
        f[4..8].copy_from_slice(&n.to_le_bytes());
        let s = parse(&mut Cursor::new(&f)).unwrap();
        assert_eq!(s.segments().last().unwrap().trailer().len(), 0);
        assert_eq!(rewrite(&f, &Updates::new()).unwrap(), f);

        let out = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
        assert_eq!(payload(&out, SegmentKind::Jumbf).unwrap(), b"STORE");
        // The earlier chunk got its pad, so the file is aligned and re-parses.
        let s = parse(&mut Cursor::new(&out)).unwrap();
        assert!(s.segments().iter().all(|g| g.extent().offset() % 2 == 0));
    }

    #[test]
    fn the_plan_tiles_its_structure_and_borrows_the_new_metadata() {
        let mut f = wav(&[chunk(b"junk", b"abc")]);
        f.pop();
        let n = f.len() as u32 - 8;
        f[4..8].copy_from_slice(&n.to_le_bytes());
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let u = Updates::new().set_xmp(b"<x/>".to_vec());
        let plan = s.write_plan(&mut Cursor::new(&f), &u).unwrap();
        crate::plan::validate(&plan, f.len() as u64).unwrap();
        // The new payload is not copied into the plan.
        assert!(plan.steps().iter().any(|st| matches!(
            st,
            crate::Step::Put { data: Cow::Borrowed(d), .. } if *d == b"<x/>"
        )));
    }

    #[test]
    fn webp_xmp_maintains_the_vp8x_flag() {
        // VP8L: signature, then 14-bit width-1 and height-1 packed: 3 x 2
        // (width - 1 = 2 in the low bits; height - 1 = 1 split across the words).
        let vp8l = chunk(b"VP8L", &[0x2f, 0x02, 0x40, 0, 0, 0]);
        let bare = riff(b"WEBP", std::slice::from_ref(&vp8l));

        // No VP8X: one is created first, from the image size.
        let out = rewrite(&bare, &Updates::new().set_xmp(b"<x/>".to_vec())).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        assert_eq!(s.segments()[1].name(), Name::FourCC(VP8X));
        assert_eq!(vp8x_flags(&out), Some(VP8X_XMP_FLAG));
        let at = s.segments()[1].payload().offset() as usize;
        assert_eq!(&out[at + 4..at + 7], &[2, 0, 0]); // width - 1
        assert_eq!(&out[at + 7..at + 10], &[1, 0, 0]); // height - 1

        // Removing the XMP clears the flag but keeps the chunk.
        let back = rewrite(&out, &Updates::new().remove_xmp()).unwrap();
        assert_eq!(vp8x_flags(&back), Some(0));
        assert!(payload(&back, SegmentKind::Xmp).is_none());

        // An existing VP8X has the flag set, other flags untouched.
        let mut d = vec![0u8; 10];
        d[0] = 0x10; // alpha
        let with = riff(b"WEBP", &[chunk(b"VP8X", &d), vp8l]);
        let out = rewrite(&with, &Updates::new().set_xmp(b"<x/>".to_vec())).unwrap();
        assert_eq!(vp8x_flags(&out), Some(0x14));
        // A manifest alone leaves VP8X alone.
        let out = rewrite(&with, &Updates::new().set_jumbf(b"S".to_vec())).unwrap();
        assert_eq!(vp8x_flags(&out), Some(0x10));
    }

    #[test]
    fn webp_size_from_a_lossy_chunk_and_the_missing_case() {
        let mut d = vec![0u8; 10];
        d[6..8].copy_from_slice(&640u16.to_le_bytes());
        d[8..10].copy_from_slice(&480u16.to_le_bytes());
        let f = riff(b"WEBP", &[chunk(b"VP8 ", &d)]);
        let out = rewrite(&f, &Updates::new().set_xmp(b"<x/>".to_vec())).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        let at = s.segments()[1].payload().offset() as usize;
        assert_eq!(&out[at + 4..at + 7], &639u32.to_le_bytes()[..3]);
        assert_eq!(&out[at + 7..at + 10], &479u32.to_le_bytes()[..3]);

        let none = riff(b"WEBP", &[chunk(b"ICCP", b"x")]);
        assert!(rewrite(&none, &Updates::new().set_xmp(b"<x/>".to_vec())).is_err());
        // Not needed when no XMP is written.
        assert!(rewrite(&none, &Updates::new().set_jumbf(b"S".to_vec())).is_ok());
    }

    #[test]
    fn avix_chunks_hold_no_entities_and_keep_their_size() {
        let avix = riff(
            b"AVIX",
            &[chunk(b"C2PA", b"NOT MINE"), chunk(b"junk", b"zz")],
        );
        let mut f = riff(b"AVI ", &[chunk(b"hdrl", b"abcd")]);
        let first_len = f.len();
        f.extend_from_slice(&avix);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        assert!(s.entities(SegmentKind::Jumbf).next().is_none());

        let mut out = Cursor::new(Vec::new());
        s.handler()
            .write(
                &s,
                &mut Cursor::new(&f),
                &mut out,
                &Updates::new().set_jumbf(b"STORE".to_vec()),
                &mut NullSink,
            )
            .unwrap();
        let out = out.into_inner();
        let grown = 8 + 6; // C2PA header, "STORE" and its pad
        assert_eq!(le32(&out[4..8]) as usize, first_len - 8 + grown);
        assert_eq!(&out[first_len + grown..], &avix[..]);
        // The new store is the entity, not the C2PA chunk inside AVIX.
        assert_eq!(payload(&out, SegmentKind::Jumbf).unwrap(), b"STORE");
    }

    #[test]
    fn only_the_first_riff_size_changes_with_avix() {
        let avix = riff(b"AVIX", &[chunk(b"junk", b"zz")]);
        let mut f = riff(b"AVI ", &[chunk(b"hdrl", b"abcd")]);
        let first_len = f.len();
        f.extend_from_slice(&avix);

        let s = parse(&mut Cursor::new(&f)).unwrap();
        let mut out = Cursor::new(Vec::new());
        s.handler()
            .write(
                &s,
                &mut Cursor::new(&f),
                &mut out,
                &Updates::new().set_jumbf(b"STORE".to_vec()),
                &mut NullSink,
            )
            .unwrap();
        let out = out.into_inner();
        let grown = 8 + 6; // C2PA header, "STORE" and its pad
        assert_eq!(le32(&out[4..8]) as usize, first_len - 8 + grown);
        assert_eq!(&out[first_len + grown..], &avix[..]);
        assert_eq!(payload(&out, SegmentKind::Jumbf).unwrap(), b"STORE");
    }

    #[test]
    fn hostile_input_is_an_error() {
        // Chunk longer than the file.
        let mut f = wav(&[]);
        let at = f.len() - 14; // the data chunk header
        f[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&mut Cursor::new(&f)).is_err());

        // RIFF size larger than the file.
        let mut f = wav(&[]);
        f[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&mut Cursor::new(&f)).is_err());

        // RIFF size too small to hold a form type.
        let mut f = wav(&[]);
        f[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(parse(&mut Cursor::new(&f)).is_err());

        // A truncated chunk header inside the RIFF chunk.
        let mut f = riff(b"WAVE", &[]);
        f.extend_from_slice(&[1, 2, 3]);
        let n = f.len() as u32 - 8;
        f[4..8].copy_from_slice(&n.to_le_bytes());
        assert!(parse(&mut Cursor::new(&f)).is_err());

        // Too short, and not RIFF.
        assert!(parse(&mut Cursor::new(b"RIFF")).is_err());
        assert!(parse(&mut Cursor::new(b"RIFX\0\0\0\0WAVE")).is_err());
    }

    #[test]
    fn sink_sees_every_output_byte_once_in_order() {
        let f = wav(&[chunk(b"C2PA", b"STORE")]);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let mut next = 0u64;
        let mut seen = Vec::new();
        let mut sink = |c: Chunk<'_>| -> Result<()> {
            assert_eq!(c.offset, next);
            next += c.data.len() as u64;
            seen.extend_from_slice(c.data);
            Ok(())
        };
        let mut out = Cursor::new(Vec::new());
        s.handler()
            .write(
                &s,
                &mut Cursor::new(&f),
                &mut out,
                &Updates::new().set_xmp(b"<x/>".to_vec()),
                &mut sink,
            )
            .unwrap();
        assert_eq!(seen, out.into_inner());
    }

    #[test]
    fn a_placeholder_is_filled_in_place() {
        let f = wav(&[chunk(b"C2PA", &[0u8; 20])]);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let e = s.require_entity(SegmentKind::Jumbf).unwrap();
        let mut cur = Cursor::new(f.clone());
        s.patch_exact(&mut cur, e, &[9u8; 20]).unwrap();
        let out = cur.into_inner();
        assert_eq!(out.len(), f.len());
        assert_eq!(payload(&out, SegmentKind::Jumbf).unwrap(), vec![9u8; 20]);
    }
}
