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

//! PNG on the structure model.
//!
//! Each chunk is one segment: `length(4) type(4) [iTXt prefix]` is the header,
//! the chunk data is the payload, and the CRC is the trailer.

use crate::crc::Crc32;
use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, FormatSpec, ReadSeek, ReadWriteSeek};
use crate::plan::WritePlan;
use crate::segment::SegmentKind;
use crate::segment::{Name, Role, Segment};
use crate::sink::Sink;
use crate::structure::{SegmentId, Structure, StructureBuilder};
use crate::{MetadataUpdate, Updates};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, OnceLock};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const ITXT: [u8; 4] = *b"iTXt";
const CABX: [u8; 4] = *b"caBX";
const IEND: [u8; 4] = *b"IEND";
const IDAT: [u8; 4] = *b"IDAT";
const FCTL: [u8; 4] = *b"fcTL";
const EXIF: [u8; 4] = *b"eXIf";

/// `keyword NUL`, compression flag, compression method, empty language tag NUL,
/// empty translated keyword NUL.
const XMP_PREFIX_LEN: usize = 22;
const XMP_ITXT_PREFIX: &[u8; XMP_PREFIX_LEN] = b"XML:com.adobe.xmp\0\0\0\0\0";
const XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp\0";

/// Largest legal chunk data length (PNG spec: 2^31 - 1).
const MAX_CHUNK_LEN: u64 = 0x7FFF_FFFF;

/// How much of an iTXt chunk to look at when deciding whether it holds XMP.
const PEEK: usize = 256;

/// Length of the iTXt prefix if this chunk's data begins an uncompressed XMP
/// iTXt; `None` otherwise.
fn xmp_prefix_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < XMP_KEYWORD.len() + 2 || &buf[..XMP_KEYWORD.len()] != XMP_KEYWORD {
        return None;
    }
    let mut i = XMP_KEYWORD.len();
    if buf[i] != 0 {
        return None; // compressed: not a plain XMP packet
    }
    i += 2; // compression flag + method
    i += buf[i..].iter().position(|&b| b == 0)? + 1; // language tag
    i += buf[i..].iter().position(|&b| b == 0)? + 1; // translated keyword
    Some(i)
}

/// The PNG container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct PngHandler;

/// The shared PNG handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    static H: OnceLock<Arc<PngHandler>> = OnceLock::new();
    H.get_or_init(|| Arc::new(PngHandler)).clone()
}

/// The PNG container's [`ContainerHandler::id`].
pub const ID: &str = "png";

impl ContainerHandler for PngHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[FormatSpec::new("image/png", "png")];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.starts_with(SIGNATURE)
    }

    fn parse(self: Arc<Self>, mut src: &mut dyn ReadSeek) -> Result<Structure> {
        parse_with(self, &mut src)
    }

    fn write_plan<'a>(
        &self,
        src_structure: &Structure,
        _src: &mut dyn ReadSeek,
        updates: &'a Updates,
    ) -> Result<WritePlan<'a>> {
        plan(src_structure, updates)
    }

    fn compose(&self, kind: SegmentKind, data: &[u8]) -> Result<Vec<u8>> {
        let op = match kind {
            SegmentKind::Jumbf => new_chunk(kind, CABX, &[], data)?,
            SegmentKind::Xmp => new_chunk(kind, ITXT, XMP_ITXT_PREFIX, data)?,
            _ => return Err(Error::UnsupportedFormat),
        };
        let Op::New {
            ty,
            prefix,
            payload,
            ..
        } = op
        else {
            return Err(Error::UnsupportedFormat);
        };
        let mut out = Vec::with_capacity(12 + prefix.len() + payload.len());
        let (hdr, n) = chunk_header(&ty, prefix, payload.len());
        out.extend_from_slice(&hdr[..n]);
        out.extend_from_slice(payload);
        out.extend_from_slice(&chunk_crc(&ty, prefix, payload));
        Ok(out)
    }

    fn fix_trailer(
        &self,
        mut out: &mut dyn ReadWriteSeek,
        structure: &Structure,
        segment: SegmentId,
    ) -> Result<()> {
        fix_crc(&mut out, structure.segment(segment)?)
    }
}

/// Parse a PNG into a validated structure.
pub fn parse<R: Read + Seek>(src: &mut R) -> Result<Structure> {
    parse_with(handler(), src)
}

fn parse_with<R: Read + Seek>(
    handler: Arc<dyn ContainerHandler>,
    src: &mut R,
) -> Result<Structure> {
    let total = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    if total < SIGNATURE.len() as u64 + 12 {
        return Err(Error::InvalidFormat("PNG too small".into()));
    }
    let mut sig = [0u8; 8];
    src.read_exact(&mut sig)?;
    if &sig != SIGNATURE {
        return Err(Error::InvalidFormat("Not a PNG file".into()));
    }

    let mut b = StructureBuilder::new(handler, "image/png", total);
    // Named PNGh so a hasher listing every box sees the signature as the first one.
    b.push_span(SegmentKind::Header, Name::FourCC(*b"PNGh"), 8, 0, 0)?;

    let mut offset = 8u64;
    let mut found_iend = false;
    while !found_iend {
        let remaining = total - offset;
        if remaining < 12 {
            return Err(Error::InvalidFormat("PNG truncated: missing IEND".into()));
        }
        let mut hdr = [0u8; 8];
        src.read_exact(&mut hdr)?;
        let len = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let ty = [hdr[4], hdr[5], hdr[6], hdr[7]];
        if len > MAX_CHUNK_LEN {
            return Err(Error::InvalidSegment {
                offset,
                reason: format!("chunk length too large: {len}"),
            });
        }
        let chunk_len = 12 + len;
        if chunk_len > remaining {
            return Err(Error::InvalidSegment {
                offset,
                reason: "chunk extends past end of file".into(),
            });
        }

        let mut prefix = 0u64;
        let kind = match ty {
            IDAT => SegmentKind::ImageData,
            CABX => SegmentKind::Jumbf,
            EXIF => SegmentKind::Exif,
            ITXT => {
                let n = len.min(PEEK as u64) as usize;
                let mut peek = [0u8; PEEK];
                src.read_exact(&mut peek[..n])?;
                match xmp_prefix_len(&peek[..n]) {
                    Some(p) => {
                        prefix = p as u64;
                        SegmentKind::Xmp
                    }
                    None => SegmentKind::Other,
                }
            }
            _ => SegmentKind::Other,
        };

        let id = b.push_span(kind, Name::FourCC(ty), 8 + prefix as u32, len - prefix, 4)?;
        match kind {
            SegmentKind::Xmp | SegmentKind::Jumbf | SegmentKind::Exif => {
                b.entity_of_segments(kind, &[id])?
            }
            _ => {}
        }

        offset += chunk_len;
        src.seek(SeekFrom::Start(offset))?;
        found_iend = ty == IEND;
    }

    // The chunks through IEND are the structure; anything after is data beyond it.
    b.end_boxes()?;
    if offset < total {
        b.push_span(SegmentKind::Other, Name::None, 0, total - offset, 0)?;
    }
    b.finish()
}

/// One planned output chunk.
enum Op<'a> {
    /// Copy a source segment unchanged.
    Copy(SegmentId),
    /// Emit a new chunk.
    New {
        kind: SegmentKind,
        ty: [u8; 4],
        prefix: &'static [u8],
        payload: &'a [u8],
    },
}

struct Dims {
    kind: SegmentKind,
    name: Name,
    header_len: u32,
    payload_len: u64,
    trailer_len: u16,
}

impl Op<'_> {
    fn dims(&self, src: &Structure) -> Result<Dims> {
        Ok(match self {
            Op::Copy(id) => {
                let s = src.segment(*id)?;
                Dims {
                    kind: s.kind(),
                    name: s.name(),
                    header_len: s.header().len() as u32,
                    payload_len: s.payload().len(),
                    trailer_len: s.trailer().len() as u16,
                }
            }
            Op::New {
                kind,
                ty,
                prefix,
                payload,
            } => Dims {
                kind: *kind,
                name: Name::FourCC(*ty),
                header_len: 8 + prefix.len() as u32,
                payload_len: payload.len() as u64,
                trailer_len: 4,
            },
        })
    }
}

fn new_chunk<'a>(
    kind: SegmentKind,
    ty: [u8; 4],
    prefix: &'static [u8],
    payload: &'a [u8],
) -> Result<Op<'a>> {
    if (prefix.len() as u64).saturating_add(payload.len() as u64) > MAX_CHUNK_LEN {
        return Err(Error::LimitExceeded {
            what: "PNG chunk length",
            limit: MAX_CHUNK_LEN,
        });
    }
    Ok(Op::New {
        kind,
        ty,
        prefix,
        payload,
    })
}

/// Decide what the output contains. This is the single source of truth for
/// both the destination structure and the bytes written.
fn plan<'a>(src: &Structure, updates: &'a Updates) -> Result<WritePlan<'a>> {
    let mut ops: Vec<Op<'a>> = Vec::with_capacity(src.segments().len() + 2);
    let mut xmp_done = false;
    let mut jumbf_done = false;
    let has_jumbf = src
        .segments()
        .iter()
        .any(|s| s.kind() == SegmentKind::Jumbf);

    for (i, seg) in src.segments().iter().enumerate() {
        let id = SegmentId::from_index(i)?;
        match seg.kind() {
            SegmentKind::Xmp => match &updates.xmp {
                MetadataUpdate::Keep => {
                    ops.push(Op::Copy(id));
                    xmp_done = true;
                }
                MetadataUpdate::Set(d) if !xmp_done => {
                    ops.push(new_chunk(SegmentKind::Xmp, ITXT, XMP_ITXT_PREFIX, d)?);
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
                    ops.push(new_chunk(SegmentKind::Jumbf, CABX, &[], d)?);
                    jumbf_done = true;
                }
                _ => {}
            },
            _ => {
                // C2PA A.3.2: the caBX chunk should precede the IDAT chunks. Insert
                // before the first IDAT, or before an APNG fcTL that directly
                // precedes it so the frame-control chunk stays with its data.
                // Only when there is nothing to replace: an existing manifest keeps
                // its position (C2PA 11.2.3: an update must not move the store).
                if !has_jumbf && !jumbf_done && seg.name() == Name::FourCC(IDAT) {
                    if let MetadataUpdate::Set(d) = &updates.jumbf {
                        let op = new_chunk(SegmentKind::Jumbf, CABX, &[], d)?;
                        let after_fctl = matches!(
                            ops.last(),
                            Some(Op::Copy(p)) if src.segments()[p.index()].name() == Name::FourCC(FCTL)
                        );
                        if after_fctl {
                            let fctl = ops.pop().ok_or(Error::NotFound { what: "fcTL op" })?;
                            ops.push(op);
                            ops.push(fctl);
                        } else {
                            ops.push(op);
                        }
                        jumbf_done = true;
                    }
                }
                if seg.name() == Name::FourCC(IEND) {
                    if !xmp_done {
                        if let MetadataUpdate::Set(d) = &updates.xmp {
                            ops.push(new_chunk(SegmentKind::Xmp, ITXT, XMP_ITXT_PREFIX, d)?);
                            xmp_done = true;
                        }
                    }
                    if !jumbf_done {
                        if let MetadataUpdate::Set(d) = &updates.jumbf {
                            ops.push(new_chunk(SegmentKind::Jumbf, CABX, &[], d)?);
                            jumbf_done = true;
                        }
                    }
                }
                ops.push(Op::Copy(id));
            }
        }
    }

    // Destination size, then destination structure.
    let mut total = 0u64;
    let mut dims = Vec::with_capacity(ops.len());
    for op in &ops {
        let d = op.dims(src)?;
        total = (d.header_len as u64)
            .checked_add(d.payload_len)
            .and_then(|v| v.checked_add(d.trailer_len as u64))
            .and_then(|v| total.checked_add(v))
            .ok_or(Error::LimitExceeded {
                what: "output size",
                limit: u64::MAX,
            })?;
        dims.push(d);
    }

    let mut b = StructureBuilder::new(src.handler().clone(), src.media_type().to_owned(), total);
    // Where the data beyond the chunk structure starts in the output.
    let k = src.box_count();
    let trailing_op = ops
        .iter()
        .position(|op| matches!(op, Op::Copy(id) if id.index() == k));
    for (i, d) in dims.iter().enumerate() {
        if trailing_op == Some(i) {
            b.end_boxes()?;
        }
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
        let id = SegmentId::from_index(i)?;
        match op {
            Op::Copy(sid) => {
                plan.copy_segment(id, src.segment(*sid)?);
            }
            Op::New {
                ty,
                prefix,
                payload,
                ..
            } => {
                let (hdr, n) = chunk_header(ty, prefix, payload.len());
                plan.put(id, Role::Header, hdr[..n].to_vec());
                plan.put(id, Role::Payload, *payload);
                plan.put(id, Role::Trailer, chunk_crc(ty, prefix, payload).to_vec());
            }
        }
    }
    Ok(plan)
}

/// Length, type and prefix of a new chunk; returns the bytes and how many are used.
/// The length was bounded by [`new_chunk`].
fn chunk_header(
    ty: &[u8; 4],
    prefix: &[u8],
    payload_len: usize,
) -> ([u8; 8 + XMP_PREFIX_LEN], usize) {
    let data_len = (prefix.len() + payload_len) as u32;
    let mut hdr = [0u8; 8 + XMP_PREFIX_LEN];
    hdr[0..4].copy_from_slice(&data_len.to_be_bytes());
    hdr[4..8].copy_from_slice(ty);
    hdr[8..8 + prefix.len()].copy_from_slice(prefix);
    (hdr, 8 + prefix.len())
}

/// The checksum of a new chunk, which covers its type and data.
fn chunk_crc(ty: &[u8; 4], prefix: &[u8], payload: &[u8]) -> [u8; 4] {
    let mut crc = Crc32::new();
    crc.update(ty);
    crc.update(prefix);
    crc.update(payload);
    crc.finalize().to_be_bytes()
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
    let plan = plan(src_structure, updates)?;
    plan.execute(src, out, sink)?;
    Ok(plan.into_structure())
}

/// Recompute and store the CRC of the chunk in `seg` by reading it back from `out`.
pub(crate) fn fix_crc<W: Read + Write + Seek>(out: &mut W, seg: &Segment) -> Result<()> {
    let trailer = seg.trailer();
    if trailer.len() != 4 || seg.header().len() < 8 {
        return Err(Error::InvalidSegment {
            offset: seg.extent().offset(),
            reason: "not a PNG chunk".into(),
        });
    }
    // CRC covers type + data: everything after the 4-byte length, before the CRC.
    let start = seg.extent().offset() + 4;
    let mut remaining = trailer.offset() - start;
    out.seek(SeekFrom::Start(start))?;
    let mut crc = Crc32::new();
    let mut buf = [0u8; 8192];
    while remaining > 0 {
        let n = remaining.min(buf.len() as u64) as usize;
        out.read_exact(&mut buf[..n])?;
        crc.update(&buf[..n]);
        remaining -= n as u64;
    }
    out.seek(SeekFrom::Start(trailer.offset()))?;
    out.write_all(&crc.finalize().to_be_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::Chunk;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    fn chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(ty);
        v.extend_from_slice(data);
        let mut c = Crc32::new();
        c.update(ty);
        c.update(data);
        v.extend_from_slice(&c.finalize().to_be_bytes());
        v
    }

    fn sample_png() -> Vec<u8> {
        let mut v = SIGNATURE.to_vec();
        v.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
        v.extend(chunk(b"IDAT", &[1, 2, 3, 4, 5, 6, 7, 8]));
        v.extend(chunk(b"IEND", &[]));
        v
    }

    /// Every chunk's stored CRC matches its contents.
    fn verify_crcs(bytes: &[u8]) {
        let s = parse(&mut Cursor::new(bytes.to_vec())).unwrap();
        for seg in s.segments().iter().skip(1) {
            if seg.trailer().len() != 4 {
                continue;
            }
            let e = seg.extent();
            let body = &bytes[(e.offset() + 4) as usize..seg.trailer().offset() as usize];
            let stored = &bytes[seg.trailer().offset() as usize..e.end() as usize];
            let mut c = Crc32::new();
            c.update(body);
            assert_eq!(
                stored,
                &c.finalize().to_be_bytes()[..],
                "bad CRC at {}",
                e.offset()
            );
        }
    }

    fn jumbf_stub(n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
        v[4..8].copy_from_slice(b"jumb");
        v
    }

    /// Chunk type names of the output, in order.
    fn order_after_set(png: &[u8]) -> Vec<String> {
        let s = parse(&mut Cursor::new(png.to_vec())).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(png.to_vec()),
            &mut out,
            &Updates::new().set_jumbf(jumbf_stub(64)),
            &mut crate::NullSink,
        )
        .unwrap();
        dest.segments()
            .iter()
            .skip(1)
            .map(|g| match g.name() {
                Name::FourCC(t) => String::from_utf8_lossy(&t).into_owned(),
                _ => "?".into(),
            })
            .collect()
    }

    #[test]
    fn new_cabx_precedes_idat() {
        // C2PA A.3.2: caBX should precede the IDAT chunks, not sit before IEND.
        let mut v = SIGNATURE.to_vec();
        v.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
        v.extend(chunk(b"tEXt", b"k\0v"));
        v.extend(chunk(b"IDAT", &[1, 2, 3, 4]));
        v.extend(chunk(b"IDAT", &[5, 6, 7, 8]));
        v.extend(chunk(b"IEND", &[]));
        assert_eq!(
            order_after_set(&v),
            ["IHDR", "tEXt", "caBX", "IDAT", "IDAT", "IEND"]
        );
    }

    #[test]
    fn new_cabx_stays_ahead_of_apng_fctl() {
        let mut v = SIGNATURE.to_vec();
        v.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
        v.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        v.extend(chunk(b"fcTL", &[0u8; 26]));
        v.extend(chunk(b"IDAT", &[1, 2, 3, 4]));
        v.extend(chunk(b"IEND", &[]));
        assert_eq!(
            order_after_set(&v),
            ["IHDR", "acTL", "caBX", "fcTL", "IDAT", "IEND"]
        );
    }

    #[test]
    fn existing_cabx_is_replaced_in_place() {
        let mut v = SIGNATURE.to_vec();
        v.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
        v.extend(chunk(b"IDAT", &[1, 2, 3, 4]));
        v.extend(chunk(b"caBX", &jumbf_stub(40)));
        v.extend(chunk(b"IEND", &[]));
        // Position of an existing manifest is kept; no second one is added.
        assert_eq!(order_after_set(&v), ["IHDR", "IDAT", "caBX", "IEND"]);
    }

    #[test]
    fn parse_tiles_file() {
        let png = sample_png();
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        assert_eq!(s.total_size(), png.len() as u64);
        let mut next = 0;
        for r in s.regions() {
            assert_eq!(r.range.offset(), next);
            next = r.range.end();
        }
        assert_eq!(next, png.len() as u64);
    }

    #[test]
    fn rejects_truncated_and_oversized_chunks() {
        let mut png = sample_png();
        png.truncate(png.len() - 5);
        assert!(parse(&mut Cursor::new(png)).is_err());

        let mut bad = SIGNATURE.to_vec();
        bad.extend_from_slice(&0x7FFF_FFF0u32.to_be_bytes());
        bad.extend_from_slice(b"IDAT");
        bad.extend_from_slice(&[0; 16]);
        assert!(parse(&mut Cursor::new(bad)).is_err());
    }

    #[test]
    fn trailing_bytes_are_kept_in_structure() {
        let mut png = sample_png();
        png.extend_from_slice(b"trailing");
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        assert_eq!(s.segments().last().unwrap().extent().len(), 8);
    }

    #[test]
    fn data_after_iend_is_reported_as_trailing() {
        let clean = sample_png();
        let s = parse(&mut Cursor::new(clean.clone())).unwrap();
        assert_eq!(s.trailing(), None);

        let mut png = clean.clone();
        png.extend_from_slice(b"trailing");
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        let t = s.trailing().unwrap();
        assert_eq!((t.offset(), t.len()), (clean.len() as u64, 8));

        // An edit moves it with the file, and what was planned is what is written.
        let updates = Updates::new().set_jumbf(vec![0xAB; 100]);
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(png.clone()),
            &mut out,
            &updates,
            &mut crate::sink::NullSink,
        )
        .unwrap();
        let out = out.into_inner();
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        assert_eq!(dest.trailing(), re.trailing());
        assert_eq!(
            &out[re.trailing().unwrap().offset() as usize..],
            b"trailing"
        );
    }

    #[test]
    fn sink_sees_every_output_byte_once_in_order() {
        let png = sample_png();
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        let updates = Updates::new().set_jumbf(vec![0xAB; 100]);

        let mut seen: Vec<u8> = Vec::new();
        let mut expect_off = 0u64;
        let mut sink = |c: Chunk<'_>| -> Result<()> {
            assert_eq!(c.offset, expect_off);
            expect_off += c.data.len() as u64;
            seen.extend_from_slice(c.data);
            Ok(())
        };
        let mut out = Cursor::new(Vec::new());
        let dest = write(&s, &mut Cursor::new(png), &mut out, &updates, &mut sink).unwrap();
        let out = out.into_inner();
        assert_eq!(seen, out);
        assert_eq!(dest.total_size(), out.len() as u64);
        verify_crcs(&out);

        let reparsed = parse(&mut Cursor::new(out.clone())).unwrap();
        let e = reparsed.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.len(), 100);
        let data = reparsed.read_entity(&mut Cursor::new(out), e).unwrap();
        assert_eq!(data, vec![0xAB; 100]);
    }

    #[test]
    fn patch_updates_payload_and_crc() {
        let png = sample_png();
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        let updates = Updates::new().set_jumbf(vec![0u8; 64]);
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(png),
            &mut out,
            &updates,
            &mut crate::sink::NullSink,
        )
        .unwrap();

        let e = dest.require_entity(SegmentKind::Jumbf).unwrap().clone();
        dest.patch(&mut out, &e, b"signed manifest").unwrap();
        let bytes = out.into_inner();
        verify_crcs(&bytes);

        let re = parse(&mut Cursor::new(bytes.clone())).unwrap();
        let e = re.require_entity(SegmentKind::Jumbf).unwrap();
        let data = re.read_entity(&mut Cursor::new(bytes), e).unwrap();
        assert_eq!(&data[..15], b"signed manifest");
        assert!(data[15..].iter().all(|&b| b == 0));
        assert_eq!(data.len(), 64);
    }

    #[test]
    fn patch_rejects_oversized_data() {
        let png = sample_png();
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        let updates = Updates::new().set_jumbf(vec![0u8; 8]);
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(png),
            &mut out,
            &updates,
            &mut crate::sink::NullSink,
        )
        .unwrap();
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap().clone();
        assert!(dest.patch(&mut out, &e, &[1u8; 9]).is_err());
    }

    /// The hash a consumer builds from the write-side stream equals the hash it
    /// builds by walking the finished structure: two independent routes agree.
    #[test]
    fn sink_hash_equals_region_walk_hash() {
        let png = sample_png();
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        let updates = Updates::new().set_jumbf(vec![0x5A; 200]);

        // Consumer policy (the sink's business, not asset-io's): skip the
        // payload and trailer of Jumbf segments.
        let mut kinds: Vec<SegmentKind> = Vec::new();
        struct Hash<'k> {
            h: Sha256,
            kinds: &'k mut Vec<SegmentKind>,
        }
        impl Sink for Hash<'_> {
            fn begin(&mut self, dest: &Structure) -> Result<()> {
                self.kinds.extend(dest.segments().iter().map(|s| s.kind()));
                Ok(())
            }
            fn wants(&self, seg: &Segment, role: Role) -> bool {
                !(seg.kind() == SegmentKind::Jumbf && role != Role::Header)
            }
            fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
                self.h.update(c.data);
                Ok(())
            }
        }
        let mut sink = Hash {
            h: Sha256::new(),
            kinds: &mut kinds,
        };
        let mut out = Cursor::new(Vec::new());
        let dest = write(&s, &mut Cursor::new(png), &mut out, &updates, &mut sink).unwrap();
        let streamed = sink.h.finalize();

        let mut walked = Sha256::new();
        let mut buf = vec![0u8; 16];
        let mut src = Cursor::new(out.into_inner());
        for r in dest.regions() {
            let seg = dest.segment(r.segment).unwrap();
            if seg.kind() == SegmentKind::Jumbf && r.role != Role::Header {
                continue;
            }
            dest.stream_range(&mut src, r.range, &mut buf, |b| {
                walked.update(b);
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(streamed[..], walked.finalize()[..]);
        assert_eq!(kinds.len(), dest.segments().len());
    }

    #[test]
    fn xmp_round_trip_and_compressed_itxt_is_not_xmp() {
        let png = sample_png();
        let s = parse(&mut Cursor::new(png.clone())).unwrap();
        let updates = Updates::new().set_xmp(b"<x/>".to_vec());
        let mut out = Cursor::new(Vec::new());
        write(
            &s,
            &mut Cursor::new(png),
            &mut out,
            &updates,
            &mut crate::sink::NullSink,
        )
        .unwrap();
        let bytes = out.into_inner();
        verify_crcs(&bytes);
        let re = parse(&mut Cursor::new(bytes.clone())).unwrap();
        let e = re.require_entity(SegmentKind::Xmp).unwrap();
        assert_eq!(re.read_entity(&mut Cursor::new(bytes), e).unwrap(), b"<x/>");

        // Same keyword but compression flag set: must not be treated as XMP.
        let mut prefix = XMP_ITXT_PREFIX.to_vec();
        prefix[18] = 1;
        let mut data = prefix;
        data.extend_from_slice(b"zzz");
        let mut p = SIGNATURE.to_vec();
        p.extend(chunk(b"iTXt", &data));
        p.extend(chunk(b"IEND", &[]));
        let s = parse(&mut Cursor::new(p)).unwrap();
        assert!(s.entity(SegmentKind::Xmp).unwrap().is_none());
    }
}
