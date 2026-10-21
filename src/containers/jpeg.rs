//! JPEG on the layout model.
//!
//! Every marker segment is one [`Segment`]. For a segment with a length field,
//! the header is `FF xx len(2)` plus any signature or JPEG XT prefix, and the
//! payload is the rest. A scan is one `ImageData` segment: header `FF DA len
//! scan-header`, payload the entropy-coded data up to the next marker that is not
//! a restart marker. Progressive files therefore have one such segment per scan,
//! with their DHT/DRI markers as ordinary segments between them. A scan that
//! contains restart markers is flagged as subdividing; see
//! [`Structure::elements`](crate::Structure::elements) and [`subdivide`].
//!
//! JUMBF is carried in JPEG XT APP11 segments. A manifest larger than one
//! segment spans several; they form a single [`Entity`](crate::Entity) whose
//! parts are the per-segment payloads, so the repeated XT headers are never part
//! of the data and are always visible as headers.
//!
//! # Multi-Picture Format
//!
//! When another image (`FF D8 FF`) follows an `EOI` directly, the file holds
//! several images, as in a Multi-Picture Format (MPF) file such as Ultra HDR. Each
//! image's markers are listed in order (`SOI`, ..., `EOI`, `SOI`, ..., `EOI`), and
//! [`Structure::subassets`] reports one part per image, plus one for any data after
//! the last image. Only the first image's XMP, EXIF and JUMBF are interpreted as the
//! asset's; later images are left as plain segments. New metadata is inserted into
//! the first image. Bytes that start like an image (`FF D8 FF`) but do not parse as
//! one are trailing data, as they are after a single image.
//!
//! The MPF APP2 segment records each image's size and offset. When an edit changes
//! the size of the first image or moves the block, the recorded values are shifted
//! to match, so the output stays a valid MPF file. The edit is refused rather than
//! writing a stale or ambiguous index when the index is not one that can be edited
//! safely: it must be well formed (one `MPEntry` array, outside the header and the
//! IFD, agreeing with the stated image count), appear once, and record the offset
//! of every image and the size of every image but the first exactly. The first
//! image's size may be recorded short of the image, as in Pixel Ultra HDR files, and
//! is rewritten to the true size; one that claims more than the image is refused.
//! Parsing and copying unchanged never consult it.
//!
//! Not yet ported: EXIF thumbnail location.

use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, FormatSpec, ReadSeek, ReadWriteSeek};
use crate::segment::SegmentKind;
use crate::segment::{Name, Role, Segment};
use crate::sink::{flush_run, write_buffer_len, Emitter, Piece, Sink, RUN_WINDOWS};
use crate::structure::{Part, SegmentId, Structure, StructureBuilder};
use crate::ByteRange;
use crate::{MetadataUpdate, Updates};
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, OnceLock};

const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APP11: u8 = 0xEB;

/// Start of the Multi-Picture Format APP2 payload.
const MPF_SIGNATURE: &[u8] = b"MPF\0";

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXT_SIGNATURE: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const EXIF_SIGNATURE: &[u8] = b"Exif\0\0";
/// Common identifier of a JUMBF APP11 packet, and the box type of a JUMBF superbox.
const JP: [u8; 2] = *b"JP";
/// Read size when scanning entropy-coded data for markers.
const SCAN_BUF: usize = 32 * 1024;
const JUMB: [u8; 4] = *b"jumb";

const XMP_SIG_LEN: usize = XMP_SIGNATURE.len();
const EXT_SIG_LEN: usize = XMP_EXT_SIGNATURE.len();
/// Extended XMP prefix after the marker/length: signature, GUID(32), total(4), offset(4).
const EXT_PREFIX_LEN: usize = EXT_SIG_LEN + 32 + 4 + 4;
/// Largest header we ever build: marker/length plus the extended XMP prefix.
const MAX_HEADER: usize = 4 + EXT_PREFIX_LEN;

/// Largest value of a marker's length field.
const MAX_LEN_FIELD: u64 = 0xFFFF;
/// Largest standard XMP packet one APP1 can carry.
const MAX_STD_XMP: u64 = MAX_LEN_FIELD - 2 - XMP_SIG_LEN as u64;
/// Extended XMP payload per segment.
const EXT_CHUNK: usize = 65533 - EXT_PREFIX_LEN - 2;
/// JUMBF payload per APP11 segment (room for the 16-byte continuation prefix).
const JUMBF_CHUNK: usize = 65533 - 16;
/// JPEG XT box instance number written for new JUMBF.
const XT_EN: u16 = 0x0211;
/// Largest extended XMP we will reassemble.
const MAX_EXT_XMP: u32 = 100 * 1024 * 1024;

/// How much of a segment to look at when classifying it.
const PEEK: usize = 80;

/// A JUMBF box being assembled from APP11 packets: its instance number, the
/// segments so far, and its type once the first packet has been seen.
struct Open {
    en: u16,
    ids: Vec<SegmentId>,
    ty: Option<[u8; 16]>,
}

enum Class {
    Plain,
    XmpMain,
    XmpExt,
    Exif,
    Jumbf { en: u16, z: u32 },
    JumbfRaw,
}

/// The JPEG container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct JpegHandler;

/// The shared JPEG handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    static H: OnceLock<Arc<JpegHandler>> = OnceLock::new();
    H.get_or_init(|| Arc::new(JpegHandler)).clone()
}

/// The JPEG container's [`ContainerHandler::id`].
pub const ID: &str = "jpeg";

/// ISO/IEC 10918-1 Table B.1 symbol for a marker code.
fn marker_symbol(m: u8) -> Cow<'static, str> {
    match m {
        0x01 => "TEM".into(),
        0xC4 => "DHT".into(),
        0xC8 => "JPG".into(),
        0xCC => "DAC".into(),
        0xC0..=0xCF => Cow::Owned(format!("SOF{}", m - 0xC0)),
        0xD0..=0xD7 => Cow::Owned(format!("RST{}", m - 0xD0)),
        0xD8 => "SOI".into(),
        0xD9 => "EOI".into(),
        0xDA => "SOS".into(),
        0xDB => "DQT".into(),
        0xDC => "DNL".into(),
        0xDD => "DRI".into(),
        0xDE => "DHP".into(),
        0xDF => "EXP".into(),
        0xE0..=0xEF => Cow::Owned(format!("APP{}", m - 0xE0)),
        0xF0..=0xFD => Cow::Owned(format!("JPG{}", m - 0xF0)),
        0xFE => "COM".into(),
        // 0x02..=0xBF are reserved (RES); 0x00 and 0xFF are never markers.
        _ => "RES".into(),
    }
}

impl ContainerHandler for JpegHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn box_name(&self, name: Name) -> Option<Cow<'static, str>> {
        match name {
            Name::Marker(m) => Some(marker_symbol(m)),
            Name::None => None,
            other => Some(Cow::Owned(other.to_string())),
        }
    }

    fn formats(&self) -> &[FormatSpec] {
        // A Multi-Picture Format file is a JPEG and has no media type of its own.
        const F: &[FormatSpec] =
            &[FormatSpec::new("image/jpeg", "jpg").extension_aliases(&["jpeg", "mpo"])];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.starts_with(&[0xFF, SOI, 0xFF])
    }

    fn parse(self: Arc<Self>, mut src: &mut dyn ReadSeek) -> Result<Structure> {
        parse_with(self, &mut src)
    }

    fn write(
        &self,
        src_structure: &Structure,
        mut src: &mut dyn ReadSeek,
        mut out: &mut dyn ReadWriteSeek,
        updates: &Updates,
        sink: &mut dyn Sink,
    ) -> Result<Structure> {
        write(src_structure, &mut src, &mut out, updates, sink)
    }

    fn plan(
        &self,
        src_structure: &Structure,
        mut src: &mut dyn ReadSeek,
        updates: &Updates,
    ) -> Result<Structure> {
        plan(src_structure, &mut src, updates).map(|(_, dest)| dest)
    }

    fn compose(&self, kind: SegmentKind, data: &[u8]) -> Result<Vec<u8>> {
        compose(kind, data)
    }

    fn read_xmp(
        &self,
        structure: &Structure,
        mut src: &mut dyn ReadSeek,
    ) -> Result<Option<Vec<u8>>> {
        read_xmp(structure, &mut src)
    }

    fn subdivide(
        &self,
        mut src: &mut dyn ReadSeek,
        segment: &Segment,
        emit: &mut dyn FnMut(&Segment) -> Result<()>,
    ) -> Result<()> {
        subdivide(&mut src, segment, emit)
    }
}

/// Parse a JPEG into a validated layout.
pub fn parse<R: Read + Seek>(src: &mut R) -> Result<Structure> {
    parse_with(handler(), src)
}

/// Size of the read-ahead window used while parsing. The markers of a typical JPEG
/// fit in a few KiB. Measured on 24 fixtures (Apple M2): 4 KiB was as fast as or
/// faster than 8, 16 and 32 KiB, from memory and from a file.
const PARSE_WINDOW: usize = 4 * 1024;

/// A read-ahead window over a seekable stream, for a parser that makes many small
/// reads and seeks. A seek only records the position; the stream is touched when a
/// read misses the window. A read at least as large as the window goes straight to
/// the stream. Positions are the stream's own, and the stream's position after use
/// is unspecified. The first seek must be absolute (`Start` or `End`).
struct ReadAhead<'a, R: Read + Seek> {
    inner: &'a mut R,
    /// The window's contents; empty when there is none. Its capacity is never used
    /// uninitialised: `read_to_end` fills spare capacity without zeroing it first.
    buf: Vec<u8>,
    window: usize,
    /// Stream offset of `buf[0]`.
    start: u64,
    /// Logical position.
    pos: u64,
}

impl<'a, R: Read + Seek> ReadAhead<'a, R> {
    fn new(inner: &'a mut R, window: usize) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(window),
            window,
            start: 0,
            pos: 0,
        }
    }
}

impl<R: Read + Seek> Read for ReadAhead<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let in_window = self.pos >= self.start && self.pos < self.start + self.buf.len() as u64;
        if !in_window {
            self.inner.seek(SeekFrom::Start(self.pos))?;
            if out.len() >= self.window {
                let n = self.inner.read(out)?;
                self.pos += n as u64;
                return Ok(n);
            }
            self.buf.clear();
            let filled =
                Read::take(&mut *self.inner, self.window as u64).read_to_end(&mut self.buf);
            if let Err(e) = filled {
                self.buf.clear();
                return Err(e);
            }
            self.start = self.pos;
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let off = (self.pos - self.start) as usize;
        let n = out.len().min(self.buf.len() - off);
        out[..n].copy_from_slice(&self.buf[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for ReadAhead<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start")
            })?,
            SeekFrom::End(d) => self.inner.seek(SeekFrom::End(d))?,
        };
        Ok(self.pos)
    }
}

fn parse_with<R: Read + Seek>(
    handler: Arc<dyn ContainerHandler>,
    src: &mut R,
) -> Result<Structure> {
    // A parse is a long run of reads of one to a few bytes, each preceded by a seek.
    // From a `File` that is a syscall apiece; the window makes it a copy.
    let mut ahead = ReadAhead::new(src, PARSE_WINDOW);
    let src = &mut ahead;
    let total = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    if total < 4 {
        return Err(Error::InvalidFormat("JPEG too small".into()));
    }
    let mut soi = [0u8; 2];
    src.read_exact(&mut soi)?;
    if soi != [0xFF, SOI] {
        return Err(Error::InvalidFormat("Not a JPEG file".into()));
    }

    let mut b = StructureBuilder::new(handler, "image/jpeg", total);
    b.push_span(SegmentKind::Header, Name::Marker(SOI), 2, 0, 0)?;

    // Only the first image carries the asset's metadata.
    let mut meta = Meta::default();
    let mut offset = scan_image(src, &mut b, total, 2, true, &mut meta)?;
    // The first image's markers are the structure. Everything after its EOI, later
    // images and trailing data alike, is data beyond the end of it.
    b.end_boxes()?;
    if let Some(ids) = meta.xmp_open.take() {
        b.entity_of_segments(SegmentKind::Xmp, &ids)?;
    }
    if let Some(o) = meta.jumbf_open.take() {
        b.entity_of_segments_typed(SegmentKind::Jumbf, o.ty, &o.ids)?;
    }

    // Another image directly after this one makes a multi-picture file. Later
    // images are listed as segments and left uninterpreted. Bytes that start like an
    // image but are not one are trailing data, as they are in a single-image file:
    // the layout is rolled back to the end of the last good image.
    let mut multi = false;
    while total - offset >= 4 && next_is_image(src, offset)? {
        b.begin_subasset(Some("image/jpeg".into()))?;
        match scan_image(src, &mut b, total, offset, false, &mut Meta::default()) {
            Ok(end) => {
                offset = end;
                multi = true;
            }
            Err(e) if is_malformed(&e) => {
                offset = b.rollback_subasset()?;
                break;
            }
            Err(e) => return Err(e),
        }
    }

    if offset < total {
        if multi {
            b.begin_subasset(None)?;
        }
        b.push_span(SegmentKind::Other, Name::None, 0, total - offset, 0)?;
    }
    b.finish()
}

/// Whether `e` says the bytes are not a well-formed image, as opposed to a failure
/// to read them, which must not be mistaken for trailing data.
fn is_malformed(e: &Error) -> bool {
    match e {
        Error::InvalidFormat(_) | Error::InvalidSegment { .. } | Error::LimitExceeded { .. } => {
            true
        }
        Error::Io(io) => io.kind() == std::io::ErrorKind::UnexpectedEof,
        _ => false,
    }
}

/// Metadata being assembled while the primary image is scanned.
#[derive(Default)]
struct Meta {
    xmp_open: Option<Vec<SegmentId>>,
    jumbf_open: Option<Open>,
    /// End offset of the last segment added to `jumbf_open`. C2PA A.3.1 requires the
    /// APP11 segments of one manifest to be sequential and contiguous.
    jumbf_end: u64,
}

/// List the markers of one image, from `offset` through its `EOI`, and return the
/// offset after it. Only the primary image's metadata is interpreted.
fn scan_image<R: Read + Seek>(
    src: &mut R,
    b: &mut StructureBuilder,
    total: u64,
    mut offset: u64,
    primary: bool,
    meta: &mut Meta,
) -> Result<u64> {
    let Meta {
        xmp_open,
        jumbf_open,
        jumbf_end,
    } = meta;
    loop {
        if total - offset < 2 {
            return Err(Error::InvalidFormat("JPEG truncated: missing EOI".into()));
        }
        src.seek(SeekFrom::Start(offset))?;

        // A marker is FF xx, optionally preceded by FF fill bytes.
        let mut first = [0u8; 1];
        src.read_exact(&mut first)?;
        if first[0] != 0xFF {
            return Err(Error::InvalidSegment {
                offset,
                reason: format!("expected marker prefix 0xFF, found 0x{:02X}", first[0]),
            });
        }
        let mut fill = 0u64;
        let marker = loop {
            let mut m = [0u8; 1];
            src.read_exact(&mut m)?;
            if m[0] == 0xFF {
                fill += 1;
            } else {
                break m[0];
            }
        };
        if marker == 0x00 {
            return Err(Error::InvalidSegment {
                offset,
                reason: "stuffed byte outside entropy-coded data".into(),
            });
        }
        if fill > 0 {
            b.push_span(SegmentKind::Other, Name::None, 0, fill, 0)?;
        }
        let mstart = offset + fill;

        // Markers without a length field.
        match marker {
            EOI => {
                b.push_span(SegmentKind::Other, Name::Marker(EOI), 2, 0, 0)?;
                return Ok(mstart + 2);
            }
            0x01 | 0xD0..=0xD7 | SOI => {
                b.push_span(SegmentKind::Other, Name::Marker(marker), 2, 0, 0)?;
                offset = mstart + 2;
                continue;
            }
            _ => {}
        }

        let mut lb = [0u8; 2];
        src.read_exact(&mut lb)?;
        let len = u16::from_be_bytes(lb) as u64;
        if len < 2 {
            return Err(Error::InvalidSegment {
                offset: mstart,
                reason: "marker length < 2".into(),
            });
        }
        let seg_total = 2 + len;
        if mstart + seg_total > total {
            return Err(Error::InvalidSegment {
                offset: mstart,
                reason: "segment extends past end of file".into(),
            });
        }
        let data_len = len - 2;

        if marker == SOS {
            let data_start = mstart + seg_total;
            let mut scan = EntropyScan::new(data_start, total);
            let mut has_restarts = false;
            let stop = loop {
                match scan.next(src)? {
                    Event::Rst(..) => has_restarts = true,
                    Event::Marker(at) => break at,
                    Event::Eof => {
                        return Err(Error::InvalidFormat("EOI marker not found".into()));
                    }
                }
            };
            let id = b.push_span(
                SegmentKind::ImageData,
                Name::Marker(SOS),
                seg_total as u32,
                stop - data_start,
                0,
            )?;
            if has_restarts {
                b.mark_subdivides(id)?;
            }
            // The marker that ended the scan (DHT, SOS, EOI, ...) is handled by the
            // main loop, so later scans and metadata after the first scan are seen.
            offset = stop;
            continue;
        }

        let mut kind = SegmentKind::Other;
        let mut hdr_extra = 0u64;
        let mut class = Class::Plain;
        let mut jtype: Option<[u8; 16]> = None;

        if primary && matches!(marker, APP1 | APP2 | APP11) {
            let n = data_len.min(PEEK as u64) as usize;
            let mut buf = [0u8; PEEK];
            src.read_exact(&mut buf[..n])?;
            let peek = &buf[..n];

            if marker == APP2 {
                if peek.starts_with(MPF_SIGNATURE) {
                    hdr_extra = MPF_SIGNATURE.len() as u64;
                }
            } else if marker == APP1 {
                if peek.starts_with(XMP_SIGNATURE) {
                    kind = SegmentKind::Xmp;
                    hdr_extra = XMP_SIG_LEN as u64;
                    class = Class::XmpMain;
                } else if peek.starts_with(XMP_EXT_SIGNATURE)
                    && data_len >= EXT_PREFIX_LEN as u64
                    && xmp_open.is_some()
                {
                    kind = SegmentKind::Xmp;
                    hdr_extra = EXT_PREFIX_LEN as u64;
                    class = Class::XmpExt;
                } else if peek.starts_with(EXIF_SIGNATURE) {
                    kind = SegmentKind::Exif;
                    hdr_extra = EXIF_SIGNATURE.len() as u64;
                    class = Class::Exif;
                }
            } else if n >= 16 && peek[0..2] == JP[..] && peek[12..16] == JUMB[..] {
                let en = u16::from_be_bytes([peek[2], peek[3]]);
                let z = u32::from_be_bytes([peek[4], peek[5], peek[6], peek[7]]);
                // The spec numbers packets from 1, but real files (e.g. Designer.jpeg)
                // start at 0; treat 0 and 1 as the first packet.
                let first = z <= 1;
                let overhead = if first { 8 } else { 16 };
                let continues = !first && jumbf_open.as_ref().is_some_and(|o| o.en == en);
                if continues && mstart != *jumbf_end {
                    return Err(Error::InvalidFormat(
                        "JUMBF APP11 segments are not contiguous".into(),
                    ));
                }
                if data_len >= overhead && (first || continues) {
                    kind = SegmentKind::Jumbf;
                    hdr_extra = overhead;
                    class = Class::Jumbf { en, z };
                    // The type is in the first packet: after the 8 bytes of the XT
                    // header (CI, En, Z) come the superbox and its description box.
                    if first {
                        jtype = crate::containers::jumbf::superbox_type(&peek[8..]);
                    }
                }
            } else if n >= 8 && peek[4..8] == JUMB[..] {
                kind = SegmentKind::Jumbf;
                class = Class::JumbfRaw;
                jtype = crate::containers::jumbf::superbox_type(peek);
            }
        }

        let id = b.push_span(
            kind,
            Name::Marker(marker),
            (4 + hdr_extra) as u32,
            data_len - hdr_extra,
            0,
        )?;

        match class {
            Class::Plain => {}
            Class::Exif => b.entity_of_segments(SegmentKind::Exif, &[id])?,
            Class::XmpMain => {
                if let Some(ids) = xmp_open.take() {
                    b.entity_of_segments(SegmentKind::Xmp, &ids)?;
                }
                *xmp_open = Some(vec![id]);
            }
            Class::XmpExt => {
                if let Some(ids) = xmp_open.as_mut() {
                    ids.push(id);
                }
            }
            Class::Jumbf { en, z } => {
                if z <= 1 {
                    if let Some(o) = jumbf_open.take() {
                        b.entity_of_segments_typed(SegmentKind::Jumbf, o.ty, &o.ids)?;
                    }
                    *jumbf_open = Some(Open {
                        en,
                        ids: vec![id],
                        ty: jtype,
                    });
                } else if let Some(o) = jumbf_open.as_mut() {
                    o.ids.push(id);
                }
                *jumbf_end = mstart + seg_total;
            }
            Class::JumbfRaw => {
                if let Some(o) = jumbf_open.take() {
                    b.entity_of_segments_typed(SegmentKind::Jumbf, o.ty, &o.ids)?;
                }
                *jumbf_open = Some(Open {
                    en: 0,
                    ids: vec![id],
                    ty: jtype,
                });
                *jumbf_end = mstart + seg_total;
            }
        }

        offset = mstart + seg_total;
    }
}

/// Whether another JPEG image starts at `offset` (`FF D8 FF`).
fn next_is_image<R: Read + Seek>(src: &mut R, offset: u64) -> Result<bool> {
    let mut m = [0u8; 3];
    src.seek(SeekFrom::Start(offset))?;
    src.read_exact(&mut m)?;
    Ok(m == [0xFF, SOI, 0xFF])
}

/// The Multi-Picture Format index: where each entry's size and offset fields are
/// in an MPF block (the bytes after `MPF\0`), and the byte order of the block.
/// Offsets in the entries are relative to the start of the block.
struct MpIndex {
    little: bool,
    /// Position of each entry's size field; its offset field follows.
    entries: Vec<usize>,
}

impl MpIndex {
    fn parse(p: &[u8]) -> Option<Self> {
        let little = match p.get(..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let me = Self {
            little,
            entries: Vec::new(),
        };
        if me.u16(p, 2)? != 0x2A {
            return None;
        }
        let ifd = me.u32(p, 4)? as usize;
        if ifd < 8 {
            return None;
        }
        let count = me.u16(p, ifd)? as usize;
        // The IFD: a count, the entries, and the offset of the next IFD.
        let ifd_end = ifd.checked_add(2 + count * 12 + 4)?;
        if ifd_end > p.len() {
            return None;
        }
        let mut array = None;
        let mut images = None;
        for i in 0..count {
            // Entry fields: tag, type, count, value (inline, or the offset of data).
            let e = ifd + 2 + i * 12;
            let (tag, ty, n, val) = (
                me.u16(p, e)?,
                me.u16(p, e + 2)?,
                me.u32(p, e + 4)?,
                me.u32(p, e + 8)?,
            );
            match tag {
                // NumberOfImages: one LONG.
                0xB001 => {
                    if images.is_some() || ty != 4 || n != 1 {
                        return None;
                    }
                    images = Some(val as usize);
                }
                // MPEntry: UNDEFINED, 16 bytes per image. A second one would leave
                // readers disagreeing about which index is the real one.
                0xB002 => {
                    if array.is_some() || ty != 7 {
                        return None;
                    }
                    array = Some((val as usize, n as usize));
                }
                _ => {}
            }
        }
        let (at, len) = array?;
        let end = at.checked_add(len)?;
        if len == 0 || len % 16 != 0 || end > p.len() {
            return None;
        }
        // The entries are rewritten in place, so they must not overlap the header or
        // the IFD, and the block must be self-consistent about how many there are.
        if at < 8 || (at < ifd_end && ifd < end) {
            return None;
        }
        if images.is_some_and(|n| n != len / 16) {
            return None;
        }
        let entries = (0..len / 16).map(|k| at + 16 * k + 4).collect();
        Some(Self { entries, ..me })
    }

    fn u16(&self, p: &[u8], at: usize) -> Option<u16> {
        let b: [u8; 2] = p.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, p: &[u8], at: usize) -> Option<u32> {
        let b: [u8; 4] = p.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn put(&self, p: &mut [u8], at: usize, v: u32) {
        let b = if self.little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        p[at..at + 4].copy_from_slice(&b);
    }
}

/// Every MPF block in the primary image, as the segments holding them. A valid
/// file has at most one.
fn find_all_mpf<R: Read + Seek>(
    src: &Structure,
    rd: &mut R,
    primary_end: usize,
) -> Result<Vec<SegmentId>> {
    let mut found = Vec::new();
    for (i, seg) in src.segments()[..primary_end].iter().enumerate() {
        if seg.name() == Name::Marker(APP2) && seg.header().len() == 4 + MPF_SIGNATURE.len() as u64
        {
            let h = seg.header();
            let tail = src.read_range(rd, h.sub(4, MPF_SIGNATURE.len() as u64)?)?;
            if tail == MPF_SIGNATURE {
                found.push(SegmentId::from_index(i)?);
            }
        }
    }
    Ok(found)
}

/// The MPF block of the source, as the segment holding it.
#[cfg(test)]
fn find_mpf<R: Read + Seek>(
    src: &Structure,
    rd: &mut R,
    primary_end: usize,
) -> Result<Option<SegmentId>> {
    Ok(find_all_mpf(src, rd, primary_end)?.first().copied())
}

/// The MPF block rewritten for the output layout, or `None` if no image moved.
///
/// The block records the size and offset of every image, and the offsets count
/// from the block itself, so anything that changes the size of the primary image
/// or moves the block invalidates it. Each recorded value is shifted by how far
/// the image or the block moved. The block has the same length as before, so the
/// layout does not depend on it.
fn fix_mpf<R: Read + Seek>(
    src: &Structure,
    rd: &mut R,
    op_of_src: &[Option<usize>],
    dest: &Structure,
) -> Result<Option<(usize, Vec<u8>)>> {
    let subs = src.subassets();
    let primary_end = subs[0].segments().end;
    if subs.len() < 2 {
        return Ok(None);
    }
    let blocks = find_all_mpf(src, rd, primary_end)?;
    let Some(&id) = blocks.first() else {
        return Ok(None);
    };
    let Some(op) = op_of_src[id.index()] else {
        return Ok(None);
    };
    let old_base = src.segment(id)?.payload().offset();
    let new_base = dest.segments()[op].payload().offset();
    let images: Vec<usize> = (0..subs.len())
        .filter(|&k| subs[k].media_type().is_some())
        .collect();
    let moved = images.iter().any(|&k| {
        let (s, d) = (&subs[k], &dest.subassets()[k]);
        s.range() != d.range()
    }) || old_base != new_base;
    if !moved {
        return Ok(None);
    }
    // Only an edit that must rewrite the index needs to know which one is real.
    if blocks.len() > 1 {
        return Err(Error::Ambiguous {
            what: "MPF block",
            count: blocks.len(),
        });
    }

    let mut payload = src.read_range(rd, src.segment(id)?.payload())?;
    let bad = || Error::InvalidFormat("MPF index is not well formed".into());
    let mismatch = |what: String, recorded: u64, found: u64| {
        Error::InvalidFormat(format!(
            "MPF index does not match the images in the file: {what} is recorded as {recorded}, \
             found {found}"
        ))
    };
    let to_u32 = |v: u64| -> Result<u32> {
        u32::try_from(v).map_err(|_| Error::LimitExceeded {
            what: "MPF value",
            limit: u32::MAX as u64,
        })
    };
    let index = MpIndex::parse(&payload).ok_or_else(bad)?;
    if index.entries.len() != images.len() {
        return Err(mismatch(
            "the number of images".into(),
            index.entries.len() as u64,
            images.len() as u64,
        ));
    }
    for (n, &k) in images.iter().enumerate() {
        let (s, d) = (subs[k].range(), dest.subassets()[k].range());
        let at = index.entries[n];

        // The recorded values must describe the file as it is. Rewriting a wrong
        // index would only move the error. The size of the first image is the one
        // exception: writers in the wild record a few hundred bytes less than the
        // image (every Pixel Ultra HDR sample does), nothing is located by it, and
        // it is rewritten to the true size below. One that claims more than the
        // image is not.
        let size = u64::from(index.u32(&payload, at).ok_or_else(bad)?);
        let size_ok = if n == 0 {
            size <= s.len()
        } else {
            size == s.len()
        };
        if !size_ok {
            return Err(mismatch(format!("the size of image {n}"), size, s.len()));
        }
        index.put(&mut payload, at, to_u32(d.len())?);

        // The first image is at offset 0 by definition; the others are recorded
        // relative to the block, and are what readers find the images by.
        let off = u64::from(index.u32(&payload, at + 4).ok_or_else(bad)?);
        let expected = if n == 0 {
            Some(0)
        } else {
            s.offset().checked_sub(old_base)
        };
        match expected {
            Some(e) if e == off => {}
            e => {
                return Err(mismatch(
                    format!("the offset of image {n}"),
                    off,
                    e.unwrap_or(0),
                ))
            }
        }
        if n > 0 {
            let new = d.offset().checked_sub(new_base).ok_or_else(bad)?;
            index.put(&mut payload, at + 4, to_u32(new)?);
        }
    }
    Ok(Some((op, payload)))
}

/// What the entropy-coded data scanner found.
enum Event {
    /// A restart marker `FF Dn`: offset of its `FF` and the marker byte.
    Rst(u64, u8),
    /// The first marker that is not a restart marker: offset of its `FF`.
    Marker(u64),
    /// End of the scanned range.
    Eof,
}

/// Streaming scan of entropy-coded data. `FF 00` is stuffing and `FF FF` is fill;
/// `FF D0..D7` is a restart marker; any other `FF xx` ends the scan. Fixed 8 KiB
/// buffer, no allocation.
struct EntropyScan {
    next_read: u64,
    end: u64,
    base: u64,
    idx: usize,
    len: usize,
    prev_ff: bool,
    buf: [u8; SCAN_BUF],
}

impl EntropyScan {
    fn new(start: u64, end: u64) -> Self {
        Self {
            next_read: start,
            end,
            base: start,
            idx: 0,
            len: 0,
            prev_ff: false,
            buf: [0; SCAN_BUF],
        }
    }

    fn next<R: Read + Seek>(&mut self, src: &mut R) -> Result<Event> {
        loop {
            if self.idx == self.len {
                if self.next_read >= self.end {
                    return Ok(Event::Eof);
                }
                src.seek(SeekFrom::Start(self.next_read))?;
                let want = (self.end - self.next_read).min(self.buf.len() as u64) as usize;
                let n = src.read(&mut self.buf[..want])?;
                if n == 0 {
                    return Ok(Event::Eof);
                }
                self.base = self.next_read;
                self.next_read += n as u64;
                self.idx = 0;
                self.len = n;
            }
            if !self.prev_ff {
                // Entropy data is almost all non-0xFF: skip to the next 0xFF.
                match memchr::memchr(0xFF, &self.buf[self.idx..self.len]) {
                    Some(p) => {
                        self.idx += p + 1;
                        self.prev_ff = true;
                    }
                    None => self.idx = self.len,
                }
                continue;
            }
            // The previous byte was 0xFF; this one says what it was.
            let byte = self.buf[self.idx];
            let at = self.base + self.idx as u64;
            self.idx += 1;
            match byte {
                0x00 => self.prev_ff = false,
                0xFF => {} // fill: the last FF is the marker prefix
                0xD0..=0xD7 => {
                    self.prev_ff = false;
                    return Ok(Event::Rst(at - 1, byte));
                }
                _ => return Ok(Event::Marker(at - 1)),
            }
        }
    }
}

/// Split a scan segment at its restart markers.
///
/// Yields the scan itself (marker, length, scan header and entropy data up to the
/// first restart marker), then one element per restart marker (`RSTn`, its two
/// bytes and the entropy data that follows). The elements tile the segment.
/// Called through [`Structure::elements`](crate::Structure::elements).
pub(crate) fn subdivide<R: Read + Seek>(
    src: &mut R,
    seg: &Segment,
    emit: &mut dyn FnMut(&Segment) -> Result<()>,
) -> Result<()> {
    let payload = seg.payload();
    let end = seg.extent().end();
    let kind = seg.kind();
    let mut start = seg.extent().offset();
    let mut name = seg.name();
    let mut header_len = seg.header().len() as u32;
    let mut scan = EntropyScan::new(payload.offset(), payload.end());
    loop {
        let (boundary, next) = match scan.next(src)? {
            Event::Rst(at, m) => (at, Some(m)),
            Event::Eof => (end, None),
            Event::Marker(at) => {
                return Err(Error::InvalidSegment {
                    offset: at,
                    reason: "marker inside scan".into(),
                })
            }
        };
        let len = boundary
            .checked_sub(start)
            .ok_or_else(|| Error::InvalidSegment {
                offset: start,
                reason: "restart marker before scan start".into(),
            })?;
        emit(&Segment::new(
            ByteRange::new(start, len)?,
            header_len,
            0,
            kind,
            name,
        )?)?;
        match next {
            Some(m) => {
                start = boundary;
                name = Name::Marker(m);
                header_len = 2;
            }
            None => return Ok(()),
        }
    }
}

// ----------------------------------------------------------------------------
// Planning
// ----------------------------------------------------------------------------

enum Op<'a> {
    /// Copy a source segment unchanged.
    Copy(SegmentId),
    /// Copy a source segment with its payload replaced by one of the same length.
    Patched { id: SegmentId, payload: Vec<u8> },
    /// Standard XMP in one APP1.
    XmpStd { payload: &'a [u8] },
    /// Stub main XMP pointing at extended XMP.
    XmpMain { payload: Vec<u8> },
    /// One extended XMP chunk.
    XmpExt {
        guid: [u8; 32],
        total: u32,
        offset: u32,
        payload: &'a [u8],
    },
    /// One JPEG XT APP11 part of a JUMBF.
    Jumbf {
        seq: u32,
        lbox_tbox: [u8; 8],
        payload: &'a [u8],
    },
}

struct Dims {
    kind: SegmentKind,
    name: Name,
    header_len: u32,
    payload_len: u64,
    trailer_len: u16,
    subdivides: bool,
}

/// Value for a marker's length field, or an error if it does not fit.
fn len_field(header_len: u32, payload_len: u64) -> Result<()> {
    // The field counts itself and everything after the 2-byte marker.
    let v = (header_len as u64 - 2).saturating_add(payload_len);
    if v > MAX_LEN_FIELD {
        return Err(Error::LimitExceeded {
            what: "JPEG segment length",
            limit: MAX_LEN_FIELD,
        });
    }
    Ok(())
}

impl Op<'_> {
    fn dims(&self, src: &Structure) -> Result<Dims> {
        let (kind, marker, header_len, payload_len) = match self {
            Op::Copy(id) | Op::Patched { id, .. } => {
                let s = src.segment(*id)?;
                return Ok(Dims {
                    kind: s.kind(),
                    name: s.name(),
                    header_len: s.header().len() as u32,
                    payload_len: s.payload().len(),
                    trailer_len: s.trailer().len() as u16,
                    subdivides: s.subdivides(),
                });
            }
            Op::XmpStd { payload } => (
                SegmentKind::Xmp,
                APP1,
                4 + XMP_SIG_LEN as u32,
                payload.len() as u64,
            ),
            Op::XmpMain { payload } => (
                SegmentKind::Xmp,
                APP1,
                4 + XMP_SIG_LEN as u32,
                payload.len() as u64,
            ),
            Op::XmpExt { payload, .. } => (
                SegmentKind::Xmp,
                APP1,
                4 + EXT_PREFIX_LEN as u32,
                payload.len() as u64,
            ),
            Op::Jumbf { seq, payload, .. } => (
                SegmentKind::Jumbf,
                APP11,
                if *seq > 1 { 4 + 16 } else { 4 + 8 },
                payload.len() as u64,
            ),
        };
        len_field(header_len, payload_len)?;
        Ok(Dims {
            kind,
            name: Name::Marker(marker),
            header_len,
            payload_len,
            trailer_len: 0,
            subdivides: false,
        })
    }
}

/// Entities of the destination: kind, JUMBF type, and the ops they are made of.
type Groups = Vec<(SegmentKind, Option<[u8; 16]>, Vec<usize>)>;

fn push_new_xmp<'a>(ops: &mut Vec<Op<'a>>, groups: &mut Groups, xmp: &'a [u8]) -> Result<()> {
    let first = ops.len();
    if xmp.len() as u64 <= MAX_STD_XMP {
        ops.push(Op::XmpStd { payload: xmp });
    } else {
        let total = u32::try_from(xmp.len()).map_err(|_| Error::LimitExceeded {
            what: "XMP size",
            limit: u32::MAX as u64,
        })?;
        let guid_str = format!("{:032x}", md5::compute(xmp));
        if guid_str.len() != 32 {
            return Err(Error::InvalidFormat("unexpected GUID length".into()));
        }
        let mut guid = [0u8; 32];
        guid.copy_from_slice(guid_str.as_bytes());
        let stub = format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:xmpNote="http://ns.adobe.com/xmp/note/">
      <xmpNote:HasExtendedXMP>{guid_str}</xmpNote:HasExtendedXMP>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
        );
        ops.push(Op::XmpMain {
            payload: stub.into_bytes(),
        });
        let mut off = 0usize;
        while off < xmp.len() {
            let n = (xmp.len() - off).min(EXT_CHUNK);
            ops.push(Op::XmpExt {
                guid,
                total,
                offset: off as u32,
                payload: &xmp[off..off + n],
            });
            off += n;
        }
    }
    groups.push((SegmentKind::Xmp, None, (first..ops.len()).collect()));
    Ok(())
}

fn push_new_jumbf<'a>(ops: &mut Vec<Op<'a>>, groups: &mut Groups, jumbf: &'a [u8]) -> Result<()> {
    if jumbf.len() < 8 || jumbf[4..8] != JUMB[..] {
        return Err(Error::InvalidFormat(
            "JUMBF must begin with a jumb superbox (LBox, TBox)".into(),
        ));
    }
    let mut lbox_tbox = [0u8; 8];
    lbox_tbox.copy_from_slice(&jumbf[..8]);
    let first = ops.len();
    for (i, chunk) in jumbf.chunks(JUMBF_CHUNK).enumerate() {
        let seq = u32::try_from(i + 1).map_err(|_| Error::LimitExceeded {
            what: "JUMBF parts",
            limit: u32::MAX as u64,
        })?;
        ops.push(Op::Jumbf {
            seq,
            lbox_tbox,
            payload: chunk,
        });
    }
    let ty = crate::containers::jumbf::superbox_type(jumbf);
    groups.push((SegmentKind::Jumbf, ty, (first..ops.len()).collect()));
    Ok(())
}

/// New metadata goes before the first non-application marker.
fn is_insertion_point(seg: &Segment) -> bool {
    seg.kind() != SegmentKind::Header
        && matches!(seg.name(), Name::Marker(m) if !(0xE0..=0xEF).contains(&m) && m != 0xFE)
}

/// Decide what the output contains. Single source of truth for the destination
/// layout and the bytes written.
fn plan<'a, R: Read + Seek>(
    src: &Structure,
    rd: &mut R,
    updates: &'a Updates,
) -> Result<(Vec<Op<'a>>, Structure)> {
    let n = src.segments().len();
    let primary_end = src.subassets()[0].segments().end;
    let mut ops: Vec<Op<'a>> = Vec::with_capacity(n + 2);
    let mut groups: Groups = Vec::new();
    let mut op_of_src: Vec<Option<usize>> = vec![None; n];
    let mut xmp_done = false;
    let mut jumbf_done = false;
    let mut inserted = false;

    // The segments of the JUMBF the update applies to.
    let mut targeted = vec![false; n];
    for (kind, entity) in src.entity_list() {
        if kind == SegmentKind::Jumbf && updates.jumbf_targets(entity.jumbf_type()) {
            for p in entity.parts() {
                targeted[p.segment().index()] = true;
            }
        }
    }

    for (i, seg) in src.segments().iter().enumerate() {
        let id = SegmentId::from_index(i)?;
        match seg.kind() {
            SegmentKind::Xmp => match &updates.xmp {
                MetadataUpdate::Keep => {
                    op_of_src[i] = Some(ops.len());
                    ops.push(Op::Copy(id));
                    xmp_done = true;
                }
                MetadataUpdate::Set(d) if !xmp_done => {
                    push_new_xmp(&mut ops, &mut groups, d)?;
                    xmp_done = true;
                }
                _ => {}
            },
            // JUMBF the update does not target is kept: other types, and all of it
            // when nothing is to change.
            SegmentKind::Jumbf => match &updates.jumbf {
                MetadataUpdate::Set(d) if targeted[i] && !jumbf_done => {
                    push_new_jumbf(&mut ops, &mut groups, d)?;
                    jumbf_done = true;
                }
                MetadataUpdate::Set(_) | MetadataUpdate::Remove if targeted[i] => {}
                _ => {
                    op_of_src[i] = Some(ops.len());
                    ops.push(Op::Copy(id));
                }
            },
            _ => {
                if !inserted && i < primary_end && is_insertion_point(seg) {
                    inserted = true;
                    if !xmp_done {
                        if let MetadataUpdate::Set(d) = &updates.xmp {
                            push_new_xmp(&mut ops, &mut groups, d)?;
                            xmp_done = true;
                        }
                    }
                    if !jumbf_done {
                        if let MetadataUpdate::Set(d) = &updates.jumbf {
                            push_new_jumbf(&mut ops, &mut groups, d)?;
                            jumbf_done = true;
                        }
                    }
                }
                op_of_src[i] = Some(ops.len());
                ops.push(Op::Copy(id));
            }
        }
    }

    // Entities of kept segments carry over as they were.
    for (kind, entity) in src.entity_list() {
        let idx: Option<Vec<usize>> = entity
            .parts()
            .map(|p| op_of_src[p.segment().index()])
            .collect();
        if let Some(idx) = idx {
            groups.push((kind, entity.jumbf_type(), idx));
        }
    }

    // Destination size, then destination layout.
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
    // Where the sub-assets after the primary start in the output.
    let mut starts = Vec::new();
    for sub in &src.subassets()[1..] {
        let op = op_of_src[sub.segments().start]
            .ok_or_else(|| Error::InvalidFormat("sub-asset start was dropped".into()))?;
        starts.push((op, sub.media_type().map(|m| m.to_owned().into())));
    }
    let mut starts = starts.into_iter().peekable();
    // Where the data beyond the first image's structure starts in the output.
    let trailing_op = match src.box_count() {
        k if k < n => Some(
            op_of_src[k].ok_or_else(|| Error::InvalidFormat("trailing data was dropped".into()))?,
        ),
        _ => None,
    };
    for (i, d) in dims.iter().enumerate() {
        if trailing_op == Some(i) {
            b.end_boxes()?;
        }
        if let Some((_, media)) = starts.next_if(|(op, _)| *op == i) {
            b.begin_subasset(media)?;
        }
        let id = b.push_span(d.kind, d.name, d.header_len, d.payload_len, d.trailer_len)?;
        if d.subdivides {
            b.mark_subdivides(id)?;
        }
    }
    for (kind, ty, idx) in &groups {
        let mut ids = Vec::with_capacity(idx.len());
        for &i in idx {
            ids.push(SegmentId::from_index(i)?);
        }
        b.entity_of_segments_typed(*kind, *ty, &ids)?;
    }
    let dest = b.finish()?;
    if let Some((op, payload)) = fix_mpf(src, rd, &op_of_src, &dest)? {
        let Op::Copy(id) = ops[op] else {
            return Err(Error::InvalidFormat("MPF block was not kept".into()));
        };
        ops[op] = Op::Patched { id, payload };
    }
    Ok((ops, dest))
}

fn put_marker_len(hdr: &mut [u8], marker: u8, header_len: u32, payload_len: u64) -> Result<()> {
    let v = (header_len as u64 - 2).saturating_add(payload_len);
    let v = u16::try_from(v).map_err(|_| Error::LimitExceeded {
        what: "JPEG segment length",
        limit: MAX_LEN_FIELD,
    })?;
    hdr[0] = 0xFF;
    hdr[1] = marker;
    hdr[2..4].copy_from_slice(&v.to_be_bytes());
    Ok(())
}

fn emit_new<W: Write, S: Sink + ?Sized>(em: &mut Emitter<'_, W, S>, op: &Op<'_>) -> Result<()> {
    let mut hdr = [0u8; MAX_HEADER];
    let (n, payload) = encode_new(op, &mut hdr)?;
    em.put(Role::Header, &hdr[..n])?;
    em.put(Role::Payload, payload)
}

/// Build the header of a new segment in `hdr`; returns its length and the payload.
fn encode_new<'o>(op: &'o Op<'_>, hdr: &mut [u8; MAX_HEADER]) -> Result<(usize, &'o [u8])> {
    let (n, payload): (usize, &[u8]) = match op {
        Op::Copy(_) | Op::Patched { .. } => {
            return Err(Error::InvalidFormat("not a new segment".into()))
        }
        Op::XmpStd { payload } => {
            let h = 4 + XMP_SIG_LEN;
            put_marker_len(&mut hdr[..], APP1, h as u32, payload.len() as u64)?;
            hdr[4..h].copy_from_slice(XMP_SIGNATURE);
            (h, payload)
        }
        Op::XmpMain { payload } => {
            let h = 4 + XMP_SIG_LEN;
            put_marker_len(&mut hdr[..], APP1, h as u32, payload.len() as u64)?;
            hdr[4..h].copy_from_slice(XMP_SIGNATURE);
            (h, payload.as_slice())
        }
        Op::XmpExt {
            guid,
            total,
            offset,
            payload,
        } => {
            let h = 4 + EXT_PREFIX_LEN;
            put_marker_len(&mut hdr[..], APP1, h as u32, payload.len() as u64)?;
            let mut p = 4;
            hdr[p..p + EXT_SIG_LEN].copy_from_slice(XMP_EXT_SIGNATURE);
            p += EXT_SIG_LEN;
            hdr[p..p + 32].copy_from_slice(guid);
            p += 32;
            hdr[p..p + 4].copy_from_slice(&total.to_be_bytes());
            p += 4;
            hdr[p..p + 4].copy_from_slice(&offset.to_be_bytes());
            (h, payload)
        }
        Op::Jumbf {
            seq,
            lbox_tbox,
            payload,
        } => {
            let h = if *seq > 1 { 4 + 16 } else { 4 + 8 };
            put_marker_len(&mut hdr[..], APP11, h as u32, payload.len() as u64)?;
            hdr[4..6].copy_from_slice(&JP);
            hdr[6..8].copy_from_slice(&XT_EN.to_be_bytes());
            hdr[8..12].copy_from_slice(&seq.to_be_bytes());
            if *seq > 1 {
                hdr[12..20].copy_from_slice(lbox_tbox);
            }
            (h, payload)
        }
    };
    Ok((n, payload))
}

/// The segments `write` inserts for new XMP or JUMBF, as one byte string.
fn compose(kind: SegmentKind, data: &[u8]) -> Result<Vec<u8>> {
    let mut ops = Vec::new();
    let mut groups = Groups::new();
    match kind {
        SegmentKind::Jumbf => push_new_jumbf(&mut ops, &mut groups, data)?,
        SegmentKind::Xmp => push_new_xmp(&mut ops, &mut groups, data)?,
        _ => return Err(Error::UnsupportedFormat),
    }
    let mut out = Vec::with_capacity(data.len() + ops.len() * MAX_HEADER);
    for op in &ops {
        let mut hdr = [0u8; MAX_HEADER];
        let (n, payload) = encode_new(op, &mut hdr)?;
        out.extend_from_slice(&hdr[..n]);
        out.extend_from_slice(payload);
    }
    Ok(out)
}

/// Write `src` with `updates` applied, delivering every output byte to `sink`.
///
/// Unchanged segments are copied byte for byte. `out` is written from its
/// current start; offsets reported to the sink are relative to the first byte
/// written. Returns the layout of the output.
pub fn write<R, W, S>(
    src_structure: &Structure,
    src: &mut R,
    out: &mut W,
    updates: &Updates,
    sink: &mut S,
) -> Result<Structure>
where
    R: Read + Seek,
    W: Write,
    S: Sink + ?Sized,
{
    let (ops, dest) = plan(src_structure, src, updates)?;
    sink.begin(&dest)?;

    let chunk = write_buffer_len(dest.total_size());
    let mut buf = vec![0u8; chunk];
    // Source regions of consecutive copied segments that are contiguous in `src` are
    // read and written as one run; see `Emitter::copy_run`. A run is flushed at a
    // gap, before any segment that is not a plain copy, and once it spans
    // RUN_WINDOWS buffers.
    let run_limit = (chunk as u64).saturating_mul(RUN_WINDOWS);
    let mut pieces: Vec<Piece> = Vec::new();
    let mut run_len = 0u64;
    {
        let mut em = Emitter::new(out, sink, &dest);
        for (i, op) in ops.iter().enumerate() {
            let id = SegmentId::from_index(i)?;
            match op {
                Op::Copy(sid) => {
                    let s = src_structure.segment(*sid)?;
                    for (role, range) in s.regions() {
                        if pieces
                            .last()
                            .is_some_and(|p| p.range.end() != range.offset())
                        {
                            flush_run(&mut em, src, &mut pieces, &mut buf)?;
                            run_len = 0;
                        }
                        run_len = run_len.saturating_add(range.len());
                        pieces.push(Piece {
                            dest: id,
                            role,
                            range,
                        });
                    }
                    if run_len >= run_limit {
                        flush_run(&mut em, src, &mut pieces, &mut buf)?;
                        run_len = 0;
                    }
                }
                Op::Patched {
                    id: patched,
                    payload,
                } => {
                    flush_run(&mut em, src, &mut pieces, &mut buf)?;
                    run_len = 0;
                    em.begin_segment(id)?;
                    let s = src_structure.segment(*patched)?;
                    for (role, range) in s.regions() {
                        if role == Role::Payload {
                            em.put(role, payload)?;
                        } else {
                            em.copy(src, role, range, &mut buf)?;
                        }
                    }
                }
                _ => {
                    flush_run(&mut em, src, &mut pieces, &mut buf)?;
                    run_len = 0;
                    em.begin_segment(id)?;
                    emit_new(&mut em, op)?;
                }
            }
        }
        flush_run(&mut em, src, &mut pieces, &mut buf)?;
        em.finish()?;
    }
    Ok(dest)
}

// ----------------------------------------------------------------------------
// Reading
// ----------------------------------------------------------------------------

/// The JUMBF bytes (XT framing removed, parts concatenated), or `None`.
/// Several distinct JUMBF boxes is an error.
pub fn read_jumbf<R: Read + Seek>(s: &Structure, src: &mut R) -> Result<Option<Vec<u8>>> {
    match s.entity(SegmentKind::Jumbf)? {
        Some(e) => s.read_entity(src, e).map(Some),
        None => Ok(None),
    }
}

/// The XMP packet, or `None`. With extended XMP, the reassembled extended
/// packet is returned (the standard packet only carries a pointer to it).
pub fn read_xmp<R: Read + Seek>(s: &Structure, src: &mut R) -> Result<Option<Vec<u8>>> {
    let Some(e) = s.entity(SegmentKind::Xmp)? else {
        return Ok(None);
    };
    if e.part_count() == 1 {
        return s.read_entity(src, e).map(Some);
    }

    let parts: Vec<&Part> = e.parts().collect();
    let mut guid: Option<[u8; 32]> = None;
    let mut total: Option<u32> = None;
    let mut capacity = 0u64;
    let mut chunks: Vec<(ByteRange, u32)> = Vec::with_capacity(parts.len() - 1);

    for p in &parts[1..] {
        let header = s.segment(p.segment())?.header();
        if header.len() < 40 {
            return Err(Error::InvalidSegment {
                offset: header.offset(),
                reason: "extended XMP header too short".into(),
            });
        }
        let tail = s.read_range(src, header.sub(header.len() - 40, 40)?)?;
        let mut g = [0u8; 32];
        g.copy_from_slice(&tail[..32]);
        let t = u32::from_be_bytes([tail[32], tail[33], tail[34], tail[35]]);
        let off = u32::from_be_bytes([tail[36], tail[37], tail[38], tail[39]]);
        match (guid, total) {
            (Some(pg), Some(pt)) if pg != g || pt != t => {
                return Err(Error::InvalidFormat(
                    "inconsistent extended XMP parts".into(),
                ));
            }
            _ => {
                guid = Some(g);
                total = Some(t);
            }
        }
        capacity = capacity.saturating_add(p.range().len());
        chunks.push((p.range(), off));
    }

    let total = total.ok_or(Error::NotFound {
        what: "extended XMP",
    })?;
    // The allocation is bounded by data actually present in the file.
    if total > MAX_EXT_XMP || total as u64 > capacity {
        return Err(Error::LimitExceeded {
            what: "extended XMP size",
            limit: capacity.min(MAX_EXT_XMP as u64),
        });
    }
    let mut out = vec![0u8; total as usize];
    for (range, off) in chunks {
        let end = off as u64 + range.len();
        if end > total as u64 {
            return Err(Error::InvalidSegment {
                offset: range.offset(),
                reason: "extended XMP chunk outside declared length".into(),
            });
        }
        let data = s.read_range(src, range)?;
        out[off as usize..end as usize].copy_from_slice(&data);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::{Chunk, NullSink};
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    fn marker(m: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, m];
        v.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    fn sample_jpeg() -> Vec<u8> {
        let mut v = vec![0xFF, SOI];
        v.extend(marker(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        v.extend(marker(0xDB, &[0u8; 65]));
        v.extend(marker(0xDB, &[1u8; 65])); // second DQT
        v.extend(marker(0xC0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]));
        v.extend(marker(0xC4, &[0u8; 20]));
        v.extend(marker(SOS, &[1, 1, 0, 0, 0x3F, 0]));
        v.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00, 0x56]);
        v.extend_from_slice(&[0xFF, EOI]);
        v
    }

    fn fake_jumbf(n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
        v[4..8].copy_from_slice(b"jumb");
        for (i, b) in v[8..].iter_mut().enumerate() {
            *b = ((i * 7) % 255) as u8 + 1;
        }
        v
    }

    fn do_write(jpeg: &[u8], updates: &Updates) -> (Vec<u8>, Structure) {
        let s = parse(&mut Cursor::new(jpeg.to_vec())).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(jpeg.to_vec()),
            &mut out,
            updates,
            &mut NullSink,
        )
        .unwrap();
        (out.into_inner(), dest)
    }

    #[test]
    fn box_names() {
        let n = |m| marker_symbol(m).into_owned();
        assert_eq!(n(0xD8), "SOI");
        assert_eq!(n(0xD9), "EOI");
        assert_eq!(n(0xDA), "SOS");
        assert_eq!(n(0xE0), "APP0");
        assert_eq!(n(0xEB), "APP11");
        assert_eq!(n(0xEF), "APP15");
        assert_eq!(n(0xC0), "SOF0");
        assert_eq!(n(0xC2), "SOF2");
        assert_eq!(n(0xC4), "DHT");
        assert_eq!(n(0xCC), "DAC");
        assert_eq!(n(0xD0), "RST0");
        assert_eq!(n(0xD7), "RST7");
        assert_eq!(n(0xDB), "DQT");
        assert_eq!(n(0xDD), "DRI");
        assert_eq!(n(0xF0), "JPG0");
        assert_eq!(n(0xFD), "JPG13");
        assert_eq!(n(0xFE), "COM");
        assert_eq!(n(0x10), "RES");
        assert!(JpegHandler.box_name(Name::None).is_none());
    }

    #[test]
    fn parse_tiles_file() {
        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        let mut next = 0;
        for r in s.regions() {
            assert_eq!(r.range.offset(), next);
            next = r.range.end();
        }
        assert_eq!(next, j.len() as u64);
        let scan = s
            .segments()
            .iter()
            .find(|g| g.kind() == SegmentKind::ImageData)
            .unwrap();
        assert_eq!(scan.payload().len(), 5);
    }

    #[test]
    fn keep_is_byte_identical_including_repeated_markers() {
        let j = sample_jpeg();
        let (out, _) = do_write(&j, &Updates::new());
        assert_eq!(out, j);
    }

    #[test]
    fn fill_bytes_and_trailing_data_round_trip() {
        let mut j = vec![0xFF, SOI];
        j.extend(marker(0xE0, b"JFIF\0"));
        j.push(0xFF); // fill byte before the next marker
        j.extend(marker(0xDB, &[0u8; 65]));
        j.extend_from_slice(&[0xFF, EOI]);
        j.extend_from_slice(b"trailing");
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        assert_eq!(s.total_size(), j.len() as u64);
        let (out, _) = do_write(&j, &Updates::new());
        assert_eq!(out, j);
    }

    #[test]
    fn rejects_malformed_input() {
        let j = sample_jpeg();
        assert!(parse(&mut Cursor::new(j[..j.len() - 10].to_vec())).is_err());
        let bad = vec![0xFF, SOI, 0xFF, 0xE0, 0xFF, 0xFF, 1, 2, 3];
        assert!(parse(&mut Cursor::new(bad)).is_err());
        let short_len = vec![0xFF, SOI, 0xFF, 0xE0, 0x00, 0x01, 0xFF, EOI];
        assert!(parse(&mut Cursor::new(short_len)).is_err());
        assert!(parse(&mut Cursor::new(vec![1, 2, 3, 4])).is_err());
    }

    #[test]
    fn minimal_file_still_gets_metadata() {
        let j = vec![0xFF, SOI, 0xFF, EOI];
        let updates = Updates::new().set_jumbf(fake_jumbf(100));
        let (out, dest) = do_write(&j, &updates);
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(
            dest.read_entity(&mut Cursor::new(out), e).unwrap(),
            fake_jumbf(100)
        );
    }

    #[test]
    fn single_part_jumbf_round_trip() {
        let j = sample_jpeg();
        let data = fake_jumbf(1000);
        let (out, dest) = do_write(&j, &Updates::new().set_jumbf(data.clone()));
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        let e = re.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.part_count(), 1);
        assert_eq!(
            read_jumbf(&re, &mut Cursor::new(out)).unwrap().unwrap(),
            data
        );
        assert_eq!(dest.total_size(), re.total_size());
    }

    #[test]
    fn multi_part_jumbf_is_one_entity_with_visible_headers() {
        let j = sample_jpeg();
        let data = fake_jumbf(150_000);
        let (out, _) = do_write(&j, &Updates::new().set_jumbf(data.clone()));
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        let e = re.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.part_count(), 3);
        assert_eq!(e.len(), 150_000);
        assert_eq!(
            re.read_entity(&mut Cursor::new(out.clone()), e).unwrap(),
            data
        );
        // Continuation headers repeat LBox/TBox and are header, not payload.
        for p in e.parts().skip(1) {
            let h = re.segment(p.segment()).unwrap().header();
            let tail = &out[(h.end() - 8) as usize..h.end() as usize];
            assert_eq!(tail, &data[..8]);
        }
    }

    #[test]
    fn two_distinct_jumbf_boxes_are_ambiguous() {
        let j = sample_jpeg();
        let (out, _) = do_write(&j, &Updates::new().set_jumbf(fake_jumbf(100)));
        // Append a second, unrelated JUMBF APP11 (new box instance, Z = 1).
        let mut second = Vec::new();
        let body = fake_jumbf(50);
        let mut data = b"JP".to_vec();
        data.extend_from_slice(&0x9999u16.to_be_bytes());
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&body);
        second.extend(marker(APP11, &data));
        // Insert right after SOI. (Before EOI would fall inside the scan data.)
        let at = 2;
        let mut two = out[..at].to_vec();
        two.extend(second);
        two.extend_from_slice(&out[at..]);
        let s = parse(&mut Cursor::new(two)).unwrap();
        assert!(matches!(
            s.entity(SegmentKind::Jumbf),
            Err(Error::Ambiguous { count: 2, .. })
        ));
        assert_eq!(s.entities(SegmentKind::Jumbf).count(), 2);
    }

    /// Progressive-style file: two scans, restart markers in the first, and a
    /// manifest placed after the last scan.
    fn multi_scan_jpeg() -> Vec<u8> {
        let mut j = vec![0xFF, SOI];
        j.extend(marker(0xE0, b"JFIF\0"));
        j.extend(marker(0xC2, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]));
        j.extend(marker(0xC4, &[0u8; 20]));
        j.extend(marker(SOS, &[1, 1, 0, 0, 0x3F, 0]));
        // stuffed byte, RST0, fill + RST1
        j.extend_from_slice(&[
            0x11, 0xFF, 0x00, 0x22, 0xFF, 0xD0, 0x33, 0xFF, 0xFF, 0xD1, 0x44,
        ]);
        j.extend(marker(0xC4, &[0u8; 20]));
        j.extend(marker(SOS, &[1, 1, 0, 0, 0x3F, 0]));
        j.extend_from_slice(&[0x55, 0x66]);
        let mut d = b"JP".to_vec();
        d.extend_from_slice(&0x0211u16.to_be_bytes());
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend(fake_jumbf(60));
        j.extend(marker(APP11, &d));
        j.extend_from_slice(&[0xFF, EOI]);
        j
    }

    fn elements_of(s: &Structure, j: &[u8]) -> Vec<(Name, ByteRange)> {
        let mut els = Vec::new();
        s.elements(&mut Cursor::new(j.to_vec()), |_, e| {
            els.push((e.name(), e.extent()));
            Ok(())
        })
        .unwrap();
        els
    }

    #[test]
    fn replacing_a_manifest_keeps_its_offset() {
        // C2PA 11.2.3: for an update manifest the store must keep its file offset;
        // only its length may change.
        let j = sample_jpeg();
        let (with, d1) = do_write(&j, &Updates::new().set_jumbf(fake_jumbf(100)));
        let start = |s: &Structure| {
            let e = s.require_entity(SegmentKind::Jumbf).unwrap();
            let first = e.parts().next().unwrap();
            s.segment(first.segment()).unwrap().extent().offset()
        };
        for n in [60usize, 100, 70_000] {
            let (out, d2) = do_write(&with, &Updates::new().set_jumbf(fake_jumbf(n)));
            assert_eq!(start(&d2), start(&d1), "offset moved for {n} bytes");
            // Everything before the manifest is byte-identical.
            let at = start(&d1) as usize;
            assert_eq!(out[..at], with[..at]);
        }
    }

    #[test]
    fn multi_scan_and_restart_markers() {
        let j = multi_scan_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();

        // Two scans; only the first has restart markers.
        let scans: Vec<_> = s
            .segments()
            .iter()
            .filter(|g| g.kind() == SegmentKind::ImageData)
            .collect();
        assert_eq!(scans.len(), 2);
        assert!(scans[0].subdivides());
        assert!(!scans[1].subdivides());

        // The manifest after the last scan is found.
        assert_eq!(s.entities(SegmentKind::Jumbf).count(), 1);

        // elements() expands the scan and tiles the file.
        let els = elements_of(&s, &j);
        let names: Vec<Name> = els.iter().map(|(n, _)| *n).collect();
        let m = Name::Marker;
        assert_eq!(
            names,
            [
                m(SOI),
                m(0xE0),
                m(0xC2),
                m(0xC4),
                m(SOS),
                m(0xD0),
                m(0xD1),
                m(0xC4),
                m(SOS),
                m(APP11),
                m(EOI)
            ]
        );
        let mut next = 0;
        for (name, r) in &els {
            assert_eq!(r.offset(), next);
            next = r.end();
            if let Name::Marker(c @ 0xD0..=0xD7) = name {
                assert_eq!(
                    &j[r.offset() as usize..r.offset() as usize + 2],
                    &[0xFF, *c]
                );
            }
        }
        assert_eq!(next, j.len() as u64);

        // Round trip preserves layout, including the subdivision flag.
        let (out, dest) = do_write(&j, &Updates::new());
        assert_eq!(out, j);
        assert_eq!(dest.segments(), s.segments());

        // Set replaces the manifest that follows the scans; no second one appears.
        let (out, dest) = do_write(&j, &Updates::new().set_jumbf(fake_jumbf(200)));
        assert_eq!(dest.entities(SegmentKind::Jumbf).count(), 1);
        let re = parse(&mut Cursor::new(out)).unwrap();
        assert_eq!(re.segments(), dest.segments());
    }

    #[test]
    fn restart_markers_do_not_inflate_the_structure() {
        let mut j = vec![0xFF, SOI];
        j.extend(marker(SOS, &[1, 1, 0, 0, 0x3F, 0]));
        for i in 0..20_000u32 {
            j.extend_from_slice(&[0x01, 0xFF, 0xD0 + (i % 8) as u8]);
        }
        j.extend_from_slice(&[0xFF, EOI]);
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        assert!(s.segments().len() <= 4, "{} segments", s.segments().len());
        // SOI, the scan up to its first restart marker, 20 000 restart elements, EOI.
        assert_eq!(elements_of(&s, &j).len(), 1 + 1 + 20_000 + 1);
    }

    #[test]
    fn elements_pass_through_when_nothing_subdivides() {
        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        let els = elements_of(&s, &j);
        assert_eq!(els.len(), s.segments().len());
        for ((_, r), g) in els.iter().zip(s.segments()) {
            assert_eq!(*r, g.extent());
        }
    }

    #[test]
    fn jumbf_segments_must_be_contiguous() {
        // C2PA A.3.1: APP11 segments of one manifest are sequential and contiguous.
        let j = sample_jpeg();
        let body = fake_jumbf(100);
        let packet = |z: u32, data: &[u8]| {
            let mut d = b"JP".to_vec();
            d.extend_from_slice(&0x0211u16.to_be_bytes());
            d.extend_from_slice(&z.to_be_bytes());
            d.extend_from_slice(data);
            marker(APP11, &d)
        };
        let p1 = packet(1, &body[..60]);
        let mut rest = body[..8].to_vec(); // repeated LBox/TBox
        rest.extend_from_slice(&body[60..]);
        let p2 = packet(2, &rest);
        let build = |between: &[u8]| {
            let mut v = j[..2].to_vec();
            v.extend(&p1);
            v.extend_from_slice(between);
            v.extend(&p2);
            v.extend_from_slice(&j[2..]);
            v
        };

        let ok = parse(&mut Cursor::new(build(&[]))).unwrap();
        let e = ok.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.part_count(), 2);

        let gap = marker(0xFE, b"comment"); // COM between the packets
        assert!(matches!(
            parse(&mut Cursor::new(build(&gap))),
            Err(Error::InvalidFormat(_))
        ));
    }

    #[test]
    fn jumbf_type_comes_from_the_description_box() {
        let uuid = [
            0x63, 0x32, 0x70, 0x61, 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
            0x9B, 0x71,
        ];
        let mut b = Vec::new();
        b.extend_from_slice(&60u32.to_be_bytes());
        b.extend_from_slice(b"jumb");
        b.extend_from_slice(&29u32.to_be_bytes());
        b.extend_from_slice(b"jumd");
        b.extend_from_slice(&uuid);
        b.push(3);
        b.extend_from_slice(b"c2pa\0");
        b.resize(60, 0);

        let (out, dest) = do_write(&sample_jpeg(), &Updates::new().set_jumbf(b));
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(
            dest.jumbf_type(&mut Cursor::new(out), e).unwrap(),
            Some(uuid)
        );

        // A JUMBF box without a description box has no type.
        let (out, dest) = do_write(&sample_jpeg(), &Updates::new().set_jumbf(fake_jumbf(100)));
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(dest.jumbf_type(&mut Cursor::new(out), e).unwrap(), None);
    }

    #[test]
    fn zero_based_xt_packet_is_recognised() {
        // Some writers (e.g. Designer.jpeg) number the first XT packet Z = 0.
        let j = sample_jpeg();
        let body = fake_jumbf(100);
        let mut data = b"JP".to_vec();
        data.extend_from_slice(&0xF400u16.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&body);
        let mut with = j[..2].to_vec();
        with.extend(marker(APP11, &data));
        with.extend_from_slice(&j[2..]);

        let s = parse(&mut Cursor::new(with.clone())).unwrap();
        assert_eq!(s.entities(SegmentKind::Jumbf).count(), 1);
        let e = s.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(
            s.read_entity(&mut Cursor::new(with.clone()), e).unwrap(),
            body
        );

        // Set replaces it rather than adding a second manifest.
        let new = fake_jumbf(200);
        let (out, dest) = do_write(&with, &Updates::new().set_jumbf(new));
        assert_eq!(dest.entities(SegmentKind::Jumbf).count(), 1);
        assert_eq!(
            parse(&mut Cursor::new(out))
                .unwrap()
                .entities(SegmentKind::Jumbf)
                .count(),
            1
        );
    }

    /// A JUMBF superbox of type `[ty; 16]` with `n` payload bytes.
    fn typed_jumbf(ty: u8, n: usize) -> Vec<u8> {
        let mut desc = vec![ty; 16];
        desc.extend_from_slice(&[3, b'x', 0]);
        let mut jumd = ((desc.len() + 8) as u32).to_be_bytes().to_vec();
        jumd.extend_from_slice(b"jumd");
        jumd.extend_from_slice(&desc);
        let mut child = ((n + 8) as u32).to_be_bytes().to_vec();
        child.extend_from_slice(b"json");
        child.extend((0..n).map(|i| (i % 251) as u8 + 1));
        let mut v = ((jumd.len() + child.len() + 8) as u32)
            .to_be_bytes()
            .to_vec();
        v.extend_from_slice(b"jumb");
        v.extend(jumd);
        v.extend(child);
        v
    }

    fn jumbf_of(out: &[u8], s: &Structure, e: &crate::Entity) -> Vec<u8> {
        s.read_entity(&mut Cursor::new(out.to_vec()), e).unwrap()
    }

    /// A file holding one JUMBF of type 1 and one of type 2.
    fn two_stores() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (a, b) = (typed_jumbf(1, 300), typed_jumbf(2, 200));
        let (one, _) = do_write(&sample_jpeg(), &Updates::new().set_jumbf(a.clone()));
        let (two, _) = do_write(&one, &Updates::new().set_jumbf(b.clone()));
        (two, a, b)
    }

    #[test]
    fn jumbf_of_another_type_is_added_not_replaced() {
        let (file, a, b) = two_stores();
        let s = parse(&mut Cursor::new(file.clone())).unwrap();
        let found: Vec<_> = s
            .entities(SegmentKind::Jumbf)
            .map(|e| (e.jumbf_type(), jumbf_of(&file, &s, e)))
            .collect();
        assert_eq!(found, [(Some([1; 16]), a), (Some([2; 16]), b)]);
        // The single-entity accessor refuses to guess between them.
        assert!(matches!(
            s.entity(SegmentKind::Jumbf),
            Err(Error::Ambiguous { .. })
        ));
    }

    #[test]
    fn set_replaces_only_its_own_type() {
        let (file, _, b) = two_stores();
        let a2 = typed_jumbf(1, 5000);
        let (out, dest) = do_write(&file, &Updates::new().set_jumbf(a2.clone()));
        let found: Vec<_> = dest
            .entities(SegmentKind::Jumbf)
            .map(|e| (e.jumbf_type(), jumbf_of(&out, &dest, e)))
            .collect();
        assert_eq!(found, [(Some([1; 16]), a2), (Some([2; 16]), b)]);
        // What was written reads back the same.
        let again = parse(&mut Cursor::new(out)).unwrap();
        assert_eq!(again.entities(SegmentKind::Jumbf).count(), 2);
    }

    #[test]
    fn remove_of_a_type_keeps_the_others() {
        let (file, a, b) = two_stores();
        let (out, dest) = do_write(&file, &Updates::new().remove_jumbf_of([1; 16]));
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.jumbf_type(), Some([2; 16]));
        assert_eq!(jumbf_of(&out, &dest, e), b);

        let (out, dest) = do_write(&file, &Updates::new().remove_jumbf_of([2; 16]));
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(jumbf_of(&out, &dest, e), a);

        // A type that is not there changes nothing.
        let (out, _) = do_write(&file, &Updates::new().remove_jumbf_of([9; 16]));
        assert_eq!(out, file);

        // Removing without a type removes them all.
        let (_, dest) = do_write(&file, &Updates::new().remove_jumbf());
        assert_eq!(dest.entities(SegmentKind::Jumbf).count(), 0);
        let (out, _) = do_write(&file, &Updates::remove_all());
        assert_eq!(
            parse(&mut Cursor::new(out))
                .unwrap()
                .entities(SegmentKind::Jumbf)
                .count(),
            0
        );
    }

    #[test]
    fn keep_preserves_every_type() {
        let (file, _, _) = two_stores();
        let (out, _) = do_write(&file, &Updates::new());
        assert_eq!(out, file);
        let (out, _) = do_write(&file, &Updates::new().set_xmp(b"<x/>".to_vec()));
        let s = parse(&mut Cursor::new(out)).unwrap();
        assert_eq!(s.entities(SegmentKind::Jumbf).count(), 2);
    }

    #[test]
    fn a_set_without_a_readable_type_replaces_all_jumbf() {
        let (file, _, _) = two_stores();
        let (_, dest) = do_write(&file, &Updates::new().set_jumbf(fake_jumbf(100)));
        assert_eq!(dest.entities(SegmentKind::Jumbf).count(), 1);
        // And unknown-type JUMBF is not touched by an update of one type.
        let (with_fake, _) = do_write(&sample_jpeg(), &Updates::new().set_jumbf(fake_jumbf(100)));
        let (out, dest) = do_write(&with_fake, &Updates::new().set_jumbf(typed_jumbf(1, 50)));
        assert_eq!(dest.entities(SegmentKind::Jumbf).count(), 2);
        assert_eq!(
            parse(&mut Cursor::new(out))
                .unwrap()
                .entities(SegmentKind::Jumbf)
                .count(),
            2
        );
    }

    #[test]
    fn a_large_typed_jumbf_records_its_type_from_the_first_packet() {
        let big = typed_jumbf(7, 150_000);
        let (out, dest) = do_write(&sample_jpeg(), &Updates::new().set_jumbf(big.clone()));
        let s = parse(&mut Cursor::new(out.clone())).unwrap();
        for st in [&dest, &s] {
            let e = st.require_entity(SegmentKind::Jumbf).unwrap();
            assert!(e.part_count() > 1);
            assert_eq!(e.jumbf_type(), Some([7; 16]));
        }
        assert_eq!(
            jumbf_of(&out, &s, s.require_entity(SegmentKind::Jumbf).unwrap()),
            big
        );
    }

    #[test]
    fn rejects_invalid_new_jumbf() {
        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        for bad in [vec![], vec![0u8; 7], vec![0u8; 100]] {
            let updates = Updates::new().set_jumbf(bad);
            let r = write(
                &s,
                &mut Cursor::new(j.clone()),
                &mut Cursor::new(Vec::new()),
                &updates,
                &mut NullSink,
            );
            assert!(r.is_err());
        }
    }

    #[test]
    fn sink_sees_every_byte_once_in_order() {
        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        let mut seen = Vec::new();
        let mut next = 0u64;
        let mut sink = |c: Chunk<'_>| -> Result<()> {
            assert_eq!(c.offset, next);
            next += c.data.len() as u64;
            seen.extend_from_slice(c.data);
            Ok(())
        };
        let updates = Updates::new().set_jumbf(fake_jumbf(150_000));
        let mut out = Cursor::new(Vec::new());
        write(&s, &mut Cursor::new(j), &mut out, &updates, &mut sink).unwrap();
        assert_eq!(seen, out.into_inner());
    }

    /// The case that motivated the redesign: every part's payload is excluded,
    /// every part's header is hashed. Three independent routes must agree.
    #[test]
    fn multi_part_exclusion_three_routes_agree() {
        struct Hash(Sha256);
        impl Sink for Hash {
            fn wants(&self, seg: &Segment, role: Role) -> bool {
                !(seg.kind() == SegmentKind::Jumbf && role == Role::Payload)
            }
            fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
                self.0.update(c.data);
                Ok(())
            }
        }

        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        let updates = Updates::new().set_jumbf(fake_jumbf(150_000));
        let mut sink = Hash(Sha256::new());
        let mut out = Cursor::new(Vec::new());
        let dest = write(&s, &mut Cursor::new(j), &mut out, &updates, &mut sink).unwrap();
        let bytes = out.into_inner();
        let streamed = sink.0.finalize().to_vec();

        // Route 2: walk the finished layout.
        let mut walked = Sha256::new();
        let mut src = Cursor::new(bytes.clone());
        let mut buf = vec![0u8; 4096];
        for r in dest.regions() {
            let seg = dest.segment(r.segment).unwrap();
            if seg.kind() == SegmentKind::Jumbf && r.role == Role::Payload {
                continue;
            }
            dest.stream_range(&mut src, r.range, &mut buf, |b| {
                walked.update(b);
                Ok(())
            })
            .unwrap();
        }

        // Route 3: hash everything outside the entity's payload parts.
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.part_count(), 3);
        let mut outside = Sha256::new();
        let mut pos = 0usize;
        for p in e.parts() {
            let r = p.range();
            outside.update(&bytes[pos..r.offset() as usize]);
            pos = r.end() as usize;
        }
        outside.update(&bytes[pos..]);

        assert_eq!(streamed, walked.finalize().to_vec());
        assert_eq!(streamed, outside.finalize().to_vec());
    }

    #[test]
    fn patch_multi_part_changes_only_payloads() {
        let j = sample_jpeg();
        let mut placeholder = vec![0u8; 150_000];
        placeholder[0..4].copy_from_slice(&150_000u32.to_be_bytes());
        placeholder[4..8].copy_from_slice(b"jumb");
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(j),
            &mut out,
            &Updates::new().set_jumbf(placeholder),
            &mut NullSink,
        )
        .unwrap();
        let before = out.get_ref().clone();

        let e = dest.require_entity(SegmentKind::Jumbf).unwrap().clone();
        let signed = fake_jumbf(140_000);
        dest.patch(&mut out, &e, &signed).unwrap();
        let after = out.into_inner();

        assert_eq!(before.len(), after.len());
        let in_payload = |i: usize| {
            e.parts()
                .any(|p| p.range().contains(&ByteRange::new(i as u64, 1).unwrap()))
        };
        for i in 0..before.len() {
            if !in_payload(i) {
                assert_eq!(before[i], after[i], "byte {i} outside payload changed");
            }
        }
        let re = parse(&mut Cursor::new(after.clone())).unwrap();
        let data = read_jumbf(&re, &mut Cursor::new(after)).unwrap().unwrap();
        assert_eq!(&data[..140_000], &signed[..]);
        assert!(data[140_000..].iter().all(|&b| b == 0));
        assert!(dest
            .patch(&mut Cursor::new(vec![0u8; 10]), &e, &vec![1u8; 150_001])
            .is_err());
    }

    #[test]
    fn xmp_standard_and_extended_round_trip() {
        let j = sample_jpeg();

        let small = b"<x:xmpmeta/>".to_vec();
        let (out, _) = do_write(&j, &Updates::new().set_xmp(small.clone()));
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        assert_eq!(
            read_xmp(&re, &mut Cursor::new(out)).unwrap().unwrap(),
            small
        );

        let big = vec![b'x'; 70_000];
        let (out, dest) = do_write(&j, &Updates::new().set_xmp(big.clone()));
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        let e = re.require_entity(SegmentKind::Xmp).unwrap();
        assert_eq!(e.part_count(), 3); // stub + two extended chunks
        assert_eq!(
            dest.require_entity(SegmentKind::Xmp).unwrap().part_count(),
            3
        );
        assert_eq!(
            read_xmp(&re, &mut Cursor::new(out.clone()))
                .unwrap()
                .unwrap(),
            big
        );

        // Keeping extended XMP copies it unchanged.
        let (kept, _) = do_write(&out, &Updates::new());
        assert_eq!(kept, out);
    }

    #[test]
    fn extended_xmp_length_cannot_force_allocation() {
        // Declared total far larger than the data present must be refused.
        let j = sample_jpeg();
        let big = vec![b'x'; 70_000];
        let (mut out, _) = do_write(&j, &Updates::new().set_xmp(big));
        // Corrupt the declared total of every extended chunk to 90 MB.
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        let e = re.require_entity(SegmentKind::Xmp).unwrap();
        for p in e.parts().skip(1) {
            let h = re.segment(p.segment()).unwrap().header();
            let at = (h.end() - 8) as usize;
            out[at..at + 4].copy_from_slice(&(90u32 * 1024 * 1024).to_be_bytes());
        }
        let re = parse(&mut Cursor::new(out.clone())).unwrap();
        assert!(read_xmp(&re, &mut Cursor::new(out)).is_err());
    }

    /// Image 2 of an MPF file: a complete small JPEG.
    fn second_image() -> Vec<u8> {
        let mut v = vec![0xFF, SOI];
        v.extend(marker(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        v.extend(marker(0xDB, &[2u8; 65]));
        v.extend(marker(SOS, &[1, 1, 0, 0, 0x3F, 0]));
        v.extend_from_slice(&[0x9A, 0xBC, 0xFF, 0x00, 0xDE]);
        v.extend_from_slice(&[0xFF, EOI]);
        v
    }

    /// An MPF file: the primary image carries the MPF index, then `second`, then
    /// `trailing`. `skew` is added to the second image's recorded offset.
    fn mpf_file(second: &[u8], trailing: &[u8], skew: u32, little: bool) -> Vec<u8> {
        let u16b = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let u32b = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let build = |first_len: u32, second_off: u32| -> Vec<u8> {
            let mut b = Vec::new();
            b.extend_from_slice(if little { b"II" } else { b"MM" });
            b.extend_from_slice(&u16b(0x2A));
            b.extend_from_slice(&u32b(8));
            b.extend_from_slice(&u16b(3));
            for (tag, ty, count, val) in [
                (0xB000u16, 7u16, 4u32, u32::from_be_bytes(*b"0100")),
                (0xB001, 4, 1, 2),
                (0xB002, 7, 32, 50),
            ] {
                b.extend_from_slice(&u16b(tag));
                b.extend_from_slice(&u16b(ty));
                b.extend_from_slice(&u32b(count));
                b.extend_from_slice(&if tag == 0xB000 {
                    val.to_be_bytes()
                } else {
                    u32b(val)
                });
            }
            b.extend_from_slice(&u32b(0));
            assert_eq!(b.len(), 50);
            for (size, off) in [(first_len, 0), (second.len() as u32, second_off)] {
                b.extend_from_slice(&u32b(0x030000));
                b.extend_from_slice(&u32b(size));
                b.extend_from_slice(&u32b(off));
                b.extend_from_slice(&u16b(0));
                b.extend_from_slice(&u16b(0));
            }
            b
        };
        let head = |block: &[u8]| -> Vec<u8> {
            let mut v = vec![0xFF, SOI];
            v.extend(marker(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
            let mut app2 = MPF_SIGNATURE.to_vec();
            app2.extend_from_slice(block);
            v.extend(marker(APP2, &app2));
            v.extend(marker(0xDB, &[0u8; 65]));
            v.extend(marker(SOS, &[1, 1, 0, 0, 0x3F, 0]));
            v.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00, 0x56]);
            v.extend_from_slice(&[0xFF, EOI]);
            v
        };
        let first_len = head(&build(0, 0)).len() as u32;
        let base = 2 + 18 + 4 + 4; // SOI, APP0, APP2 marker/length, "MPF\0"
        let mut f = head(&build(first_len, first_len - base + skew));
        f.extend_from_slice(second);
        f.extend_from_slice(trailing);
        f
    }

    /// The (size, offset) pairs recorded in the file's MPF block.
    fn recorded(file: &[u8]) -> (Vec<(u32, u32)>, u64) {
        let s = parse(&mut Cursor::new(file.to_vec())).unwrap();
        let id = find_mpf(
            &s,
            &mut Cursor::new(file.to_vec()),
            s.subassets()[0].segments().end,
        )
        .unwrap()
        .unwrap();
        let payload = s.segment(id).unwrap().payload();
        let block = s
            .read_range(&mut Cursor::new(file.to_vec()), payload)
            .unwrap();
        let ix = MpIndex::parse(&block).unwrap();
        let pairs = ix
            .entries
            .iter()
            .map(|&at| (ix.u32(&block, at).unwrap(), ix.u32(&block, at + 4).unwrap()))
            .collect();
        (pairs, payload.offset())
    }

    fn assert_index_matches_layout(file: &[u8]) {
        let s = parse(&mut Cursor::new(file.to_vec())).unwrap();
        let (pairs, base) = recorded(file);
        let images: Vec<_> = s
            .subassets()
            .iter()
            .filter(|a| a.media_type().is_some())
            .collect();
        assert_eq!(pairs.len(), images.len());
        for (n, (a, (size, off))) in images.iter().zip(pairs).enumerate() {
            assert_eq!(size as u64, a.range().len(), "size of image {n}");
            let expect = if n == 0 { 0 } else { a.range().offset() - base };
            assert_eq!(off as u64, expect, "offset of image {n}");
        }
    }

    #[test]
    fn mpo_is_served_by_the_jpeg_handler() {
        let f = mpf_file(&second_image(), b"", 0, false);
        let reg = crate::HandlerRegistry::builtin();
        let s = reg.parse("mpo", &mut Cursor::new(f.clone())).unwrap();
        assert_eq!(s.media_type(), "image/jpeg");
        assert_eq!(s.subassets().len(), 2);
        let s = reg.parse_detect(&mut Cursor::new(f)).unwrap();
        assert_eq!(s.subassets().len(), 2);
    }

    #[test]
    fn multi_picture_file_is_listed_as_parts() {
        for little in [false, true] {
            let second = second_image();
            let f = mpf_file(&second, b"", 0, little);
            let s = parse(&mut Cursor::new(f.clone())).unwrap();
            let subs = s.subassets();
            assert_eq!(subs.len(), 2);
            assert!(subs[0].is_primary() && !subs[1].is_primary());
            assert_eq!(subs[1].range().len(), second.len() as u64);
            assert_eq!(subs[1].range().end(), f.len() as u64);
            assert_eq!(subs[1].media_type(), Some("image/jpeg"));
            // Parts tile the file, and the second image's segments are listed in
            // order: SOI, APP0, DQT, SOS, EOI.
            assert_eq!(subs[0].range().end(), subs[1].range().offset());
            assert_eq!(subs[0].segments().end, subs[1].segments().start);
            let names: Vec<_> = s.segments()[subs[1].segments()]
                .iter()
                .map(|g| s.box_name(g.name()).unwrap().into_owned())
                .collect();
            assert_eq!(names, ["SOI", "APP0", "DQT", "SOS", "EOI"]);
            assert_eq!(subs[1].relative(subs[1].range().offset() + 3), Some(3));
            assert_eq!(subs[1].relative(0), None);
            assert_index_matches_layout(&f);
        }
    }

    #[test]
    fn plain_files_have_one_part() {
        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        assert_eq!(s.subassets().len(), 1);
        assert_eq!(s.subassets()[0].range().len(), j.len() as u64);
        assert_eq!(s.subassets()[0].media_type(), Some("image/jpeg"));
        // Trailing data of a single image stays part of it.
        let mut t = j;
        t.extend_from_slice(b"trailing");
        let s = parse(&mut Cursor::new(t)).unwrap();
        assert_eq!(s.subassets().len(), 1);
    }

    #[test]
    fn data_after_the_last_image_is_a_final_part() {
        let f = mpf_file(&second_image(), b"\0\0\0\x18ftypmp42 trailing", 0, false);
        let s = parse(&mut Cursor::new(f.clone())).unwrap();
        let subs = s.subassets();
        assert_eq!(subs.len(), 3);
        assert_eq!(subs[2].media_type(), None);
        assert_eq!(subs[2].range().end(), f.len() as u64);
        assert_eq!(subs[1].range().end(), subs[2].range().offset());
    }

    #[test]
    fn keeping_a_multi_picture_file_is_byte_identical() {
        let f = mpf_file(&second_image(), b"tail", 0, false);
        let (out, dest) = do_write(&f, &Updates::new());
        assert_eq!(out, f);
        assert_eq!(dest.subassets().len(), 3);
    }

    #[test]
    fn editing_the_primary_image_keeps_the_mpf_index_valid() {
        for little in [false, true] {
            let second = second_image();
            let f = mpf_file(&second, b"tail", 0, little);
            let updates = Updates::new()
                .set_jumbf(fake_jumbf(200))
                .set_xmp(b"<x:xmpmeta/>".to_vec());
            let (out, dest) = do_write(&f, &updates);
            assert!(out.len() > f.len());

            let re = parse(&mut Cursor::new(out.clone())).unwrap();
            assert_eq!(re.subassets().len(), 3);
            assert_index_matches_layout(&out);
            // Later images and trailing data are untouched.
            let subs = re.subassets();
            assert_eq!(
                &out[subs[1].range().offset() as usize..subs[1].range().end() as usize],
                &second[..]
            );
            assert_eq!(&out[subs[2].range().offset() as usize..], b"tail");
            // The planned layout is the written one, parts included.
            assert_eq!(dest.subassets(), re.subassets());
            let planned = parse(&mut Cursor::new(f.clone()))
                .unwrap()
                .plan(&mut Cursor::new(f.clone()), &updates)
                .unwrap();
            assert_eq!(planned.subassets(), re.subassets());

            // The edited file is its own fixed point, and removal restores the index.
            let (again, _) = do_write(&out, &Updates::new());
            assert_eq!(again, out);
            let (back, _) = do_write(&out, &Updates::remove_all());
            assert_index_matches_layout(&back);
            assert_eq!(back.len(), f.len());
        }
    }

    #[test]
    fn metadata_goes_in_the_primary_image_only() {
        let f = mpf_file(&second_image(), b"", 0, false);
        let (out, _) = do_write(&f, &Updates::new().set_jumbf(fake_jumbf(64)));
        let re = parse(&mut Cursor::new(out)).unwrap();
        let primary = re.subassets()[0].segments();
        let e = re.require_entity(SegmentKind::Jumbf).unwrap();
        assert!(e.parts().all(|p| primary.contains(&p.segment().index())));
    }

    #[test]
    fn an_index_that_does_not_match_the_images_is_refused_on_edit_only() {
        // Records one image where the file has two.
        let mut f = mpf_file(&second_image(), b"", 0, false);
        let s = parse(&mut Cursor::new(f.clone())).unwrap();
        let id = find_mpf(
            &s,
            &mut Cursor::new(f.clone()),
            s.subassets()[0].segments().end,
        )
        .unwrap()
        .unwrap();
        // The number-of-entries field of tag 0xB002 is at block offset 8 + 2 + 24 + 4.
        let at = s.segment(id).unwrap().payload().offset() as usize + 8 + 2 + 24 + 4;
        f[at..at + 4].copy_from_slice(&16u32.to_be_bytes());
        let (kept, _) = do_write(&f, &Updates::new());
        assert_eq!(kept, f);
        let s = parse(&mut Cursor::new(f.clone())).unwrap();
        let err = s
            .write(
                &mut Cursor::new(f.clone()),
                &mut Cursor::new(Vec::new()),
                &Updates::new().set_jumbf(fake_jumbf(64)),
                &mut NullSink,
            )
            .unwrap_err();
        assert!(matches!(err, Error::InvalidFormat(_)), "{err:?}");
    }

    #[test]
    fn a_hash_over_the_parts_covers_every_byte() {
        let f = mpf_file(&second_image(), b"tail", 0, false);
        let s = parse(&mut Cursor::new(f.clone())).unwrap();
        let mut whole = Sha256::new();
        let mut per_part: Vec<Sha256> = s.subassets().iter().map(|_| Sha256::new()).collect();
        let subs = s.subassets().to_vec();
        s.read(&mut Cursor::new(f.clone()), &mut |c: Chunk<'_>| {
            whole.update(c.data);
            let k = subs
                .iter()
                .position(|a| a.segments().contains(&c.segment.index()))
                .unwrap();
            per_part[k].update(c.data);
            Ok(())
        })
        .unwrap();
        for (a, h) in subs.iter().zip(per_part) {
            let bytes = &f[a.range().offset() as usize..a.range().end() as usize];
            assert_eq!(h.finalize()[..], Sha256::digest(bytes)[..]);
        }
        assert_eq!(whole.finalize()[..], Sha256::digest(&f)[..]);
    }

    fn try_write(f: &[u8], updates: &Updates) -> Result<(Vec<u8>, Structure)> {
        let s = parse(&mut Cursor::new(f.to_vec()))?;
        let mut out = Cursor::new(Vec::new());
        let dest = write(
            &s,
            &mut Cursor::new(f.to_vec()),
            &mut out,
            updates,
            &mut NullSink,
        )?;
        Ok((out.into_inner(), dest))
    }

    /// Where the MPF block (the bytes after `MPF\0`) starts in the file, and a copy.
    fn mpf_block(f: &[u8]) -> (usize, Vec<u8>) {
        let s = parse(&mut Cursor::new(f.to_vec())).unwrap();
        let id = find_mpf(
            &s,
            &mut Cursor::new(f.to_vec()),
            s.subassets()[0].segments().end,
        )
        .unwrap()
        .unwrap();
        let r = s.segment(id).unwrap().payload();
        let (a, b) = (r.offset() as usize, r.end() as usize);
        (a, f[a..b].to_vec())
    }

    #[test]
    fn bytes_that_only_look_like_an_image_are_trailing_data() {
        let junk: [&[u8]; 4] = [
            b"\xFF\xD8\xFF garbage that is not a jpeg",
            b"\xFF\xD8\xFF\xE0\x00\x01",       // marker length below 2
            b"\xFF\xD8\xFF\xE1\xFF\xFF short", // segment runs past the end
            b"\xFF\xD8\xFF\xDB",               // cut off inside a marker
        ];
        for t in junk {
            // After a single image: part of it, as before multi-picture support.
            let mut f = sample_jpeg();
            f.extend_from_slice(t);
            let s = parse(&mut Cursor::new(f.clone())).unwrap();
            assert_eq!(s.subassets().len(), 1);
            assert_eq!(s.trailing().map(|r| r.len()), Some(t.len() as u64));
            let (out, _) = do_write(&f, &Updates::new());
            assert_eq!(out, f);

            // After the last image of a multi-picture file: a part of its own.
            let second = second_image();
            let f = mpf_file(&second, t, 0, false);
            let s = parse(&mut Cursor::new(f.clone())).unwrap();
            let subs = s.subassets();
            assert_eq!(subs.len(), 3);
            assert_eq!(subs[1].range().len(), second.len() as u64);
            assert_eq!(subs[2].media_type(), None);
            assert_eq!(&f[subs[2].range().offset() as usize..], t);
            let (out, _) = do_write(&f, &Updates::new().set_jumbf(fake_jumbf(64)));
            assert_index_matches_layout(&out);
        }
    }

    #[test]
    fn data_beyond_the_first_image_is_reported_as_trailing() {
        use crate::ByteRange;
        let j = sample_jpeg();
        let s = parse(&mut Cursor::new(j.clone())).unwrap();
        assert_eq!(s.trailing(), None);

        let mut t = j.clone();
        t.extend_from_slice(b"trailing");
        let s = parse(&mut Cursor::new(t)).unwrap();
        assert_eq!(
            s.trailing(),
            Some(ByteRange::new(j.len() as u64, 8).unwrap())
        );

        // In a multi-picture file everything after the first image is beyond it.
        for tail in [&b""[..], b"tail"] {
            let f = mpf_file(&second_image(), tail, 0, false);
            let s = parse(&mut Cursor::new(f.clone())).unwrap();
            let first_end = s.subassets()[0].range().end();
            assert_eq!(
                s.trailing(),
                Some(ByteRange::new(first_end, f.len() as u64 - first_end).unwrap())
            );

            // An edit moves it with the file, and what was planned is what is written.
            let (out, dest) = do_write(&f, &Updates::new().set_jumbf(fake_jumbf(200)));
            let re = parse(&mut Cursor::new(out.clone())).unwrap();
            assert_eq!(dest.trailing(), re.trailing());
            let r = re.trailing().unwrap();
            assert_eq!(&out[r.offset() as usize..], &f[first_end as usize..]);

            // Removing the metadata puts it back where it was.
            let (back, _) = do_write(&out, &Updates::remove_all());
            assert_eq!(back.len(), f.len());
        }
    }

    #[test]
    fn a_second_image_cut_short_is_trailing_data() {
        let second = second_image();
        let f = mpf_file(&second[..second.len() - 2], b"", 0, false);
        let s = parse(&mut Cursor::new(f.clone())).unwrap();
        assert_eq!(s.subassets().len(), 1);
        let (out, _) = do_write(&f, &Updates::new());
        assert_eq!(out, f);
    }

    #[test]
    fn an_index_that_disagrees_with_the_file_is_refused_on_edit_only() {
        let edit = Updates::new().set_jumbf(fake_jumbf(64));
        let good = mpf_file(&second_image(), b"", 0, false);
        let (base, _) = mpf_block(&good);
        assert!(try_write(&good, &edit).is_ok());
        // Entries start at block offset 50, 16 bytes each: attributes, size, offset.
        for (what, at) in [
            ("offset of the first image", 58),
            ("size of the second image", 70),
            ("offset of the second image", 74),
        ] {
            let mut f = good.clone();
            f[base + at + 3] ^= 1;
            // Copying unchanged never consults the index.
            let (kept, _) = do_write(&f, &Updates::new());
            assert_eq!(kept, f, "{what}");
            let err = try_write(&f, &edit).unwrap_err();
            assert!(matches!(err, Error::InvalidFormat(_)), "{what}: {err:?}");
        }
    }

    #[test]
    fn a_first_image_size_recorded_short_is_accepted_and_corrected() {
        let edit = Updates::new().set_jumbf(fake_jumbf(64));
        let good = mpf_file(&second_image(), b"", 0, false);
        let (base, block) = mpf_block(&good);
        let first_len = parse(&mut Cursor::new(good.clone())).unwrap().subassets()[0]
            .range()
            .len();
        let set_size = |f: &mut Vec<u8>, v: u32| {
            f[base + 54..base + 58].copy_from_slice(&v.to_be_bytes());
        };
        assert_eq!(
            u32::from_be_bytes(block[54..58].try_into().unwrap()) as u64,
            first_len
        );

        // Recorded a few hundred bytes short, as Pixel Ultra HDR files do.
        for short in [1u32, 307, first_len as u32 - 1] {
            let mut f = good.clone();
            set_size(&mut f, first_len as u32 - short.min(first_len as u32 - 1));
            let (kept, _) = do_write(&f, &Updates::new());
            assert_eq!(kept, f, "copying never consults the index");
            let (out, _) = do_write(&f, &edit);
            assert_index_matches_layout(&out);
        }

        // Claiming more than the image is refused.
        let mut f = good.clone();
        set_size(&mut f, first_len as u32 + 1);
        let err = try_write(&f, &edit).unwrap_err();
        assert!(matches!(err, Error::InvalidFormat(_)), "{err:?}");
        assert!(err.to_string().contains("size of image 0"), "{err}");
    }

    #[test]
    fn a_hostile_mpf_index_is_not_parsed() {
        for little in [false, true] {
            let f = mpf_file(&second_image(), b"", 0, little);
            let (_, block) = mpf_block(&f);
            let ix = MpIndex::parse(&block).expect("a good index parses");
            assert_eq!(ix.entries, [54, 70]);

            let w16 = |b: &mut [u8], at: usize, v: u16| {
                let v = if little {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                };
                b[at..at + 2].copy_from_slice(&v);
            };
            let w32 = |b: &mut [u8], at: usize, v: u32| {
                let v = if little {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                };
                b[at..at + 4].copy_from_slice(&v);
            };
            // Layout: header 0..8, IFD at 8 (count, then entries at 10, 22, 34 with
            // tag/type/count/value at +0/+2/+4/+8), next IFD at 46, entries at 50.
            let end = block.len() as u32;
            let cases: Vec<(&str, Vec<u8>)> = {
                let with = |f: &dyn Fn(&mut Vec<u8>)| {
                    let mut b = block.clone();
                    f(&mut b);
                    b
                };
                vec![
                    ("magic", with(&|b| w16(b, 2, 0x2B))),
                    ("byte order", with(&|b| b[0] = b'X')),
                    ("IFD inside the header", with(&|b| w32(b, 4, 4))),
                    ("IFD past the end", with(&|b| w32(b, 4, 0xFFFF_FFF0))),
                    ("huge entry count", with(&|b| w16(b, 8, 0xFFFF))),
                    ("array inside the header", with(&|b| w32(b, 42, 0))),
                    ("array over the IFD", with(&|b| w32(b, 42, 20))),
                    ("array past the end", with(&|b| w32(b, 42, end - 16))),
                    ("length not a multiple of 16", with(&|b| w32(b, 38, 31))),
                    ("empty array", with(&|b| w32(b, 38, 0))),
                    ("array of the wrong type", with(&|b| w16(b, 36, 4))),
                    ("second MPEntry", with(&|b| w16(b, 22, 0xB002))),
                    ("image count disagrees", with(&|b| w32(b, 30, 3))),
                    ("image count of the wrong type", with(&|b| w16(b, 24, 3))),
                ]
            };
            for (what, b) in cases {
                assert!(MpIndex::parse(&b).is_none(), "{what} (little: {little})");
            }

            // Any prefix of the block is refused without panicking.
            for n in 0..block.len() {
                assert!(MpIndex::parse(&block[..n]).is_none(), "{n} bytes");
            }
        }
    }

    #[test]
    fn two_mpf_blocks_are_ambiguous_only_when_an_edit_needs_the_index() {
        let f = mpf_file(&second_image(), b"", 0, false);
        let s = parse(&mut Cursor::new(f.clone())).unwrap();
        let id = find_mpf(
            &s,
            &mut Cursor::new(f.clone()),
            s.subassets()[0].segments().end,
        )
        .unwrap()
        .unwrap();
        let x = s.segment(id).unwrap().extent();
        let (a, b) = (x.offset() as usize, x.end() as usize);
        let mut dup = f[..b].to_vec();
        dup.extend_from_slice(&f[a..b]);
        dup.extend_from_slice(&f[b..]);

        let (kept, _) = do_write(&dup, &Updates::new());
        assert_eq!(kept, dup);
        let err = try_write(&dup, &Updates::new().set_jumbf(fake_jumbf(64))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Ambiguous {
                    what: "MPF block",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// Every byte of the MPF block altered several ways: parsing and copying never
    /// fail or panic, and an edit that is accepted leaves an index that matches.
    #[test]
    fn a_mutated_mpf_block_is_refused_or_stays_consistent() {
        let edit = Updates::new().set_jumbf(fake_jumbf(64));
        for little in [false, true] {
            let good = mpf_file(&second_image(), b"tail", 0, little);
            let (base, block) = mpf_block(&good);
            let mut accepted = 0;
            for pos in 0..block.len() {
                let o = block[pos];
                for v in [
                    o ^ 0xFF,
                    0,
                    1,
                    0xFF,
                    o.wrapping_add(1),
                    o.wrapping_sub(1),
                    o ^ 0x80,
                ] {
                    let mut f = good.clone();
                    f[base + pos] = v;
                    let (kept, _) = do_write(&f, &Updates::new());
                    assert_eq!(kept, f);
                    if let Ok((out, dest)) = try_write(&f, &edit) {
                        accepted += 1;
                        let re = parse(&mut Cursor::new(out.clone())).unwrap();
                        assert_eq!(re.subassets(), dest.subassets());
                        assert_index_matches_layout(&out);
                    }
                }
            }
            assert!(
                accepted > 0,
                "changes to bytes the edit ignores are accepted"
            );
        }
    }
}
