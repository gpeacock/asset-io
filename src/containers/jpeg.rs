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
//! Not yet ported: EXIF thumbnail location.

use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, ReadSeek, ReadWriteSeek};
use crate::segment::{Name, Role, Segment};
use crate::segment::{SegmentKind, DEFAULT_CHUNK_SIZE};
use crate::sink::{Emitter, Sink};
use crate::structure::{Part, SegmentId, Structure, StructureBuilder};
use crate::ByteRange;
use crate::{MetadataUpdate, Updates};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, OnceLock};

const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
const APP1: u8 = 0xE1;
const APP11: u8 = 0xEB;

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

impl ContainerHandler for JpegHandler {
    fn formats(&self) -> &[&'static str] {
        &["jpg", "jpeg", "image/jpeg"]
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
        _src: &mut dyn ReadSeek,
        updates: &Updates,
    ) -> Result<Structure> {
        plan(src_structure, updates).map(|(_, dest)| dest)
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

fn parse_with<R: Read + Seek>(
    handler: Arc<dyn ContainerHandler>,
    src: &mut R,
) -> Result<Structure> {
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

    let mut offset = 2u64;
    let mut xmp_open: Option<Vec<SegmentId>> = None;
    let mut jumbf_open: Option<Open> = None;
    // End offset of the last segment added to `jumbf_open`. C2PA A.3.1 requires the
    // APP11 segments of one manifest to be sequential and contiguous.
    let mut jumbf_end: u64 = 0;

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
                offset = mstart + 2;
                break;
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

        if marker == APP1 || marker == APP11 {
            let n = data_len.min(PEEK as u64) as usize;
            let mut buf = [0u8; PEEK];
            src.read_exact(&mut buf[..n])?;
            let peek = &buf[..n];

            if marker == APP1 {
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
                if continues && mstart != jumbf_end {
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
                xmp_open = Some(vec![id]);
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
                    jumbf_open = Some(Open {
                        en,
                        ids: vec![id],
                        ty: jtype,
                    });
                } else if let Some(o) = jumbf_open.as_mut() {
                    o.ids.push(id);
                }
                jumbf_end = mstart + seg_total;
            }
            Class::JumbfRaw => {
                if let Some(o) = jumbf_open.take() {
                    b.entity_of_segments_typed(SegmentKind::Jumbf, o.ty, &o.ids)?;
                }
                jumbf_open = Some(Open {
                    en: 0,
                    ids: vec![id],
                    ty: jtype,
                });
                jumbf_end = mstart + seg_total;
            }
        }

        offset = mstart + seg_total;
    }

    if offset < total {
        b.push_span(SegmentKind::Other, Name::None, 0, total - offset, 0)?;
    }
    if let Some(ids) = xmp_open.take() {
        b.entity_of_segments(SegmentKind::Xmp, &ids)?;
    }
    if let Some(o) = jumbf_open.take() {
        b.entity_of_segments_typed(SegmentKind::Jumbf, o.ty, &o.ids)?;
    }
    b.finish()
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
            Op::Copy(id) => {
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
fn plan<'a>(src: &Structure, updates: &'a Updates) -> Result<(Vec<Op<'a>>, Structure)> {
    let n = src.segments().len();
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
                if !inserted && is_insertion_point(seg) {
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
    for d in &dims {
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
    Ok((ops, b.finish()?))
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
        Op::Copy(_) => return Err(Error::InvalidFormat("not a new segment".into())),
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
    let (ops, dest) = plan(src_structure, updates)?;
    sink.begin(&dest)?;

    let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
    {
        let mut em = Emitter::new(out, sink, &dest);
        for (i, op) in ops.iter().enumerate() {
            em.begin_segment(SegmentId::from_index(i)?)?;
            match op {
                Op::Copy(sid) => {
                    let s = src_structure.segment(*sid)?;
                    for (role, range) in s.regions() {
                        em.copy(src, role, range, &mut buf)?;
                    }
                }
                _ => emit_new(&mut em, op)?,
            }
        }
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
}
