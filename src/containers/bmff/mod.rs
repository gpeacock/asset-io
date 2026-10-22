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

//! ISO BMFF (MP4, MOV, HEIF, AVIF, ...) on the structure model.
//!
//! Segments are the file's top-level boxes. A box's `size`/`type` (and extended
//! size and `uuid` type) are the header; the rest is the payload; there is no
//! trailer. Boxes nested inside segments are not segments: [`Structure::walk`]
//! finds them lazily, reading only box headers.
//!
//! C2PA's content provenance boxes (`uuid` D8FEC3D6-...) become entities:
//! `manifest`, `original` and `update` boxes are [`SegmentKind::Jumbf`] entities
//! labelled with their purpose, whose payload is the manifest store itself (the
//! purpose string and the merkle offset field belong to the header). `merkle` boxes
//! are [`SegmentKind::Other`] entities labelled `merkle`. XMP `uuid` boxes are
//! [`SegmentKind::Xmp`] entities.
//!
//! HEIF items found through a file-level `meta` box are entities too: an `Exif`
//! item is a [`SegmentKind::Exif`] entity (the TIFF data, after the item's offset
//! field), and an item that a `thmb` reference names as a thumbnail is a
//! [`SegmentKind::Thumbnail`] entity labelled with its item type. Their bytes lie in
//! `mdat` or `idat`, so an entity can have several parts, and `Structure::slice_entity`
//! and `read_entity` work on them as on any other. Items that cannot be located
//! (external data, unsupported construction methods, extents outside the file) are
//! left out, and a `meta` box that does not parse does not make the file an error.
//!
//! Combinations the C2PA specification forbids are errors at parse time: more than
//! one manifest, original or update store; `original` without `update` or the
//! reverse; `manifest` together with `update`; a C2PA box without a known purpose.
//!
//! # Writing
//!
//! `Updates::set_jumbf` writes a `manifest` box: it replaces the existing
//! `manifest` (or `original`) box in place, or is inserted after `ftyp`. Any
//! `update` box is dropped, as when a file is re-signed. `remove_jumbf` drops every
//! C2PA box. XMP follows the same placement rules.
//!
//! [`BmffUpdates::set_merkle`] replaces the `merkle` boxes with new ones, placed right
//! after the last `mdat`; `remove_merkle` drops them. A file that has them takes a
//! new manifest only if they are also set or removed, since they belong with the
//! manifest that describes them. The boxes can be written as placeholders of their
//! final size and filled in with `Structure::patch_exact` once the media has been
//! hashed during the write.
//!
//! `set_jumbf(store).as_update()` is the same as `set_jumbf` unless the new store begins
//! with the manifests already in the file, byte for byte. Then those stay as they
//! are (a `manifest` box becomes `original`; only its purpose changes) and the
//! rest is written as the `update` box, added at the end of the file or replacing
//! the existing `update` box. Nothing before it moves.
//!
//! Adding, replacing or dropping a box moves the data after it. The absolute file
//! offsets that point there (`stco`, `co64`, `saio`, `tfhd` base data offset,
//! `tfra`, `iloc`) are rewritten while the file is copied: the size change is known
//! before copying starts, so there is one pass. Tables are rewritten through the
//! copy buffer, so memory does not grow with the size of `moov`; only `iloc`,
//! which is variable-length, is buffered (capped at 16 MiB). When no box changes
//! size, nothing is rewritten and `moov` is copied verbatim. `mfro` and `sidx`
//! hold sizes and relative offsets, which these edits do not change.

mod heif_items;

use crate::plan::{OffsetMap, Relocate, WritePlan};
use crate::{
    block_on, BoxFuture, ByteRange, ContainerHandler, Entity, Error, Existing, FormatSpec, Io,
    MetadataSlot, MetadataUpdate, Name, Node, PlanBuilder, ReadSeek, Result, Role, Segment,
    SegmentId, SegmentKind, Structure, StructureBuilder, Task, Updates, MAX_SEGMENT_SIZE,
};
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// C2PA content provenance box type, `D8FEC3D6-1B0E-483C-9297-5828877EC481`.
pub const C2PA_UUID: [u8; 16] = [
    0xd8, 0xfe, 0xc3, 0xd6, 0x1b, 0x0e, 0x48, 0x3c, 0x92, 0x97, 0x58, 0x28, 0x87, 0x7e, 0xc4, 0x81,
];
/// XMP box type, `BE7ACFCB-97A9-42E8-9C71-999491E3AFAC`.
pub const XMP_UUID: [u8; 16] = [
    0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac,
];

/// Maximum nesting depth followed by [`walk`](ContainerHandler::walk).
pub const MAX_BOX_DEPTH: u32 = 32;

/// Purpose labels of C2PA boxes (see [`labels`](crate::labels)).
pub use crate::labels::{MANIFEST, MERKLE, ORIGINAL, UPDATE};

/// What to do with the `merkle` boxes of a file; see [`BmffUpdates`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum MerkleUpdate {
    /// Keep existing boxes (default)
    #[default]
    Keep,
    /// Remove existing boxes
    Remove,
    /// Replace existing boxes with these payloads, one box each
    Set(Vec<Vec<u8>>),
}

/// ISO BMFF options for [`Updates`]: what to do with the `merkle` boxes.
///
/// Merkle boxes carry proofs for hashed media blocks. Other containers ignore
/// these options. [`Updates::remove_all`] removes them as well.
pub trait BmffUpdates: Sized {
    /// Replace the file's merkle boxes with one box per payload.
    ///
    /// The boxes are written immediately after the last `mdat`, and any existing
    /// merkle boxes are dropped. All payloads are usually the same length (the C2PA
    /// specification pads them to one length).
    ///
    /// The content usually depends on hashes of the media, which exist only after
    /// the write. Write zeroed payloads of the final length, hash the media through
    /// the [`Sink`](crate::Sink) during the write, then fill each box in place with
    /// [`Structure::patch_exact`](crate::Structure::patch_exact). The boxes are the
    /// entities labelled [`MERKLE`] of kind [`SegmentKind::Other`].
    ///
    /// A file that has merkle boxes cannot take a new manifest unless the boxes are
    /// also set or removed, and an update manifest cannot change them.
    fn set_merkle(self, payloads: Vec<Vec<u8>>) -> Self;

    /// Remove the file's merkle boxes. See [`set_merkle`](Self::set_merkle).
    fn remove_merkle(self) -> Self;

    /// Keep the file's merkle boxes (explicit, same as default).
    fn keep_merkle(self) -> Self;

    /// What to do with the merkle boxes.
    fn merkle(&self) -> &MerkleUpdate;
}

impl BmffUpdates for Updates {
    fn set_merkle(self, payloads: Vec<Vec<u8>>) -> Self {
        self.set_extension(MerkleUpdate::Set(payloads))
    }

    fn remove_merkle(self) -> Self {
        self.set_extension(MerkleUpdate::Remove)
    }

    fn keep_merkle(self) -> Self {
        self.set_extension(MerkleUpdate::Keep)
    }

    fn merkle(&self) -> &MerkleUpdate {
        static KEEP: MerkleUpdate = MerkleUpdate::Keep;
        static REMOVE: MerkleUpdate = MerkleUpdate::Remove;
        match self.extension::<MerkleUpdate>() {
            Some(m) => m,
            None if self.removes_all() => &REMOVE,
            None => &KEEP,
        }
    }
}

/// Longest purpose string looked for: longer than any defined purpose.
const MAX_PURPOSE_LEN: usize = 32;

/// Boxes whose content is a sequence of boxes that C2PA exclusions can address.
const CONTAINERS: &[[u8; 4]] = &[
    *b"moov", *b"trak", *b"mdia", *b"minf", *b"stbl", *b"moof", *b"traf", *b"edts", *b"udta",
    *b"dinf", *b"tref", *b"treg", *b"mvex", *b"mfra", *b"meta", *b"schi",
];

/// ISO/IEC 14496-12:2022 boxes that begin with version and flags.
const FULL_BOXES: &[[u8; 4]] = &[
    *b"pdin", *b"mvhd", *b"tkhd", *b"mdhd", *b"hdlr", *b"nmhd", *b"elng", *b"stsd", *b"stdp",
    *b"stts", *b"ctts", *b"cslg", *b"stss", *b"stsh", *b"elst", *b"dref", *b"stsz", *b"stz2",
    *b"stsc", *b"stco", *b"co64", *b"padb", *b"subs", *b"saiz", *b"saio", *b"mehd", *b"trex",
    *b"mfhd", *b"tfhd", *b"trun", *b"tfra", *b"mfro", *b"tfdt", *b"leva", *b"trep", *b"assp",
    *b"sbgp", *b"sgpd", *b"csgp", *b"cprt", *b"tsel", *b"kind", *b"meta", *b"xml ", *b"bxml",
    *b"iloc", *b"pitm", *b"ipro", *b"infe", *b"iinf", *b"iref", *b"ipma", *b"schm", *b"fiin",
    *b"fpar", *b"fecr", *b"gitn", *b"fire", *b"stri", *b"stsg", *b"stvi", *b"csch", *b"sidx",
    *b"ssix", *b"prft", *b"srpp", *b"vmhd", *b"smhd", *b"srat", *b"chnl", *b"dmix", *b"txtC",
    *b"mime", *b"uri ", *b"uriI", *b"hmhd", *b"sthd", *b"vvhd", *b"medc",
];

const UUID: [u8; 4] = *b"uuid";
const MDAT: [u8; 4] = *b"mdat";
const FTYP: [u8; 4] = *b"ftyp";
const META: [u8; 4] = *b"meta";
const HDLR: [u8; 4] = *b"hdlr";

/// The ISO BMFF container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct BmffHandler;

/// The shared BMFF handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(BmffHandler)
}

/// The ISO BMFF container's [`ContainerHandler::id`].
pub const ID: &str = "bmff";

impl ContainerHandler for BmffHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[
            FormatSpec::new("video/mp4", "mp4").media_type_aliases(&["application/mp4"]),
            FormatSpec::new("audio/mp4", "m4a"),
            FormatSpec::new("video/x-m4v", "m4v"),
            FormatSpec::new("video/quicktime", "mov"),
            FormatSpec::new("image/heic", "heic"),
            FormatSpec::new("image/heif", "heif"),
            FormatSpec::new("image/avif", "avif"),
        ];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.len() >= 12 && header[4..8] == FTYP
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
        Box::pin(async move {
            let (ops, labels, map) = plan(src_structure, &mut io, updates).await?;
            assemble(src_structure, &mut io, ops, &labels, map, true).await
        })
    }

    // The structure does not depend on the offset tables, so planning alone does not
    // read them.
    fn plan(
        &self,
        src_structure: &Structure,
        src: &mut dyn ReadSeek,
        updates: &Updates,
    ) -> Result<Structure> {
        Task::new(src_structure.total_size(), |mut io| async move {
            let (ops, labels, map) = plan(src_structure, &mut io, updates).await?;
            let plan = assemble(src_structure, &mut io, ops, &labels, map, false).await?;
            Ok(plan.into_structure())
        })
        .run(src)
    }

    fn compose(&self, kind: SegmentKind, data: &[u8]) -> Result<Vec<u8>> {
        let op = match kind {
            SegmentKind::Jumbf => new_c2pa(data)?,
            SegmentKind::Xmp => new_xmp(data)?,
            _ => return Err(Error::UnsupportedFormat),
        };
        let Op::New {
            header, payload, ..
        } = op
        else {
            return Err(Error::UnsupportedFormat);
        };
        let mut out = header;
        out.extend_from_slice(&payload);
        Ok(out)
    }

    fn walk(
        &self,
        _structure: &Structure,
        src: &mut dyn ReadSeek,
        f: &mut dyn FnMut(&Node<'_>) -> Result<()>,
    ) -> Result<()> {
        let total = src.seek(SeekFrom::End(0))?;
        block_on(total, src, |mut io| async move {
            walk(&mut io, 0, total, f).await
        })
    }
}

/// Parse the top-level boxes of a BMFF file into a validated structure.
pub fn parse<R: Read + Seek>(src: &mut R) -> Result<Structure> {
    handler().parse(src)
}

#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    /// Whole box, clamped as described on [`read_header`].
    size: u64,
    /// 8, or 16 with an extended size.
    header_len: u32,
    ty: [u8; 4],
}

/// Read the box header at `at`, inside a region ending at `end` (`at < end`).
///
/// - `Ok(None)`: fewer than 8 bytes remain; some files carry trailing bytes.
/// - size 0 means the box runs to `end`.
/// - a box larger than the region is an error, except `mdat`, which is clamped to
///   the region (truncated media files are common and still hashable).
async fn read_header(io: &mut Io, at: u64, end: u64) -> Result<Option<BoxHeader>> {
    let remaining = end - at;
    if remaining < 8 {
        return Ok(None);
    }
    let mut h = [0u8; 8];
    io.read_at(at, &mut h).await?;
    let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    let ty = [h[4], h[5], h[6], h[7]];
    let (mut size, header_len) = match size32 {
        0 => (remaining, 8u32),
        1 => {
            if remaining < 16 {
                return Err(Error::invalid_segment(at, "extended box size truncated"));
            }
            let mut l = [0u8; 8];
            io.read_at(at + 8, &mut l).await?;
            (u64::from_be_bytes(l), 16u32)
        }
        n => (n as u64, 8u32),
    };
    if size < header_len as u64 {
        return Err(Error::invalid_segment(
            at,
            "box size smaller than its header",
        ));
    }
    if size > remaining {
        if ty == MDAT {
            size = remaining;
        } else {
            return Err(Error::invalid_segment(at, "box extends past its container"));
        }
    }
    Ok(Some(BoxHeader {
        size,
        header_len,
        ty,
    }))
}

fn is_container(ty: &[u8; 4]) -> bool {
    CONTAINERS.contains(ty)
}

fn is_full_box(ty: &[u8; 4]) -> bool {
    FULL_BOXES.contains(ty)
}

/// QuickTime writes `meta` without version and flags; its first child is `hdlr`.
async fn meta_lacks_full_box_header(io: &mut Io, content: u64, box_end: u64) -> bool {
    if box_end - content < 8 {
        return false;
    }
    let mut b = [0u8; 8];
    io.read_at(content, &mut b).await.is_ok() && b[4..8] == HDLR[..]
}

/// One level of boxes being walked: where the next box is, where the level ends, and
/// how much of the path belongs to the levels above.
#[derive(Clone, Copy)]
struct Level {
    at: u64,
    end: u64,
    mark: usize,
}

/// Visit the boxes in `[start, end)` and the boxes nested in the containers among
/// them, in file order, a parent before its children. Reads only box headers (and
/// the few bytes after one that say what it is).
async fn walk<F>(io: &mut Io, start: u64, end: u64, f: &mut F) -> Result<()>
where
    F: FnMut(&Node<'_>) -> Result<()> + ?Sized,
{
    let mut path = String::new();
    let mut levels = vec![Level {
        at: start,
        end,
        mark: 0,
    }];
    while let Some(&Level { at, end, mark }) = levels.last() {
        let depth = levels.len() as u32;
        let Some(h) = (if at < end {
            read_header(io, at, end).await?
        } else {
            None
        }) else {
            // The level is done: no more boxes, or only trailing bytes too short
            // to be one.
            path.truncate(mark);
            levels.pop();
            continue;
        };
        let box_end = at + h.size; // read_header guarantees <= end
        let mut content = at + h.header_len as u64;
        let mut user_type = None;
        let mut full = is_full_box(&h.ty);

        if h.ty == UUID {
            if box_end - content < 16 {
                return Err(Error::invalid_segment(
                    at,
                    "uuid box too small for its type",
                ));
            }
            let mut u = [0u8; 16];
            io.read_at(content, &mut u).await?;
            content += 16;
            full = u == C2PA_UUID;
            user_type = Some(u);
        } else if h.ty == META && full && meta_lacks_full_box_header(io, content, box_end).await {
            full = false;
        }
        let header_len = (content - at) as u32;

        let (mut version, mut flags) = (None, None);
        if full {
            if box_end - content < 4 {
                return Err(Error::invalid_segment(
                    at,
                    "FullBox too small for version and flags",
                ));
            }
            let mut vf = [0u8; 4];
            io.read_at(content, &mut vf).await?;
            content += 4;
            version = Some(vf[0]);
            flags = Some(u32::from_be_bytes([0, vf[1], vf[2], vf[3]]));
        }

        let here = path.len();
        path.push('/');
        path.push_str(&String::from_utf8_lossy(&h.ty));
        f(&Node {
            path: &path,
            name: Name::FourCC(h.ty),
            extent: crate::ByteRange::new(at, h.size)?,
            header_len,
            depth,
            version,
            flags,
            user_type,
        })?;
        if let Some(level) = levels.last_mut() {
            level.at = box_end;
        }
        if is_container(&h.ty) && content < box_end {
            if depth + 1 > MAX_BOX_DEPTH {
                return Err(Error::LimitExceeded {
                    what: "box nesting depth",
                    limit: MAX_BOX_DEPTH as u64,
                });
            }
            levels.push(Level {
                at: content,
                end: box_end,
                mark: here,
            });
        } else {
            path.truncate(here);
        }
    }
    Ok(())
}

/// Media type implied by the major brand of a leading `ftyp` box.
async fn media_type(io: &mut Io, total: u64) -> Result<&'static str> {
    let Some(h) = read_header(io, 0, total).await? else {
        return Ok("video/mp4");
    };
    if h.ty != FTYP || h.size < h.header_len as u64 + 4 {
        return Ok("video/mp4");
    }
    let mut brand = [0u8; 4];
    io.read_at(h.header_len as u64, &mut brand).await?;
    Ok(match &brand {
        b"avif" | b"avis" => "image/avif",
        b"heic" | b"heix" | b"hevc" | b"hevx" => "image/heic",
        b"mif1" | b"msf1" => "image/heif",
        b"qt  " => "video/quicktime",
        b"M4A " => "audio/mp4",
        b"M4V " | b"M4VH" | b"M4VP" => "video/x-m4v",
        _ => "video/mp4",
    })
}

/// What a top-level box is, from its header and the first bytes of its content.
struct Classified {
    kind: SegmentKind,
    /// Header length including any `uuid` type and C2PA fields.
    header_len: u32,
    entity: Option<EntityKind>,
}

enum EntityKind {
    /// A C2PA box with this purpose; `merkle` boxes are not JUMBF.
    C2pa(&'static str),
    Xmp,
}

fn purpose_label(p: &[u8]) -> Option<&'static str> {
    match p {
        b"manifest" => Some(MANIFEST),
        b"original" => Some(ORIGINAL),
        b"update" => Some(UPDATE),
        b"merkle" => Some(MERKLE),
        _ => None,
    }
}

async fn classify(io: &mut Io, at: u64, h: &BoxHeader) -> Result<Classified> {
    let plain = |kind| Classified {
        kind,
        header_len: h.header_len,
        entity: None,
    };
    match h.ty {
        FTYP => return Ok(plain(SegmentKind::Header)),
        MDAT => return Ok(plain(SegmentKind::ImageData)),
        UUID => {}
        _ => return Ok(plain(SegmentKind::Other)),
    }

    let box_end = at + h.size;
    let body = at + h.header_len as u64;
    if box_end - body < 16 {
        return Err(Error::invalid_segment(
            at,
            "uuid box too small for its type",
        ));
    }
    let mut u = [0u8; 16];
    io.read_at(body, &mut u).await?;
    let after_uuid = body + 16;
    if u == XMP_UUID {
        return Ok(Classified {
            kind: SegmentKind::Xmp,
            header_len: h.header_len + 16,
            entity: Some(EntityKind::Xmp),
        });
    }
    if u != C2PA_UUID {
        return Ok(Classified {
            kind: SegmentKind::Other,
            header_len: h.header_len + 16,
            entity: None,
        });
    }

    // FullBox version and flags, then the NUL-terminated purpose.
    let purpose_at = after_uuid + 4;
    if purpose_at >= box_end {
        return Err(Error::invalid_segment(at, "C2PA box has no purpose"));
    }
    let n = (box_end - purpose_at).min(MAX_PURPOSE_LEN as u64 + 1) as usize;
    let mut buf = [0u8; MAX_PURPOSE_LEN + 1];
    io.read_at(purpose_at, &mut buf[..n]).await?;
    let nul = buf[..n].iter().position(|&b| b == 0).ok_or_else(|| {
        Error::invalid_segment(at, "C2PA box purpose is unterminated or too long")
    })?;
    let label = purpose_label(&buf[..nul])
        .ok_or_else(|| Error::invalid_segment(at, "unknown C2PA box purpose"))?;

    let mut header_len = (purpose_at - at) + nul as u64 + 1;
    let kind = if label == MERKLE {
        SegmentKind::Other
    } else {
        // 8-byte offset of the first merkle box, then the manifest store.
        header_len += 8;
        if at + header_len > box_end {
            return Err(Error::invalid_segment(
                at,
                "C2PA box too small for its merkle offset",
            ));
        }
        SegmentKind::Jumbf
    };
    Ok(Classified {
        kind,
        header_len: u32::try_from(header_len)
            .map_err(|_| Error::invalid_segment(at, "header too long"))?,
        entity: Some(EntityKind::C2pa(label)),
    })
}

#[derive(Default)]
struct StoreCounts {
    manifest: u32,
    original: u32,
    update: u32,
}

impl StoreCounts {
    fn add(&mut self, label: &str) {
        match label {
            MANIFEST => self.manifest += 1,
            ORIGINAL => self.original += 1,
            UPDATE => self.update += 1,
            _ => {}
        }
    }

    /// C2PA 2.x, A.5 and 11.2: one manifest store, or one original/update pair.
    fn check(&self) -> Result<()> {
        let err = |m: &str| Err(Error::InvalidFormat(m.into()));
        if self.manifest > 1 || self.original > 1 || self.update > 1 {
            return err("more than one C2PA manifest store box of the same purpose");
        }
        if self.original != self.update {
            return err("C2PA original and update boxes must appear together");
        }
        if self.manifest > 0 && self.update > 0 {
            return err("C2PA manifest box cannot be combined with an update box");
        }
        Ok(())
    }
}

async fn parse_with(handler: Arc<dyn ContainerHandler>, mut io: Io) -> Result<Structure> {
    let io = &mut io;
    let total = io.len();
    if total < 8 {
        return Err(Error::InvalidFormat("BMFF too small".into()));
    }
    let mut b = StructureBuilder::new(handler, media_type(io, total).await?, total);
    let mut counts = StoreCounts::default();
    // Top-level segments in order (for placing item data), and file-level `meta`
    // boxes (where HEIF items are listed).
    let mut placed: Vec<(SegmentId, u64, u64)> = Vec::new();
    let mut metas: Vec<(u64, u64)> = Vec::new();

    let mut at = 0u64;
    while at < total {
        let Some(h) = read_header(io, at, total).await? else {
            // Fewer than 8 bytes cannot be a box: keep them as unnamed trailing data.
            b.push_tail()?;
            break;
        };
        let c = classify(io, at, &h).await?;
        let id = b.push_span(
            c.kind,
            Name::FourCC(h.ty),
            c.header_len,
            h.size - c.header_len as u64,
            0,
        )?;
        placed.push((id, at, h.size));
        if h.ty == META {
            let content = at + c.header_len as u64;
            let end = at + h.size;
            let body = if meta_lacks_full_box_header(io, content, end).await {
                content
            } else {
                content + 4
            };
            if body <= end {
                metas.push((body, end));
            }
        }
        match c.entity {
            Some(EntityKind::Xmp) => b.entity_of_segments(SegmentKind::Xmp, &[id])?,
            Some(EntityKind::C2pa(label)) => {
                counts.add(label);
                let payload = b.segment(id)?.payload();
                b.entity_labelled(c.kind, label, &[(id, payload)])?;
            }
            None => {}
        }
        at += h.size;
    }
    counts.check()?;
    for (body, end) in metas {
        // Items are optional metadata: a `meta` box that cannot be read as HEIF
        // items does not make the file unreadable.
        let Ok(found) = read_items(io, body, end).await else {
            continue;
        };
        for f in &found {
            item_entity(&mut b, io, &placed, f).await?;
        }
    }
    b.finish()
}

/// Largest `meta` box read to look for items.
const MAX_META_BOX: u64 = 16 << 20;

async fn read_items(io: &mut Io, body: u64, end: u64) -> Result<Vec<heif_items::Found>> {
    let len = end - body;
    if len > MAX_META_BOX {
        return Err(Error::LimitExceeded {
            what: "meta box",
            limit: MAX_META_BOX,
        });
    }
    let buf = io.read_vec(body, len as usize).await?;
    heif_items::find(&buf, body)
}

/// The parts of `[offset, offset + len)` in each top-level segment it crosses,
/// or `None` if it leaves the file or crosses too many.
fn split_over_segments(
    placed: &[(SegmentId, u64, u64)],
    mut offset: u64,
    mut len: u64,
) -> Result<Option<Vec<(SegmentId, ByteRange)>>> {
    const MAX_PARTS: usize = 4096;
    let mut parts = Vec::new();
    while len > 0 {
        let i = placed.partition_point(|p| p.1 <= offset);
        let Some(&(id, start, size)) = i.checked_sub(1).and_then(|i| placed.get(i)) else {
            return Ok(None);
        };
        if offset >= start + size || parts.len() >= MAX_PARTS {
            return Ok(None);
        }
        let take = len.min(start + size - offset);
        parts.push((id, ByteRange::new(offset, take)?));
        offset += take;
        len -= take;
    }
    Ok(Some(parts))
}

/// Add the entity for one HEIF item. An item that cannot be located in the file is
/// skipped.
async fn item_entity(
    b: &mut StructureBuilder,
    io: &mut Io,
    placed: &[(SegmentId, u64, u64)],
    f: &heif_items::Found,
) -> Result<()> {
    let mut extents = f.extents.clone();
    if f.kind == heif_items::Kind::Exif {
        // The item starts with the offset of the TIFF header from the end of that
        // field; the entity is the TIFF data.
        let (at, len) = extents[0];
        let mut h = [0u8; 4];
        if len < 4 || io.read_at(at, &mut h).await.is_err() {
            return Ok(());
        }
        let skip = 4u64.saturating_add(u32::from_be_bytes(h) as u64);
        extents = heif_items::skip_bytes(&extents, skip);
    }
    let mut parts = Vec::new();
    for (at, len) in extents {
        match split_over_segments(placed, at, len)? {
            Some(p) => parts.extend(p),
            None => return Ok(()),
        }
    }
    if parts.is_empty() {
        return Ok(());
    }
    match f.kind {
        heif_items::Kind::Exif => b.entity(SegmentKind::Exif, &parts),
        heif_items::Kind::Thumbnail => b.entity_with_media_type(
            SegmentKind::Thumbnail,
            Some(String::from_utf8_lossy(&f.item_type).into_owned().into()),
            f.media_type.clone().map(Into::into),
            &parts,
        ),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Most offset-bearing boxes collected from one top-level box.
const MAX_PATCHES: usize = 1 << 20;
/// Largest box (`iloc`) read into memory to rewrite.
const MAX_IN_MEMORY_BOX: u64 = 16 << 20;

/// A run of source bytes that is rewritten while copying.
#[derive(Debug)]
enum Patch {
    /// `count` fixed-size entries, each with one absolute offset.
    Table {
        start: u64,
        count: u64,
        stride: u32,
        field_off: u32,
        width: u8,
    },
    /// Replacement bytes of the same length.
    Bytes { at: u64, data: Vec<u8> },
}

impl Patch {
    fn start(&self) -> u64 {
        match self {
            Patch::Table { start, .. } => *start,
            Patch::Bytes { at, .. } => *at,
        }
    }

    fn len(&self) -> u64 {
        match self {
            // Bounded when the patch is built.
            Patch::Table { count, stride, .. } => count * *stride as u64,
            Patch::Bytes { data, .. } => data.len() as u64,
        }
    }
}

/// Boxes of these types can contain offset-bearing boxes.
fn may_hold_offsets(name: Name) -> bool {
    matches!(name, Name::FourCC(t) if t == *b"moov" || t == *b"moof" || t == *b"mfra" || t == *b"meta")
}

#[derive(Debug, Clone, Copy)]
enum Table {
    Stco,
    Co64,
    Saio,
    Iloc,
    Tfhd,
    Tfra,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    table: Table,
    end: u64,
    /// First byte after the FullBox version and flags.
    content: u64,
    version: u8,
    flags: u32,
}

async fn read_u32_at(io: &mut Io, at: u64) -> Result<u32> {
    let mut b = [0u8; 4];
    io.read_at(at, &mut b).await?;
    Ok(u32::from_be_bytes(b))
}

/// Find the offset-bearing boxes inside one top-level box by reading box headers,
/// then read just their table headers.
async fn collect_patches(io: &mut Io, seg: &Segment, map: &OffsetMap) -> Result<Vec<Patch>> {
    let mut cands: Vec<Candidate> = Vec::new();
    let ext = seg.extent();
    walk(io, ext.offset(), ext.end(), &mut |n: &Node<'_>| {
        let table = match n.path {
            "/moov/trak/mdia/minf/stbl/stco" => Table::Stco,
            "/moov/trak/mdia/minf/stbl/co64" => Table::Co64,
            "/moov/trak/mdia/minf/stbl/saio" => Table::Saio,
            "/meta/iloc" => Table::Iloc,
            "/moof/traf/tfhd" => Table::Tfhd,
            "/mfra/tfra" => Table::Tfra,
            _ => return Ok(()),
        };
        if cands.len() >= MAX_PATCHES {
            return Err(Error::LimitExceeded {
                what: "offset tables in one box",
                limit: MAX_PATCHES as u64,
            });
        }
        let (Some(version), Some(flags)) = (n.version, n.flags) else {
            return Err(Error::invalid_segment(
                n.extent.offset(),
                "offset table is not a FullBox",
            ));
        };
        cands.push(Candidate {
            table,
            end: n.extent.end(),
            content: n.extent.offset() + n.header_len as u64 + 4,
            version,
            flags,
        });
        Ok(())
    })
    .await?;

    let mut patches = Vec::with_capacity(cands.len());
    for c in cands {
        if let Some(p) = build_patch(io, &c, map).await? {
            patches.push(p);
        }
    }
    patches.sort_by_key(Patch::start);
    for w in patches.windows(2) {
        if w[0].start().saturating_add(w[0].len()) > w[1].start() {
            return Err(Error::invalid_segment(
                w[1].start(),
                "overlapping offset tables",
            ));
        }
    }
    Ok(patches)
}

/// A table of `count` entries of `stride` bytes at `start` must fit before `end`.
fn table(
    start: u64,
    count: u64,
    stride: u32,
    field_off: u32,
    width: u8,
    end: u64,
) -> Result<Option<Patch>> {
    let len = count
        .checked_mul(stride as u64)
        .and_then(|l| start.checked_add(l))
        .filter(|&e| e <= end)
        .ok_or_else(|| Error::invalid_segment(start, "offset table extends past its box"))?;
    debug_assert!(len >= start);
    Ok(Some(Patch::Table {
        start,
        count,
        stride,
        field_off,
        width,
    }))
}

async fn build_patch(io: &mut Io, c: &Candidate, map: &OffsetMap) -> Result<Option<Patch>> {
    let room = |need: u64| -> Result<()> {
        if c.end.saturating_sub(c.content) < need {
            return Err(Error::invalid_segment(
                c.content,
                "offset table box is truncated",
            ));
        }
        Ok(())
    };
    match c.table {
        Table::Stco | Table::Co64 => {
            room(4)?;
            let w: u8 = if matches!(c.table, Table::Stco) { 4 } else { 8 };
            let count = read_u32_at(io, c.content).await? as u64;
            table(c.content + 4, count, w as u32, 0, w, c.end)
        }
        Table::Saio => {
            let skip = if c.flags & 1 == 1 { 8 } else { 0 };
            room(skip + 4)?;
            let count = read_u32_at(io, c.content + skip).await? as u64;
            let w: u8 = if c.version == 0 { 4 } else { 8 };
            table(c.content + skip + 4, count, w as u32, 0, w, c.end)
        }
        Table::Tfhd => {
            if c.flags & 1 == 0 {
                return Ok(None); // no base data offset
            }
            room(12)?; // track id, base data offset
            table(c.content + 4, 1, 8, 0, 8, c.end)
        }
        Table::Tfra => {
            room(12)?;
            let info = read_u32_at(io, c.content + 4).await?;
            let count = read_u32_at(io, c.content + 8).await? as u64;
            let w: u32 = if c.version == 1 { 8 } else { 4 };
            let sizes = ((info >> 4) & 3) + ((info >> 2) & 3) + (info & 3) + 3;
            table(c.content + 12, count, 2 * w + sizes, w, w as u8, c.end)
        }
        Table::Iloc => {
            let len = c.end.saturating_sub(c.content);
            if len > MAX_IN_MEMORY_BOX {
                return Err(Error::LimitExceeded {
                    what: "iloc box size",
                    limit: MAX_IN_MEMORY_BOX,
                });
            }
            let mut data = io.read_vec(c.content, len as usize).await?;
            adjust_iloc(&mut data, c.version, map)?;
            Ok(Some(Patch::Bytes {
                at: c.content,
                data,
            }))
        }
    }
}

fn put_uint(dst: &mut [u8], v: u64) -> Result<()> {
    let range = || Error::InvalidFormat("offset adjustment does not fit its field".into());
    match dst.len() {
        4 => dst.copy_from_slice(&u32::try_from(v).map_err(|_| range())?.to_be_bytes()),
        8 => dst.copy_from_slice(&v.to_be_bytes()),
        _ => return Err(range()),
    }
    Ok(())
}

fn get_uint(src: &[u8]) -> u64 {
    src.iter().fold(0u64, |a, &b| (a << 8) | b as u64)
}

/// Rewrite file offsets in an `iloc` body (after version and flags), in place.
/// With a base offset, the base moves and the extents stay relative to it;
/// without one, the extent offsets are absolute and move. Items that are not
/// file-offset constructed are left alone.
fn adjust_iloc(b: &mut [u8], version: u8, map: &OffsetMap) -> Result<()> {
    if version > 2 {
        return Err(Error::InvalidFormat("unknown iloc version".into()));
    }
    // `next(n, b)` advances over `n` bytes and returns where they start.
    let mut pos = 0usize;
    let mut next = |n: usize, b: &[u8]| -> Result<usize> {
        let s = pos;
        pos = s
            .checked_add(n)
            .filter(|&e| e <= b.len())
            .ok_or_else(|| Error::invalid_segment(0, "iloc box is truncated"))?;
        Ok(s)
    };

    let s = next(2, b)?;
    let (a, b1) = (b[s], b[s + 1]);
    let offset_size = (a >> 4) as usize;
    let length_size = (a & 15) as usize;
    let base_size = (b1 >> 4) as usize;
    let index_size = if version >= 1 { (b1 & 15) as usize } else { 0 };
    for sz in [offset_size, length_size, base_size, index_size] {
        if !matches!(sz, 0 | 4 | 8) {
            return Err(Error::InvalidFormat("unsupported iloc field size".into()));
        }
    }
    let item_count = if version < 2 {
        let s = next(2, b)?;
        get_uint(&b[s..s + 2])
    } else {
        let s = next(4, b)?;
        get_uint(&b[s..s + 4])
    };
    for _ in 0..item_count {
        next(if version < 2 { 2 } else { 4 }, b)?; // item id
        let construction = if version >= 1 {
            let s = next(2, b)?;
            b[s + 1] & 0x0f
        } else {
            0
        };
        next(2, b)?; // data reference index
        let base_at = next(base_size, b)?;
        let base = get_uint(&b[base_at..base_at + base_size]);
        let s = next(2, b)?;
        let extent_count = get_uint(&b[s..s + 2]);
        if construction == 0 && base_size > 0 && base != 0 {
            let v = map.apply(base)?;
            put_uint(&mut b[base_at..base_at + base_size], v)?;
        }
        for _ in 0..extent_count {
            next(index_size, b)?;
            let at = next(offset_size, b)?;
            if construction == 0 && (base_size == 0 || base == 0) && offset_size > 0 {
                let v = get_uint(&b[at..at + offset_size]);
                if v != 0 {
                    let v = map.apply(v)?;
                    put_uint(&mut b[at..at + offset_size], v)?;
                }
            }
            next(length_size, b)?;
        }
    }
    Ok(())
}

/// Size and type, then `prefix`, for a box whose payload is `payload_len` bytes.
fn box_header(ty: &[u8; 4], prefix: &[u8], payload_len: u64) -> Result<Vec<u8>> {
    let too_big = || Error::LimitExceeded {
        what: "box size",
        limit: u64::MAX,
    };
    let inner = (prefix.len() as u64)
        .checked_add(payload_len)
        .ok_or_else(too_big)?;
    let mut h = Vec::with_capacity(16 + prefix.len());
    match u32::try_from(inner.checked_add(8).ok_or_else(too_big)?) {
        Ok(size) => {
            h.extend_from_slice(&size.to_be_bytes());
            h.extend_from_slice(ty);
        }
        Err(_) => {
            let size = inner.checked_add(16).ok_or_else(too_big)?;
            h.extend_from_slice(&1u32.to_be_bytes());
            h.extend_from_slice(ty);
            h.extend_from_slice(&size.to_be_bytes());
        }
    }
    h.extend_from_slice(prefix);
    Ok(h)
}

/// Header of a C2PA box up to the manifest store: the merkle offset is written
/// as 0, as merkle boxes are not produced.
fn c2pa_box_header(purpose: &str, payload_len: u64) -> Result<Vec<u8>> {
    let mut prefix = C2PA_UUID.to_vec();
    prefix.extend_from_slice(&[0u8; 4]); // version and flags
    prefix.extend_from_slice(purpose.as_bytes());
    prefix.push(0);
    prefix.extend_from_slice(&[0u8; 8]); // offset of the first merkle box
    box_header(&UUID, &prefix, payload_len)
}

fn xmp_box_header(payload_len: u64) -> Result<Vec<u8>> {
    box_header(&UUID, &XMP_UUID, payload_len)
}

enum Op<'a> {
    /// Copy a source segment unchanged.
    Copy(SegmentId),
    /// Copy a source segment, rewriting the offsets inside it.
    Scan(SegmentId),
    /// Copy a C2PA box with its purpose changed to another of the same length.
    Relabel {
        id: SegmentId,
        from: &'static str,
        to: &'static str,
    },
    /// Emit a new box.
    New {
        kind: SegmentKind,
        label: Option<&'static str>,
        header: Vec<u8>,
        payload: Cow<'a, [u8]>,
    },
}

/// Record that the region ending at `old_end` changed from `old_len` to `new_len`
/// bytes. Regions at the end of the file move nothing.
fn note(map: &mut OffsetMap, total: u64, old_end: u64, new_len: u64, old_len: u64) -> Result<()> {
    if old_end >= total {
        return Ok(());
    }
    let delta = i64::try_from(new_len as i128 - old_len as i128)
        .map_err(|_| Error::InvalidFormat("box size change out of range".into()))?;
    map.add(old_end, delta);
    Ok(())
}

fn new_c2pa(d: &[u8]) -> Result<Op<'_>> {
    new_store_box(MANIFEST, Cow::Borrowed(d))
}

fn new_store_box<'a>(purpose: &'static str, d: Cow<'a, [u8]>) -> Result<Op<'a>> {
    Ok(Op::New {
        kind: SegmentKind::Jumbf,
        label: Some(purpose),
        header: c2pa_box_header(purpose, d.len() as u64)?,
        payload: d,
    })
}

/// A merkle box: C2PA type, version and flags, the purpose, then the payload
/// (no merkle offset field, unlike the store boxes).
fn new_merkle(d: &[u8]) -> Result<Op<'_>> {
    let mut prefix = C2PA_UUID.to_vec();
    prefix.extend_from_slice(&[0u8; 4]);
    prefix.extend_from_slice(MERKLE.as_bytes());
    prefix.push(0);
    Ok(Op::New {
        kind: SegmentKind::Other,
        label: Some(MERKLE),
        header: box_header(&UUID, &prefix, d.len() as u64)?,
        payload: Cow::Borrowed(d),
    })
}

fn new_xmp(d: &[u8]) -> Result<Op<'_>> {
    Ok(Op::New {
        kind: SegmentKind::Xmp,
        label: None,
        header: xmp_box_header(d.len() as u64)?,
        payload: Cow::Borrowed(d),
    })
}

/// The bytes of `entity`, its parts joined, read through `io`.
async fn read_entity(src: &Structure, io: &mut Io, entity: &Entity) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for part in entity.parts() {
        let r = part.range();
        if r.end() > src.total_size() {
            return Err(Error::invalid_segment(
                r.offset(),
                "range outside the asset",
            ));
        }
        if (out.len() as u64).saturating_add(r.len()) > MAX_SEGMENT_SIZE {
            return Err(Error::LimitExceeded {
                what: "read size",
                limit: MAX_SEGMENT_SIZE,
            });
        }
        out.extend(io.read_vec(r.offset(), r.len() as usize).await?);
    }
    Ok(out)
}

fn op_len(op: &Op<'_>, src: &Structure) -> Result<u64> {
    Ok(match op {
        Op::Copy(id) | Op::Scan(id) | Op::Relabel { id, .. } => src.segment(*id)?.extent().len(),
        Op::New {
            header, payload, ..
        } => header.len() as u64 + payload.len() as u64,
    })
}

/// Decide what the output contains: the single source of truth for both the
/// destination structure and the bytes written.
async fn plan<'a>(
    src: &Structure,
    io: &mut Io,
    updates: &'a Updates,
) -> Result<(Vec<Op<'a>>, Vec<Option<&'static str>>, OffsetMap)> {
    let segs = src.segments();
    let mut labels: Vec<Option<&'static str>> = vec![None; segs.len()];
    for (_, e) in src.entity_list() {
        if let (Some(l), Some(p)) = (e.label(), e.parts().next()) {
            labels[p.segment().index()] = purpose_label(l.as_bytes());
        }
    }
    let has_store = labels
        .iter()
        .any(|l| matches!(l, Some(MANIFEST) | Some(ORIGINAL)));
    let has_xmp = segs.iter().any(|s| s.kind() == SegmentKind::Xmp);

    // An update manifest: the new store begins with the store already in the file,
    // so that store is left as it is (as the original) and only what follows it is
    // written, as the update. Anything else is written as a whole.
    let frozen = src
        .entities_labelled(SegmentKind::Jumbf, MANIFEST)
        .next()
        .or_else(|| src.entities_labelled(SegmentKind::Jumbf, ORIGINAL).next());
    let mut addition: Option<Vec<u8>> = match (updates.jumbf(), updates.is_jumbf_update(), frozen) {
        (MetadataUpdate::Set(full), true, Some(e)) => {
            let existing = read_entity(src, io, e).await?;
            crate::jumbf::addition_to(&existing, full)?
        }
        _ => None,
    };
    let update_mode = addition.is_some();

    let merkle_kept = matches!(updates.merkle(), MerkleUpdate::Keep);
    if update_mode && !merkle_kept {
        return Err(Error::InvalidFormat(
            "an update manifest cannot change merkle boxes".into(),
        ));
    }
    if matches!(updates.jumbf(), MetadataUpdate::Set(_))
        && !update_mode
        && merkle_kept
        && labels.contains(&Some(MERKLE))
    {
        return Err(Error::InvalidFormat(
            "a file with merkle boxes needs them set or removed to take a new manifest".into(),
        ));
    }
    // Merkle boxes go right after the last mdat.
    let last_mdat = segs.iter().rposition(|s| s.name() == Name::FourCC(MDAT));
    if matches!(updates.merkle(), MerkleUpdate::Set(_)) && last_mdat.is_none() {
        return Err(Error::InvalidFormat(
            "BMFF has no mdat box to place merkle boxes after".into(),
        ));
    }

    let total = src.total_size();
    let mut map = OffsetMap::default();
    let mut ops: Vec<Op<'a>> = Vec::with_capacity(segs.len() + 2);
    let mut store = MetadataSlot::new(updates.jumbf());
    let mut xmp = MetadataSlot::new(updates.xmp());
    let mut ftyp_seen = false;

    for (i, seg) in segs.iter().enumerate() {
        let id = SegmentId::from_index(i)?;
        let (old_len, end) = (seg.extent().len(), seg.extent().end());
        match labels[i] {
            Some(MERKLE) => match updates.merkle() {
                MerkleUpdate::Keep => ops.push(Op::Copy(id)),
                _ => note(&mut map, total, end, 0, old_len)?,
            },
            // The first update: the store becomes the original, unchanged but for
            // its purpose, and the update is added after everything.
            Some(MANIFEST) if update_mode => ops.push(Op::Relabel {
                id,
                from: MANIFEST,
                to: ORIGINAL,
            }),
            Some(ORIGINAL) if update_mode => ops.push(Op::Copy(id)),
            // Later updates replace the update store: it holds all of them.
            Some(UPDATE) if update_mode => {
                let add = addition.take().ok_or(Error::NotFound {
                    what: "update store",
                })?;
                let op = new_store_box(UPDATE, Cow::Owned(add))?;
                note(&mut map, total, end, op_len(&op, src)?, old_len)?;
                ops.push(op);
            }
            Some(MANIFEST) | Some(ORIGINAL) => match store.existing() {
                Existing::Keep => ops.push(Op::Copy(id)),
                Existing::Replace(d) => {
                    let op = new_c2pa(d)?;
                    note(&mut map, total, end, op_len(&op, src)?, old_len)?;
                    ops.push(op);
                }
                _ => note(&mut map, total, end, 0, old_len)?,
            },
            // An update box does not survive a new manifest.
            Some(UPDATE) => match updates.jumbf() {
                MetadataUpdate::Keep => ops.push(Op::Copy(id)),
                _ => note(&mut map, total, end, 0, old_len)?,
            },
            _ if seg.kind() == SegmentKind::Xmp => match xmp.existing() {
                Existing::Keep => ops.push(Op::Copy(id)),
                Existing::Replace(d) => {
                    let op = new_xmp(d)?;
                    note(&mut map, total, end, op_len(&op, src)?, old_len)?;
                    ops.push(op);
                }
                _ => note(&mut map, total, end, 0, old_len)?,
            },
            _ => {
                ops.push(Op::Copy(id));
                if !ftyp_seen && seg.name() == Name::FourCC(FTYP) {
                    ftyp_seen = true;
                    if !has_store {
                        if let Some(d) = store.pending() {
                            let op = new_c2pa(d)?;
                            note(&mut map, total, end, op_len(&op, src)?, 0)?;
                            ops.push(op);
                        }
                    }
                    if !has_xmp {
                        if let Some(d) = xmp.pending() {
                            let op = new_xmp(d)?;
                            note(&mut map, total, end, op_len(&op, src)?, 0)?;
                            ops.push(op);
                        }
                    }
                }
            }
        }
        if let (Some(i2), MerkleUpdate::Set(boxes)) = (last_mdat, updates.merkle()) {
            if i2 == i {
                let mut added = 0u64;
                for d in boxes {
                    let op = new_merkle(d)?;
                    added = added
                        .checked_add(op_len(&op, src)?)
                        .ok_or(Error::LimitExceeded {
                            what: "output size",
                            limit: u64::MAX,
                        })?;
                    ops.push(op);
                }
                note(&mut map, total, end, added, 0)?;
            }
        }
    }
    if let Some(add) = addition.take() {
        // No update store existed: add one after everything else, so nothing that
        // exists moves.
        ops.push(new_store_box(UPDATE, Cow::Owned(add))?);
    }
    if (!update_mode && store.is_pending()) || xmp.is_pending() {
        return Err(Error::InvalidFormat(
            "BMFF has no ftyp box to insert metadata after".into(),
        ));
    }

    // Now that every edit is known, mark the boxes whose offsets must move.
    if !map.is_identity() {
        for op in ops.iter_mut() {
            if let Op::Copy(id) = *op {
                if may_hold_offsets(src.segment(id)?.name()) {
                    *op = Op::Scan(id);
                }
            }
        }
    }

    Ok((ops, labels, map))
}

/// A box whose size field is 0 runs to the end of the file, so only the last box of
/// the source can be one. If the write puts anything after it, it needs a size of
/// its own, or it would swallow what follows. Returns that box and its size.
async fn open_ended_box(
    src: &Structure,
    io: &mut Io,
    ops: &[Op<'_>],
) -> Result<Option<(SegmentId, u32)>> {
    let Some(last) = src.segments().len().checked_sub(1) else {
        return Ok(None);
    };
    let at = ops.iter().position(|op| {
        matches!(op, Op::Copy(id) | Op::Scan(id) | Op::Relabel { id, .. } if id.index() == last)
    });
    if !at.is_some_and(|at| at + 1 < ops.len()) {
        return Ok(None);
    }
    let id = SegmentId::from_index(last)?;
    let seg = src.segment(id)?;
    let mut size = [0u8; 4];
    io.read_at(seg.header().offset(), &mut size).await?;
    if size != [0; 4] {
        return Ok(None);
    }
    let size = u32::try_from(seg.extent().len()).map_err(|_| Error::LimitExceeded {
        what: "size of a box that no longer runs to the end of the file",
        limit: u32::MAX as u64,
    })?;
    Ok(Some((id, size)))
}

/// The header of `seg` with `size` as its size field.
fn put_size(b: &mut PlanBuilder<'_, '_>, id: SegmentId, seg: &Segment, size: u32) -> Result<()> {
    let header = seg.header();
    b.put(id, Role::Header, size.to_be_bytes().to_vec());
    b.copy(id, Role::Header, header.sub(4, header.len() - 4)?);
    Ok(())
}

/// The plan that writes `ops`: the output structure and, unless `with_steps` is
/// false, the steps. Reads the source for what the steps need to know: the offset
/// tables to rewrite and the header of a relabelled box.
async fn assemble<'a>(
    src: &Structure,
    io: &mut Io,
    ops: Vec<Op<'a>>,
    labels: &[Option<&'static str>],
    map: OffsetMap,
    with_steps: bool,
) -> Result<WritePlan<'a>> {
    let map = Arc::new(map);
    let mut b = PlanBuilder::new(src);
    let open_ended = if with_steps {
        open_ended_box(src, io, &ops).await?
    } else {
        None
    };
    for op in ops {
        let (id, kind, label) = match op {
            Op::Copy(sid) => {
                let s = src.segment(sid)?;
                let id = match open_ended.filter(|(open, _)| *open == sid) {
                    Some((_, size)) => {
                        let id = b.shape_like(sid)?;
                        put_size(&mut b, id, s, size)?;
                        b.copy(id, Role::Payload, s.payload());
                        id
                    }
                    None => b.copy_segment(sid)?,
                };
                (id, s.kind(), labels[sid.index()])
            }
            Op::Scan(sid) => {
                let seg = src.segment(sid)?;
                let id = b.shape_like(sid)?;
                if with_steps {
                    let mut patches = collect_patches(io, seg, &map).await?;
                    for (role, range) in seg.regions() {
                        match open_ended.filter(|(open, _)| *open == sid) {
                            Some((_, size)) if role == Role::Header => {
                                put_size(&mut b, id, seg, size)?
                            }
                            _ => patched_steps(&mut b, id, role, range, &mut patches, &map)?,
                        }
                    }
                }
                (id, seg.kind(), labels[sid.index()])
            }
            Op::Relabel { id: sid, from, to } => {
                let seg = src.segment(sid)?;
                let id = b.shape_like(sid)?;
                if with_steps {
                    let header = seg.header();
                    // [size, type] [uuid 16] [version, flags 4] [purpose NUL] [merkle offset 8]
                    let fixed = 16 + 4 + from.len() + 1 + 8;
                    let prefix = (header.len() as usize)
                        .checked_sub(fixed)
                        .filter(|p| *p == 8 || *p == 16)
                        .ok_or_else(|| {
                            Error::invalid_segment(header.offset(), "unexpected C2PA box header")
                        })?;
                    let mut h = vec![0u8; header.len() as usize];
                    io.read_at(header.offset(), &mut h).await?;
                    let at = prefix + 16 + 4;
                    if &h[at..at + from.len()] != from.as_bytes() || h[at + from.len()] != 0 {
                        return Err(Error::invalid_segment(
                            header.offset(),
                            "C2PA box purpose is not what was parsed",
                        ));
                    }
                    h[at..at + to.len()].copy_from_slice(to.as_bytes());
                    if let Some((_, size)) = open_ended.filter(|(open, _)| *open == sid) {
                        h[..4].copy_from_slice(&size.to_be_bytes());
                    }
                    b.put(id, Role::Header, h);
                    b.copy(id, Role::Payload, seg.payload());
                }
                (id, seg.kind(), Some(to))
            }
            Op::New {
                kind,
                label,
                header,
                payload,
            } => (
                b.put_segment(kind, Name::FourCC(UUID), header, payload, &[][..])?,
                kind,
                label,
            ),
        };
        match (label, kind) {
            (Some(l), _) => b.entity_labelled(kind, l, &[id])?,
            (None, SegmentKind::Xmp) => b.entity(SegmentKind::Xmp, &[id])?,
            _ => {}
        }
    }
    // Items (EXIF, thumbnails) are bytes inside boxes that are copied as they are,
    // so their entities follow the boxes to wherever those land.
    b.carry_entities(|k| matches!(k, SegmentKind::Exif | SegmentKind::Thumbnail));
    b.finish()
}

/// Append the steps that copy `range`, rewriting the patches inside it. `patches`
/// are sorted and disjoint; those outside `range` are skipped.
fn patched_steps(
    plan: &mut PlanBuilder<'_, '_>,
    dest: SegmentId,
    role: Role,
    range: ByteRange,
    patches: &mut [Patch],
    map: &Arc<OffsetMap>,
) -> Result<()> {
    let mut cur = range.offset();
    for p in patches {
        let (start, len) = (p.start(), p.len());
        if start < range.offset() || start.saturating_add(len) > range.end() {
            continue;
        }
        plan.copy(dest, role, ByteRange::new(cur, start - cur)?);
        match p {
            Patch::Bytes { data, .. } => {
                plan.put(dest, role, std::mem::take(data));
            }
            Patch::Table {
                stride,
                field_off,
                width,
                ..
            } => {
                let rule = Relocate::new(map.clone(), *stride, *field_off, *width)
                    .map_err(|_| Error::invalid_segment(start, "invalid offset table geometry"))?;
                plan.relocate(dest, role, ByteRange::new(start, len)?, rule);
            }
        }
        cur = start + len;
    }
    plan.copy(dest, role, ByteRange::new(cur, range.end() - cur)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(body);
        v
    }

    fn c2pa_box(purpose: &str, tail: &[u8]) -> Vec<u8> {
        let mut body = C2PA_UUID.to_vec();
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(purpose.as_bytes());
        body.push(0);
        if purpose != MERKLE {
            body.extend_from_slice(&[0u8; 8]);
        }
        body.extend_from_slice(tail);
        boxed(b"uuid", &body)
    }

    fn ftyp() -> Vec<u8> {
        boxed(b"ftyp", b"isom\0\0\0\0isom")
    }

    fn file(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn walk_paths(s: &Structure, data: &[u8]) -> Vec<(String, u64, u64)> {
        let mut out = Vec::new();
        s.walk(&mut Cursor::new(data), |n| {
            out.push((n.path.to_string(), n.extent.offset(), n.extent.len()));
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn tiles_and_labels_c2pa_boxes() {
        let data = file(&[
            ftyp(),
            c2pa_box(MANIFEST, b"STORE"),
            boxed(b"moov", &boxed(b"free", b"")),
            boxed(b"mdat", b"media"),
        ]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        assert_eq!(s.media_type(), "video/mp4");
        let covered: u64 = s.regions().map(|r| r.range.len()).sum();
        assert_eq!(covered, data.len() as u64);

        let e = s
            .entities_labelled(SegmentKind::Jumbf, MANIFEST)
            .next()
            .unwrap();
        assert_eq!(s.read_entity(&mut Cursor::new(&data), e).unwrap(), b"STORE");
        assert!(s
            .entities_labelled(SegmentKind::Jumbf, UPDATE)
            .next()
            .is_none());
    }

    #[test]
    fn walk_reports_nested_paths() {
        let trak = boxed(b"trak", &boxed(b"mdia", &boxed(b"hdlr", &[0u8; 8])));
        let data = file(&[ftyp(), boxed(b"moov", &trak), boxed(b"mdat", b"x")]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        let w = walk_paths(&s, &data);
        let paths: Vec<_> = w.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/ftyp",
                "/moov",
                "/moov/trak",
                "/moov/trak/mdia",
                "/moov/trak/mdia/hdlr",
                "/mdat"
            ]
        );
        // hdlr is a FullBox: version and flags are reported, not part of header_len.
        let mut seen = false;
        s.walk(&mut Cursor::new(&data), |n| {
            if n.path.ends_with("hdlr") {
                assert_eq!((n.version, n.flags, n.header_len), (Some(0), Some(0), 8));
                seen = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(seen);
    }

    #[test]
    fn quicktime_meta_without_full_box_header() {
        let meta = boxed(b"meta", &boxed(b"hdlr", &[0u8; 8]));
        let data = file(&[ftyp(), boxed(b"moov", &boxed(b"udta", &meta))]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        let w = walk_paths(&s, &data);
        assert!(w.iter().any(|(p, _, _)| p == "/moov/udta/meta/hdlr"));
    }

    #[test]
    fn mdat_overrun_is_clamped_others_are_errors() {
        let mut mdat = boxed(b"mdat", b"abcdef");
        mdat[3] = 200; // size larger than the file
        let data = file(&[ftyp(), mdat]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        assert_eq!(s.total_size(), data.len() as u64);

        let mut moov = boxed(b"moov", b"");
        moov[3] = 200;
        assert!(parse(&mut Cursor::new(file(&[ftyp(), moov]))).is_err());
    }

    #[test]
    fn size_zero_runs_to_end_and_large_size_is_read() {
        let mut mdat = boxed(b"mdat", b"abc");
        mdat[..4].copy_from_slice(&0u32.to_be_bytes());
        let data = file(&[ftyp(), mdat]);
        parse(&mut Cursor::new(&data)).unwrap();

        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"free");
        large.extend_from_slice(&20u64.to_be_bytes());
        large.extend_from_slice(b"1234");
        let data = file(&[ftyp(), large]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        let free = &s.segments()[1];
        assert_eq!((free.header().len(), free.payload().len()), (16, 4));
    }

    #[test]
    fn trailing_bytes_become_unnamed_data() {
        let data = file(&[ftyp(), vec![1, 2, 3]]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        assert!(s.segments().last().unwrap().name().is_none());
    }

    #[test]
    fn hostile_headers_are_rejected() {
        for bad_size in [2u32, 7] {
            let mut b = boxed(b"free", b"");
            b[..4].copy_from_slice(&bad_size.to_be_bytes());
            assert!(parse(&mut Cursor::new(file(&[ftyp(), b]))).is_err());
        }
        // Extended size smaller than its own header.
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"free");
        large.extend_from_slice(&4u64.to_be_bytes());
        assert!(parse(&mut Cursor::new(file(&[ftyp(), large]))).is_err());
        // uuid box too short to hold its type.
        assert!(parse(&mut Cursor::new(file(&[ftyp(), boxed(b"uuid", &[0u8; 4])]))).is_err());
        // Nesting past the limit.
        let mut nested = boxed(b"free", b"");
        for _ in 0..MAX_BOX_DEPTH + 1 {
            nested = boxed(b"trak", &nested);
        }
        let data = file(&[ftyp(), nested]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        assert!(s.walk(&mut Cursor::new(&data), |_| Ok(())).is_err());
    }

    #[test]
    fn c2pa_box_rules() {
        let parse_of = |parts: &[Vec<u8>]| parse(&mut Cursor::new(file(parts)));
        let (m, o, u, k) = (
            c2pa_box(MANIFEST, b"A"),
            c2pa_box(ORIGINAL, b"B"),
            c2pa_box(UPDATE, b"C"),
            c2pa_box(MERKLE, b"D"),
        );
        assert!(parse_of(&[ftyp(), m.clone(), k.clone()]).is_ok());
        assert!(parse_of(&[ftyp(), o.clone(), u.clone()]).is_ok());
        assert!(parse_of(&[ftyp(), m.clone(), m.clone()]).is_err());
        assert!(parse_of(&[ftyp(), u.clone(), u.clone(), o.clone()]).is_err());
        assert!(parse_of(&[ftyp(), o]).is_err());
        assert!(parse_of(&[ftyp(), u.clone()]).is_err());
        assert!(parse_of(&[ftyp(), m, u]).is_err());
        assert!(parse_of(&[ftyp(), c2pa_box("bogus", b"")]).is_err());

        // No NUL within the purpose limit.
        let mut body = C2PA_UUID.to_vec();
        body.extend_from_slice(&[0u8; 4]);
        body.extend_from_slice(&[b'a'; 64]);
        assert!(parse_of(&[ftyp(), boxed(b"uuid", &body)]).is_err());
    }

    #[test]
    fn xmp_uuid_box_is_an_entity() {
        let mut body = XMP_UUID.to_vec();
        body.extend_from_slice(b"<xmp/>");
        let data = file(&[ftyp(), boxed(b"uuid", &body)]);
        let s = parse(&mut Cursor::new(&data)).unwrap();
        let e = s.require_entity(SegmentKind::Xmp).unwrap();
        assert_eq!(
            s.read_entity(&mut Cursor::new(&data), e).unwrap(),
            b"<xmp/>"
        );
    }

    #[test]
    fn media_type_follows_brand() {
        let data = file(&[boxed(b"ftyp", b"avif\0\0\0\0avif")]);
        assert_eq!(
            parse(&mut Cursor::new(&data)).unwrap().media_type(),
            "image/avif"
        );
    }

    // ---- writing ----

    use crate::testing::Conformance;
    use crate::{Chunk, NullSink};

    fn full(ty: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![version];
        b.extend_from_slice(&flags.to_be_bytes()[1..]);
        b.extend_from_slice(body);
        boxed(ty, &b)
    }

    fn moov_with(stco: &[u8]) -> Vec<u8> {
        boxed(
            b"moov",
            &boxed(
                b"trak",
                &boxed(b"mdia", &boxed(b"minf", &boxed(b"stbl", stco))),
            ),
        )
    }

    /// `ftyp`, `before`, a `moov` whose one chunk offset points at the media,
    /// `mdat` holding `MEDIA`, then `tail`. Returns the file and the true offset
    /// of the media.
    fn media_file(before: &[Vec<u8>], tail: &[Vec<u8>], entry: Option<u32>) -> (Vec<u8>, u32) {
        let build = |e: u32| {
            let mut body = 1u32.to_be_bytes().to_vec();
            body.extend_from_slice(&e.to_be_bytes());
            let mut parts = vec![ftyp()];
            parts.extend_from_slice(before);
            parts.push(moov_with(&full(b"stco", 0, 0, &body)));
            parts.push(boxed(b"mdat", b"MEDIA"));
            parts.extend_from_slice(tail);
            file(&parts)
        };
        let probe = build(0);
        let tail_len: usize = tail.iter().map(Vec::len).sum();
        let media = (probe.len() - tail_len - 5) as u32;
        (build(entry.unwrap_or(media)), media)
    }

    fn rewrite(src: &[u8], u: &Updates) -> Result<Vec<u8>> {
        let s = parse(&mut Cursor::new(src))?;
        let mut out = Cursor::new(Vec::new());
        let dest = s
            .handler()
            .write(&s, &mut Cursor::new(src), &mut out, u, &mut NullSink)?;
        let out = out.into_inner();
        // The structure returned is the structure of what was written.
        let again = parse(&mut Cursor::new(&out))?;
        assert_eq!(dest.segments(), again.segments());
        assert_eq!(dest.total_size(), out.len() as u64);
        Ok(out)
    }

    fn find(bytes: &[u8], path: &str) -> (u64, u64, u32) {
        let s = parse(&mut Cursor::new(bytes)).unwrap();
        let mut hit = None;
        s.walk(&mut Cursor::new(bytes), |n| {
            if n.path == path && hit.is_none() {
                hit = Some((n.extent.offset(), n.extent.len(), n.header_len));
            }
            Ok(())
        })
        .unwrap();
        hit.unwrap_or_else(|| panic!("{path} not found"))
    }

    fn be32(b: &[u8], at: u64) -> u32 {
        u32::from_be_bytes(b[at as usize..at as usize + 4].try_into().unwrap())
    }

    fn stco_entry(b: &[u8]) -> u32 {
        let (off, _, hl) = find(b, "/moov/trak/mdia/minf/stbl/stco");
        be32(b, off + hl as u64 + 4 + 4)
    }

    fn media_at(b: &[u8], at: u32) -> bool {
        b.get(at as usize..at as usize + 5) == Some(b"MEDIA")
    }

    #[test]
    fn conforms() {
        let c = Conformance::new(handler())
            .jumbf(Some(b"STORE".to_vec()))
            .updates("merkle", Updates::new().set_merkle(vec![vec![0u8; 8]; 2]))
            .updates(
                "manifest and no merkle",
                Updates::new().set_jumbf(b"STORE".to_vec()).remove_merkle(),
            );
        c.check("plain", &media_file(&[], &[], None).0);
        c.check(
            "with a manifest",
            &media_file(&[c2pa_box(MANIFEST, b"A")], &[], None).0,
        );
        c.check(
            "with a manifest and merkle boxes",
            &media_file(&[c2pa_box(MANIFEST, b"A")], &[c2pa_box(MERKLE, b"M")], None).0,
        );
        c.check("fragmented", &fragmented());
        c.check("heif", &heif(EXIF_ITEM, THUMB));
    }

    #[test]
    fn manifest_is_inserted_after_ftyp_and_offsets_follow() {
        let (f, e0) = media_file(&[], &[], None);
        let out = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        assert_eq!(s.segments()[1].name(), Name::FourCC(UUID));
        let e = s
            .entities_labelled(SegmentKind::Jumbf, MANIFEST)
            .next()
            .unwrap();
        assert_eq!(s.read_entity(&mut Cursor::new(&out), e).unwrap(), b"STORE");
        let added = (out.len() - f.len()) as u32;
        assert_eq!(added, 8 + 16 + 4 + 9 + 8 + 5);
        assert_eq!(stco_entry(&out), e0 + added);
        assert!(media_at(&out, stco_entry(&out)));
    }

    #[test]
    fn replacing_in_place_tracks_size_changes() {
        let (f, e0) = media_file(&[], &[], None);
        let one = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();

        // Smaller: offsets shrink with it, position is kept.
        let smaller = rewrite(&one, &Updates::new().set_jumbf(b"ST".to_vec())).unwrap();
        assert_eq!(smaller.len(), one.len() - 3);
        assert_eq!(stco_entry(&smaller), e0 + 47);
        assert!(media_at(&smaller, stco_entry(&smaller)));

        // Same size: only the payload differs and nothing is rewritten.
        let same = rewrite(&one, &Updates::new().set_jumbf(b"STORX".to_vec())).unwrap();
        assert_eq!(same.len(), one.len());
        assert_eq!(stco_entry(&same), stco_entry(&one));
        let diff: Vec<_> = (0..one.len()).filter(|&i| one[i] != same[i]).collect();
        assert_eq!(diff.len(), 1);

        // Removing restores the original file.
        assert_eq!(rewrite(&one, &Updates::new().remove_jumbf()).unwrap(), f);
    }

    #[test]
    fn re_signing_drops_the_update_box() {
        let (f, e0) = media_file(&[c2pa_box(ORIGINAL, b"A")], &[c2pa_box(UPDATE, b"U")], None);
        let out = rewrite(&f, &Updates::new().set_jumbf(b"NEW".to_vec())).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        assert_eq!(s.entities_labelled(SegmentKind::Jumbf, UPDATE).count(), 0);
        assert_eq!(s.entities_labelled(SegmentKind::Jumbf, ORIGINAL).count(), 0);
        assert_eq!(s.entities_labelled(SegmentKind::Jumbf, MANIFEST).count(), 1);
        // "NEW" is two bytes longer than "A" in a box of the same shape.
        assert_eq!(stco_entry(&out), e0 + 2);
        assert!(media_at(&out, stco_entry(&out)));
    }

    #[test]
    fn a_new_manifest_needs_a_decision_about_merkle_boxes() {
        let (m1, m2) = (jman("m1", b"1"), jman("m2", b"2"));
        let first = jstore(std::slice::from_ref(&m1));
        let (f, _) = media_file(
            &[c2pa_box(MANIFEST, &first)],
            &[c2pa_box(MERKLE, b"D")],
            None,
        );
        assert_eq!(rewrite(&f, &Updates::new()).unwrap(), f);
        assert!(rewrite(&f, &Updates::new().set_jumbf(b"B".to_vec())).is_err());
        let removed =
            rewrite(&f, &Updates::new().set_jumbf(b"B".to_vec()).remove_merkle()).unwrap();
        let s = parse(&mut Cursor::new(&removed)).unwrap();
        assert_eq!(s.entities_labelled(SegmentKind::Other, MERKLE).count(), 0);

        // An update manifest leaves the merkle boxes alone, and cannot change them.
        let extended = jstore(&[m1, m2]);
        let keep = rewrite(&f, &Updates::new().set_jumbf(extended.clone()).as_update()).unwrap();
        let s = parse(&mut Cursor::new(&keep)).unwrap();
        assert_eq!(merkle_payloads(&s, &keep), vec![b"D".to_vec()]);
        let u = Updates::new()
            .set_jumbf(extended)
            .as_update()
            .remove_merkle();
        assert!(rewrite(&f, &u).is_err());
    }

    fn merkle_payloads(s: &Structure, data: &[u8]) -> Vec<Vec<u8>> {
        s.entities_labelled(SegmentKind::Other, MERKLE)
            .map(|e| s.read_entity(&mut Cursor::new(data), e).unwrap())
            .collect()
    }

    #[test]
    fn merkle_boxes_follow_the_last_mdat_and_are_filled_in_place() {
        let (f, e0) = media_file(&[], &[boxed(b"free", b"zz")], None);
        let out = rewrite(&f, &Updates::new().set_merkle(vec![vec![0u8; 12]; 3])).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();

        // Three boxes directly after mdat, before the trailing box; nothing before
        // them moved.
        let (mdat_at, mdat_len, _) = find(&out, "/mdat");
        let first = s
            .entities_labelled(SegmentKind::Other, MERKLE)
            .next()
            .unwrap();
        let seg = s.segment(first.parts().next().unwrap().segment()).unwrap();
        assert_eq!(seg.extent().offset(), mdat_at + mdat_len);
        assert_eq!(merkle_payloads(&s, &out).len(), 3);
        assert_eq!(stco_entry(&out), e0);
        assert_eq!(out.len(), f.len() + 3 * (8 + 16 + 4 + 7 + 12));

        // Fill each box in place, as after hashing the media.
        let mut cur = Cursor::new(out.clone());
        for (i, e) in s.entities_labelled(SegmentKind::Other, MERKLE).enumerate() {
            s.patch_exact(&mut cur, e, &[i as u8 + 1; 12]).unwrap();
        }
        let filled = cur.into_inner();
        assert_eq!(filled.len(), out.len());
        let s2 = parse(&mut Cursor::new(&filled)).unwrap();
        assert_eq!(
            merkle_payloads(&s2, &filled),
            vec![vec![1u8; 12], vec![2; 12], vec![3; 12]]
        );

        // Setting again replaces; removing restores the original file.
        let two = rewrite(&filled, &Updates::new().set_merkle(vec![vec![9u8; 4]; 2])).unwrap();
        assert_eq!(
            merkle_payloads(&parse(&mut Cursor::new(&two)).unwrap(), &two).len(),
            2
        );
        assert_eq!(
            rewrite(&filled, &Updates::new().remove_merkle()).unwrap(),
            f
        );
        // Keep is the default.
        assert_eq!(rewrite(&filled, &Updates::new()).unwrap(), filled);
    }

    /// The intended flow: placeholders of final size, media hashed (here: summed)
    /// as it streams past in the same pass, boxes filled in afterwards.
    #[test]
    fn media_seen_by_the_sink_can_fill_the_merkle_boxes() {
        struct Sum {
            mdat: Vec<usize>,
            sum: u64,
            seen: u64,
        }
        impl crate::Sink for Sum {
            fn begin(&mut self, structure: &Structure) -> Result<()> {
                self.mdat = structure
                    .segments()
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.name() == Name::FourCC(MDAT))
                    .map(|(i, _)| i)
                    .collect();
                Ok(())
            }
            fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
                if c.role == Role::Payload && self.mdat.contains(&c.segment.index()) {
                    self.sum += c.data.iter().map(|&b| b as u64).sum::<u64>();
                    self.seen += c.data.len() as u64;
                }
                Ok(())
            }
        }

        let (f, _) = media_file(&[], &[], None);
        let src = parse(&mut Cursor::new(&f)).unwrap();
        let mut out = Cursor::new(Vec::new());
        let mut sink = Sum {
            mdat: vec![],
            sum: 0,
            seen: 0,
        };
        let u = Updates::new().set_merkle(vec![vec![0u8; 8]]);
        let dest = src
            .handler()
            .write(&src, &mut Cursor::new(&f), &mut out, &u, &mut sink)
            .unwrap();
        assert_eq!(sink.seen, 5);
        assert_eq!(sink.sum, b"MEDIA".iter().map(|&b| b as u64).sum::<u64>());

        let e = dest
            .entities_labelled(SegmentKind::Other, MERKLE)
            .next()
            .unwrap();
        dest.patch_exact(&mut out, e, &sink.sum.to_be_bytes())
            .unwrap();
        let bytes = out.into_inner();
        let s = parse(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(
            merkle_payloads(&s, &bytes),
            vec![sink.sum.to_be_bytes().to_vec()]
        );
    }

    #[test]
    fn merkle_and_manifest_together_keep_offsets_right() {
        let (f, e0) = media_file(&[], &[], None);
        let u = Updates::new()
            .set_jumbf(b"STORE".to_vec())
            .set_merkle(vec![vec![7u8; 6]; 2]);
        let out = rewrite(&f, &u).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        assert_eq!(merkle_payloads(&s, &out), vec![vec![7u8; 6]; 2]);
        // The manifest goes in front of the media and moves it; merkle boxes behind
        // the media do not.
        let added = (8 + 16 + 4 + 9 + 8 + 5) as u32;
        assert_eq!(stco_entry(&out), e0 + added);
        assert!(media_at(&out, stco_entry(&out)));
    }

    #[test]
    fn a_box_that_ran_to_the_end_gets_its_size_when_something_follows_it() {
        // The last box, with a size of 0 (to the end of the file), then merkle boxes
        // after it: without a size of its own it would swallow them.
        let mut mdat = boxed(b"mdat", b"MEDIA");
        mdat[..4].copy_from_slice(&0u32.to_be_bytes());
        let f = file(&[ftyp(), mdat]);
        let out = rewrite(&f, &Updates::new().set_merkle(vec![vec![0u8; 8]])).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        let sizes: Vec<_> = s.segments().iter().map(|g| g.extent().len()).collect();
        assert_eq!(sizes[1], 13); // the mdat, now sized: 8 + "MEDIA"
        assert_eq!(s.segments().len(), 3);
        assert_eq!(&out[ftyp().len() + 4..][..4], b"mdat");
        assert_eq!(be32(&out, ftyp().len() as u64), 13);

        // Nothing after it: the header is left as it was.
        let kept = rewrite(&f, &Updates::new()).unwrap();
        assert_eq!(kept, f);
        let manifest_first = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
        let at = ftyp().len() + manifest_first.len() - f.len();
        assert_eq!(be32(&manifest_first, at as u64), 0);
    }

    #[test]
    fn a_first_box_that_ran_to_the_end_is_sized_before_a_box_is_added() {
        // Found by fuzzing: an ftyp of size 0 followed by an inserted XMP box.
        let mut f = ftyp();
        f[..4].copy_from_slice(&0u32.to_be_bytes());
        let out = rewrite(&f, &Updates::new().set_xmp(b"<x/>".to_vec())).unwrap();
        assert_eq!(be32(&out, 0), f.len() as u32);
        assert_eq!(parse(&mut Cursor::new(&out)).unwrap().segments().len(), 2);
    }

    #[test]
    fn merkle_option_defaults_and_remove_all() {
        assert!(matches!(Updates::new().merkle(), MerkleUpdate::Keep));
        assert!(matches!(
            Updates::remove_all().merkle(),
            MerkleUpdate::Remove
        ));
        // An explicit choice beats remove_all.
        assert!(matches!(
            Updates::remove_all().keep_merkle().merkle(),
            MerkleUpdate::Keep
        ));

        // remove_all drops the merkle boxes a file has.
        let (f, _) = media_file(&[], &[], None);
        let with = rewrite(&f, &Updates::new().set_merkle(vec![vec![1u8; 4]])).unwrap();
        let s = parse(&mut Cursor::new(&with)).unwrap();
        assert_eq!(merkle_payloads(&s, &with).len(), 1);
        let out = rewrite(&with, &Updates::remove_all()).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        assert!(merkle_payloads(&s, &out).is_empty());
    }

    #[test]
    fn offset_tables_are_a_relocation_step_not_a_copy() {
        let (f, _) = media_file(&[], &[], None);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let u = Updates::new().set_jumbf(b"STORE".to_vec());
        let plan = s.write_plan(&mut Cursor::new(&f), &u).unwrap();
        crate::plan::validate(&plan, f.len() as u64).unwrap();
        let tables: Vec<_> = plan
            .steps()
            .iter()
            .filter(|st| matches!(st, crate::Step::Relocate { .. }))
            .collect();
        assert_eq!(tables.len(), 1);
        // Planning the structure alone does not read the tables.
        let structure = s.plan(&mut Cursor::new(&f), &u).unwrap();
        assert_eq!(structure.total_size(), plan.structure().total_size());
    }

    #[test]
    fn merkle_boxes_need_an_mdat() {
        let f = file(&[ftyp()]);
        assert!(rewrite(&f, &Updates::new().set_merkle(vec![vec![0u8; 4]])).is_err());
        // Removing from a file without any is fine.
        assert_eq!(rewrite(&f, &Updates::new().remove_merkle()).unwrap(), f);
    }

    #[test]
    fn xmp_is_inserted_replaced_and_removed() {
        let (f, e0) = media_file(&[], &[], None);
        let out = rewrite(&f, &Updates::new().set_xmp(b"<x/>".to_vec())).unwrap();
        let s = parse(&mut Cursor::new(&out)).unwrap();
        let e = s.require_entity(SegmentKind::Xmp).unwrap();
        assert_eq!(s.read_entity(&mut Cursor::new(&out), e).unwrap(), b"<x/>");
        assert_eq!(stco_entry(&out), e0 + 8 + 16 + 4);
        assert!(media_at(&out, stco_entry(&out)));
        assert_eq!(rewrite(&out, &Updates::new().remove_xmp()).unwrap(), f);

        // Both at once, manifest first.
        let both = rewrite(
            &f,
            &Updates::new()
                .set_jumbf(b"S".to_vec())
                .set_xmp(b"<x/>".to_vec()),
        )
        .unwrap();
        let s = parse(&mut Cursor::new(&both)).unwrap();
        assert_eq!(s.segments()[1].kind(), SegmentKind::Jumbf);
        assert_eq!(s.segments()[2].kind(), SegmentKind::Xmp);
        assert!(media_at(&both, stco_entry(&both)));
    }

    #[test]
    fn no_ftyp_means_nowhere_to_insert() {
        let f = file(&[boxed(b"free", b"")]);
        assert!(rewrite(&f, &Updates::new().set_jumbf(b"S".to_vec())).is_err());
        assert_eq!(rewrite(&f, &Updates::new()).unwrap(), f);
    }

    #[test]
    fn offsets_that_cannot_move_are_errors() {
        // 32-bit chunk offset that would pass 4 GiB.
        let (f, _) = media_file(&[], &[], Some(0xFFFF_FFF8));
        assert!(rewrite(&f, &Updates::new().set_jumbf(b"S".to_vec())).is_err());

        // A table claiming more entries than its box holds.
        let mut body = 0xFFFF_FFFFu32.to_be_bytes().to_vec();
        body.extend_from_slice(&[0u8; 4]);
        let f = file(&[
            ftyp(),
            moov_with(&full(b"stco", 0, 0, &body)),
            boxed(b"mdat", b"MEDIA"),
        ]);
        assert!(rewrite(&f, &Updates::new().set_jumbf(b"S".to_vec())).is_err());
    }

    fn jstore(children: &[Vec<u8>]) -> Vec<u8> {
        let mut d = vec![7u8; 16];
        d.push(3);
        d.extend_from_slice(b"c2pa\0");
        let mut body = boxed(b"jumd", &d);
        for c in children {
            body.extend_from_slice(c);
        }
        boxed(b"jumb", &body)
    }

    fn jman(label: &str, data: &[u8]) -> Vec<u8> {
        let mut d = vec![1u8; 16];
        d.push(3);
        d.extend_from_slice(label.as_bytes());
        d.push(0);
        let mut body = boxed(b"jumd", &d);
        body.extend_from_slice(&boxed(b"json", data));
        boxed(b"jumb", &body)
    }

    fn payload_of(bytes: &[u8], label: &str) -> Option<Vec<u8>> {
        let s = parse(&mut Cursor::new(bytes)).unwrap();
        let found = s
            .entities_labelled(SegmentKind::Jumbf, label)
            .next()
            .map(|e| s.read_entity(&mut Cursor::new(bytes), e).unwrap());
        found
    }

    #[test]
    fn an_update_keeps_the_manifest_as_original_and_adds_an_update_box() {
        let (m1, m2) = (jman("m1", b"1"), jman("m2", b"2"));
        let first = jstore(std::slice::from_ref(&m1));
        let (f, e0) = media_file(&[c2pa_box(MANIFEST, &first)], &[], None);
        let out = rewrite(
            &f,
            &Updates::new()
                .set_jumbf(jstore(&[m1, m2.clone()]))
                .as_update(),
        )
        .unwrap();

        // Everything is as it was, but for the purpose and the box at the end.
        let mut expect = f.clone();
        let at = expect.windows(9).position(|w| w == b"manifest\0").unwrap();
        expect[at..at + 8].copy_from_slice(b"original");
        expect.extend_from_slice(&c2pa_box(UPDATE, &jstore(&[m2])));
        assert_eq!(out, expect);

        assert_eq!(payload_of(&out, ORIGINAL).unwrap(), first);
        assert!(payload_of(&out, MANIFEST).is_none());
        assert_eq!(stco_entry(&out), e0);
    }

    #[test]
    fn later_updates_replace_the_update_store() {
        let (m1, m2, m3) = (jman("m1", b"1"), jman("m2", b"2"), jman("m3", b"3"));
        for before_moov in [false, true] {
            let update = c2pa_box(UPDATE, &jstore(std::slice::from_ref(&m2)));
            let original = c2pa_box(ORIGINAL, &jstore(std::slice::from_ref(&m1)));
            let (f, _) = if before_moov {
                media_file(&[original, update], &[], None)
            } else {
                media_file(&[original], &[update], None)
            };
            let full = jstore(&[m1.clone(), m2.clone(), m3.clone()]);
            let out = rewrite(&f, &Updates::new().set_jumbf(full).as_update()).unwrap();
            assert_eq!(
                payload_of(&out, UPDATE).unwrap(),
                jstore(&[m2.clone(), m3.clone()])
            );
            assert_eq!(
                payload_of(&out, ORIGINAL).unwrap(),
                jstore(std::slice::from_ref(&m1))
            );
            // An update box ahead of the media moves it; the offsets follow.
            assert!(
                media_at(&out, stco_entry(&out)),
                "before_moov={before_moov}"
            );

            // A manifest being signed replaces its placeholder in the update store.
            let m3b = jman("m3", b"three, signed");
            let full = jstore(&[m1.clone(), m2.clone(), m3b.clone()]);
            let again = rewrite(&out, &Updates::new().set_jumbf(full).as_update()).unwrap();
            assert_eq!(
                payload_of(&again, UPDATE).unwrap(),
                jstore(&[m2.clone(), m3b])
            );
            assert!(media_at(&again, stco_entry(&again)));
        }
    }

    #[test]
    fn an_update_that_is_not_an_extension_is_written_as_a_whole() {
        let (m1, m2, m3) = (jman("m1", b"1"), jman("m2", b"2"), jman("m3", b"3"));

        // Nothing to update.
        let (clean, _) = media_file(&[], &[], None);
        let full = jstore(std::slice::from_ref(&m1));
        let out = rewrite(&clean, &Updates::new().set_jumbf(full.clone()).as_update()).unwrap();
        assert_eq!(payload_of(&out, MANIFEST).unwrap(), full);

        // The existing manifests changed (redaction): the whole store is written,
        // and the old update store is gone.
        let (f, _) = media_file(
            &[
                c2pa_box(ORIGINAL, &jstore(std::slice::from_ref(&m1))),
                c2pa_box(UPDATE, &jstore(std::slice::from_ref(&m2))),
            ],
            &[],
            None,
        );
        let edited = jstore(&[jman("m1", b"redacted"), m2, m3]);
        let out = rewrite(&f, &Updates::new().set_jumbf(edited.clone()).as_update()).unwrap();
        assert_eq!(payload_of(&out, MANIFEST).unwrap(), edited);
        assert!(payload_of(&out, ORIGINAL).is_none());
        assert!(payload_of(&out, UPDATE).is_none());
        assert!(media_at(&out, stco_entry(&out)));

        // Set after update clears the hint.
        let (f, _) = media_file(
            &[c2pa_box(MANIFEST, &jstore(std::slice::from_ref(&m1)))],
            &[],
            None,
        );
        let extended = jstore(&[m1.clone(), jman("m9", b"9")]);
        let plain = Updates::new().set_jumbf(extended.clone());
        let out = rewrite(&f, &plain).unwrap();
        assert!(payload_of(&out, ORIGINAL).is_none());
        assert_eq!(payload_of(&out, MANIFEST).unwrap(), extended);
        // Without the hint a pure extension is a standard manifest, not an update;
        // a later set_jumbf also clears it.
        let cleared = Updates::new()
            .set_jumbf(extended.clone())
            .as_update()
            .set_jumbf(extended.clone());
        let out = rewrite(&f, &cleared).unwrap();
        assert!(payload_of(&out, ORIGINAL).is_none());
    }

    #[test]
    fn a_placeholder_is_filled_in_place_at_exactly_its_size() {
        let placeholder = vec![0u8; 40];
        let (f, e0) = media_file(&[c2pa_box(MANIFEST, &placeholder)], &[], None);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let e = s
            .entities_labelled(SegmentKind::Jumbf, MANIFEST)
            .next()
            .unwrap();
        let range = e.parts().next().unwrap().range();

        let signed: Vec<u8> = (1..=40u8).collect();
        let mut cur = Cursor::new(f.clone());
        s.patch_exact(&mut cur, e, &signed).unwrap();
        let out = cur.into_inner();

        // Only the payload changed: same length, same offsets, same everything else.
        assert_eq!(out.len(), f.len());
        let (a, b) = (range.offset() as usize, range.end() as usize);
        assert_eq!(&out[a..b], &signed[..]);
        assert_eq!(&out[..a], &f[..a]);
        assert_eq!(&out[b..], &f[b..]);
        assert_eq!(stco_entry(&out), e0);

        // A different size is an error and leaves the file alone.
        for bad in [&signed[..39], &[signed.clone(), vec![0]].concat()[..]] {
            let mut cur = Cursor::new(f.clone());
            assert!(s.patch_exact(&mut cur, e, bad).is_err());
            assert_eq!(cur.into_inner(), f);
        }
    }

    const EXIF_ITEM: &[u8] = b"\0\0\0\x06Exif\0\0II*\0\x08\0\0\0";
    const TIFF: &[u8] = b"II*\0\x08\0\0\0";
    const THUMB: &[u8] = b"THUMBNAIL-BYTES";

    /// `ftyp`, a `meta` listing a HEVC item, an EXIF item and a JPEG thumbnail of
    /// the first, then an `mdat` holding the data of the last two.
    fn heif(exif: &[u8], thumb: &[u8]) -> Vec<u8> {
        heif_with(exif, thumb, heif_items::build::infe(3, b"jpeg"))
    }

    /// [`heif`] with a chosen `infe` (item 3) for the thumbnail.
    fn heif_with(exif: &[u8], thumb: &[u8], thumb_infe: Vec<u8>) -> Vec<u8> {
        use heif_items::build::{iinf, iloc0, infe, meta, thmb};
        let build = |e_at: u32, t_at: u32| {
            let body = meta(&[
                iinf(&[infe(1, b"hvc1"), infe(2, b"Exif"), thumb_infe.clone()]),
                thmb(3, 1),
                iloc0(&[
                    (2, &[(e_at, exif.len() as u32)]),
                    (3, &[(t_at, thumb.len() as u32)]),
                ]),
            ]);
            let mut p = vec![0u8; 4]; // version and flags
            p.extend_from_slice(&body);
            file(&[
                ftyp(),
                boxed(b"meta", &p),
                boxed(b"mdat", &[exif, thumb].concat()),
            ])
        };
        let data_at = (build(0, 0).len() - exif.len() - thumb.len()) as u32;
        build(data_at, data_at + exif.len() as u32)
    }

    fn item(s: &Structure, bytes: &[u8], kind: SegmentKind) -> Option<(Option<String>, Vec<u8>)> {
        let e = s.entities(kind).next()?;
        Some((
            e.label().map(str::to_owned),
            s.read_entity(&mut Cursor::new(bytes), e).unwrap(),
        ))
    }

    #[test]
    fn heif_exif_and_thumbnail_items_are_entities() {
        let f = heif(EXIF_ITEM, THUMB);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        // The EXIF entity starts at the TIFF header, after the offset field and
        // the bytes it skips.
        assert_eq!(item(&s, &f, SegmentKind::Exif), Some((None, TIFF.to_vec())));
        assert_eq!(
            item(&s, &f, SegmentKind::Thumbnail),
            Some((Some("jpeg".into()), THUMB.to_vec()))
        );
        let t = s.embedded_thumbnail(&mut Cursor::new(&f)).unwrap().unwrap();
        assert_eq!(
            (t.data.as_slice(), t.media_type()),
            (THUMB, Some("image/jpeg"))
        );
        // Still a tiling, and the C2PA rules are not disturbed.
        let covered: u64 = s.regions().map(|r| r.range.len()).sum();
        assert_eq!(covered, f.len() as u64);
        assert_eq!(rewrite(&f, &Updates::new()).unwrap(), f);
    }

    #[test]
    fn thumbnail_media_types_follow_the_item() {
        use heif_items::build::{infe, infe_mime};
        let media = |thumb_infe: Vec<u8>, write: bool| {
            let f = heif_with(TIFF, THUMB, thumb_infe);
            let s = parse(&mut Cursor::new(&f)).unwrap();
            let (s, f) = if write {
                // The media type is carried through a write that moves the item.
                let out = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
                (parse(&mut Cursor::new(&out)).unwrap(), out)
            } else {
                (s, f)
            };
            let t = s.embedded_thumbnail(&mut Cursor::new(&f)).unwrap().unwrap();
            assert_eq!(t.data, THUMB);
            let e = s.entities(SegmentKind::Thumbnail).next().unwrap();
            assert_eq!(e.media_type(), t.media_type());
            t.media_type().map(str::to_owned)
        };
        for write in [false, true] {
            assert_eq!(
                media(infe(3, b"jpeg"), write).as_deref(),
                Some("image/jpeg")
            );
            assert_eq!(
                media(infe_mime(3, b"image/png"), write).as_deref(),
                Some("image/png")
            );
            // A coded item is a bitstream, not a file, so it has no media type.
            assert_eq!(media(infe(3, b"av01"), write), None);
            assert_eq!(media(infe_mime(3, b"nonsense"), write), None);
        }
    }

    #[test]
    fn items_follow_their_boxes_through_a_write() {
        let f = heif(EXIF_ITEM, THUMB);
        let s = parse(&mut Cursor::new(&f)).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = s
            .handler()
            .write(
                &s,
                &mut Cursor::new(&f),
                &mut out,
                &Updates::new().set_jumbf(b"STORE".to_vec()),
                &mut NullSink,
            )
            .unwrap();
        let out = out.into_inner();
        assert!(out.len() > f.len());

        // The structure returned describes the items where they now are ...
        for (kind, want) in [(SegmentKind::Exif, TIFF), (SegmentKind::Thumbnail, THUMB)] {
            assert_eq!(item(&dest, &out, kind).unwrap().1, want);
        }
        // ... as does parsing the output: the offsets in iloc moved with the data.
        let again = parse(&mut Cursor::new(&out)).unwrap();
        assert_eq!(
            item(&again, &out, SegmentKind::Exif),
            item(&dest, &out, SegmentKind::Exif)
        );
        assert_eq!(
            item(&again, &out, SegmentKind::Thumbnail),
            item(&dest, &out, SegmentKind::Thumbnail)
        );
    }

    #[test]
    fn bad_items_and_bad_meta_do_not_fail_the_file() {
        // A TIFF offset that runs past the item.
        let bad_offset = heif(b"\xFF\xFF\xFF\xFFExif\0\0II*\0", THUMB);
        let s = parse(&mut Cursor::new(&bad_offset)).unwrap();
        assert!(s.entities(SegmentKind::Exif).next().is_none());
        assert!(s.entities(SegmentKind::Thumbnail).next().is_some());

        // A meta box that is not HEIF items at all.
        let junk = file(&[
            ftyp(),
            boxed(b"meta", &[0, 0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3]),
            boxed(b"mdat", b"x"),
        ]);
        let s = parse(&mut Cursor::new(&junk)).unwrap();
        assert!(s.entities(SegmentKind::Exif).next().is_none());
        assert!(s
            .embedded_thumbnail(&mut Cursor::new(&junk))
            .unwrap()
            .is_none());
    }

    #[test]
    fn item_data_is_split_where_it_crosses_boxes() {
        let a = SegmentId::from_index(0).unwrap();
        let b = SegmentId::from_index(1).unwrap();
        let placed = [(a, 0u64, 10u64), (b, 10, 10)];
        let parts = split_over_segments(&placed, 6, 8).unwrap().unwrap();
        assert_eq!(
            parts,
            vec![
                (a, ByteRange::new(6, 4).unwrap()),
                (b, ByteRange::new(10, 4).unwrap())
            ]
        );
        // Past the end of the file: not placed.
        assert!(split_over_segments(&placed, 18, 5).unwrap().is_none());
        assert!(split_over_segments(&placed, 25, 1).unwrap().is_none());
    }

    #[test]
    fn iloc_offsets_follow() {
        let build = |v: u32| {
            let mut iloc = vec![0x44, 0x00];
            iloc.extend_from_slice(&1u16.to_be_bytes()); // items
            iloc.extend_from_slice(&1u16.to_be_bytes()); // id
            iloc.extend_from_slice(&0u16.to_be_bytes()); // data reference
            iloc.extend_from_slice(&1u16.to_be_bytes()); // extents
            iloc.extend_from_slice(&v.to_be_bytes());
            iloc.extend_from_slice(&5u32.to_be_bytes());
            let mut meta = vec![0u8; 4];
            meta.extend_from_slice(&full(b"iloc", 0, 0, &iloc));
            file(&[ftyp(), boxed(b"meta", &meta), boxed(b"mdat", b"MEDIA")])
        };
        let media = (build(0).len() - 5) as u32;
        let f = build(media);
        let out = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
        let (off, _, hl) = find(&out, "/meta/iloc");
        let v = be32(&out, off + hl as u64 + 4 + 10);
        assert_eq!(v, media + 50);
        assert!(media_at(&out, v));
    }

    /// A movie fragment and a fragment-random-access box that both record where
    /// the `moof` is, which is `FRAGMENT_BASE`.
    const FRAGMENT_BASE: u64 = 20;

    fn fragmented() -> Vec<u8> {
        let base = FRAGMENT_BASE; // the moof starts right after ftyp
        let mut tfhd = 1u32.to_be_bytes().to_vec();
        tfhd.extend_from_slice(&base.to_be_bytes());
        let moof = boxed(b"moof", &boxed(b"traf", &full(b"tfhd", 0, 1, &tfhd)));
        let mut tfra = 1u32.to_be_bytes().to_vec(); // track
        tfra.extend_from_slice(&0u32.to_be_bytes()); // entry field sizes
        tfra.extend_from_slice(&1u32.to_be_bytes()); // entries
        tfra.extend_from_slice(&0u32.to_be_bytes()); // time
        tfra.extend_from_slice(&(base as u32).to_be_bytes()); // moof offset
        tfra.extend_from_slice(&[1, 1, 1]);
        let mfra = boxed(b"mfra", &full(b"tfra", 0, 0, &tfra));
        file(&[ftyp(), moof, boxed(b"mdat", b"MEDIA"), mfra])
    }

    #[test]
    fn fragment_offsets_follow() {
        let base = FRAGMENT_BASE;
        let f = fragmented();

        let out = rewrite(&f, &Updates::new().set_jumbf(b"STORE".to_vec())).unwrap();
        let moof_at = find(&out, "/moof").0;
        assert_eq!(moof_at, base + 50);
        let (off, _, hl) = find(&out, "/moof/traf/tfhd");
        let b = u64::from_be_bytes(
            out[(off + hl as u64 + 4 + 4) as usize..][..8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(b, moof_at);
        let (off, _, hl) = find(&out, "/mfra/tfra");
        assert_eq!(be32(&out, off + hl as u64 + 4 + 12 + 4) as u64, moof_at);
    }
}
