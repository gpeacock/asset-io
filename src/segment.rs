//! Physical container elements, their logical classification, and shared size limits.

use crate::error::{Error, Result};
use crate::range::ByteRange;
use std::borrow::Cow;

/// Entity labels that mean the same in every container.
///
/// A container that stores a JUMBF store in more than one place labels the
/// entities so a caller can tell them apart (see [`Entity::label`](crate::Entity::label)).
/// ISO BMFF does: a file has a `MANIFEST`, or an `ORIGINAL` store plus an `UPDATE`
/// that extends it. Merkle proof boxes are labelled `MERKLE`.
pub mod labels {
    /// The C2PA manifest store.
    pub const MANIFEST: &str = "manifest";
    /// The first store of a file that also has an `UPDATE`.
    pub const ORIGINAL: &str = "original";
    /// Children added to the `ORIGINAL` store after it was frozen.
    pub const UPDATE: &str = "update";
    /// A Merkle tree box.
    pub const MERKLE: &str = "merkle";
}

/// Chunk size for streaming large segments (64KB)
pub const DEFAULT_CHUNK_SIZE: usize = 65536;

/// Maximum size for a single in-memory read, to stop hostile files from
/// requesting multi-GB allocations (256 MB).
///
/// Legitimate metadata is much smaller: XMP is usually under 1 MB and JUMBF under
/// 10 MB. Image data is streamed, never read in one piece.
pub const MAX_SEGMENT_SIZE: u64 = 256 * 1024 * 1024;

/// Logical classification of a segment (SDK-assigned)
///
/// This represents what the segment IS from the SDK's perspective,
/// independent of how it's physically stored in any particular format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// File header/signature
    Header,
    /// XMP metadata
    Xmp,
    /// JUMBF/C2PA data
    Jumbf,
    /// Compressed image data
    ImageData,
    /// EXIF metadata
    Exif,
    /// A pre-rendered preview image stored as its own element (a HEIF `thmb` item).
    /// A thumbnail inside EXIF data is not this kind; see
    /// [`Structure::embedded_thumbnail`](crate::Structure::embedded_thumbnail).
    Thumbnail,
    /// Other/unknown segment type
    Other,
}

impl SegmentKind {
    /// Get a string representation of this kind
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Xmp => "xmp",
            Self::Jumbf => "jumbf",
            Self::ImageData => "image_data",
            Self::Exif => "exif",
            Self::Thumbnail => "thumbnail",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for SegmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Which part of a segment a run of bytes belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Container framing before the payload (marker, length, type, box header, ...).
    Header,
    /// The element's content.
    Payload,
    /// Container framing after the payload (CRC, alignment padding, ...).
    Trailer,
}

/// Compact, allocation-free identity of a container element.
///
/// What the name means is container-defined: a JPEG marker byte, or the four
/// character code of a PNG chunk, RIFF chunk or BMFF box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Name {
    /// Unnamed (signature, trailing data, ...).
    None,
    /// JPEG marker code (the byte after `0xFF`).
    Marker(u8),
    /// Four character code.
    FourCC([u8; 4]),
    /// Container-defined numeric identity (a stream serial number, an index).
    /// The handler decides how it is written as a box name.
    Id(u64),
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Name::None => Ok(()),
            Name::Marker(m) => write!(f, "0xFF{m:02X}"),
            Name::Id(n) => write!(f, "#{n}"),
            Name::FourCC(cc) => {
                for &b in cc {
                    let c = if b.is_ascii_graphic() { b as char } else { '?' };
                    write!(f, "{c}")?;
                }
                Ok(())
            }
        }
    }
}

impl Name {
    /// True for unnamed elements (signature bytes, fill, trailing data).
    pub const fn is_none(&self) -> bool {
        matches!(self, Name::None)
    }

    /// The element's name as a box-based hash lists it, or `None` if it has no
    /// name.
    ///
    /// JPEG markers use the symbols of ISO/IEC 10918-1 Table B.1 (`SOI`, `APP11`,
    /// `SOS`, `RST3`, ...). Four character codes are returned as text. This is the
    /// only place that mapping lives, so callers need no per-container tables.
    pub fn box_name(&self) -> Option<Cow<'static, str>> {
        match *self {
            Name::None => None,
            Name::Marker(m) => Some(jpeg_marker_symbol(m)),
            Name::FourCC(cc) => Some(Cow::Owned(String::from_utf8_lossy(&cc).into_owned())),
            Name::Id(n) => Some(Cow::Owned(n.to_string())),
        }
    }
}

/// ISO/IEC 10918-1 Table B.1 symbol for a marker code.
fn jpeg_marker_symbol(m: u8) -> Cow<'static, str> {
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

/// One physical element of a container.
///
/// A segment's `extent` is split into `header | payload | trailer`. Segments
/// carry geometry only; they own no data and no heap memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    extent: ByteRange,
    header_len: u32,
    trailer_len: u16,
    kind: SegmentKind,
    name: Name,
    subdivides: bool,
}

impl Segment {
    /// Create a segment, validating that header and trailer fit in the extent.
    pub fn new(
        extent: ByteRange,
        header_len: u32,
        trailer_len: u16,
        kind: SegmentKind,
        name: Name,
    ) -> Result<Self> {
        if extent.is_empty() {
            return Err(Error::InvalidSegment {
                offset: extent.offset(),
                reason: "empty segment".into(),
            });
        }
        let overhead = header_len as u64 + trailer_len as u64;
        if overhead > extent.len() {
            return Err(Error::InvalidSegment {
                offset: extent.offset(),
                reason: format!(
                    "header ({header_len}) + trailer ({trailer_len}) exceed extent ({})",
                    extent.len()
                ),
            });
        }
        Ok(Self {
            extent,
            header_len,
            trailer_len,
            kind,
            name,
            subdivides: false,
        })
    }

    /// True if the payload holds finer elements that can only be found by reading
    /// it (for example restart intervals inside a JPEG scan). Callers do not act
    /// on this directly: [`Structure::elements`](crate::Structure::elements) expands
    /// such segments transparently.
    pub const fn subdivides(&self) -> bool {
        self.subdivides
    }

    /// Mark this segment as subdividing. Used by parsers.
    pub(crate) const fn with_subdivision(mut self) -> Self {
        self.subdivides = true;
        self
    }

    /// Whole element, header through trailer.
    pub const fn extent(&self) -> ByteRange {
        self.extent
    }

    /// Logical classification.
    pub const fn kind(&self) -> SegmentKind {
        self.kind
    }

    /// Container-defined identity.
    pub const fn name(&self) -> Name {
        self.name
    }

    /// Header bytes (possibly empty).
    pub const fn header(&self) -> ByteRange {
        ByteRange::raw(self.extent.offset(), self.header_len as u64)
    }

    /// Payload bytes (possibly empty).
    pub const fn payload(&self) -> ByteRange {
        ByteRange::raw(
            self.extent.offset() + self.header_len as u64,
            self.extent.len() - self.header_len as u64 - self.trailer_len as u64,
        )
    }

    /// Trailer bytes (possibly empty).
    pub const fn trailer(&self) -> ByteRange {
        ByteRange::raw(
            self.extent.end() - self.trailer_len as u64,
            self.trailer_len as u64,
        )
    }

    /// The non-empty `header`, `payload`, `trailer` runs in file order.
    pub fn regions(&self) -> impl Iterator<Item = (Role, ByteRange)> {
        [
            (Role::Header, self.header()),
            (Role::Payload, self.payload()),
            (Role::Trailer, self.trailer()),
        ]
        .into_iter()
        .filter(|(_, r)| !r.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_names() {
        let n = |m| Name::Marker(m).box_name().unwrap().into_owned();
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
        assert_eq!(Name::FourCC(*b"IHDR").box_name().unwrap(), "IHDR");
        assert_eq!(Name::FourCC(*b"PNGh").box_name().unwrap(), "PNGh");
        assert!(Name::None.box_name().is_none());
        assert!(Name::None.is_none());
    }

    #[test]
    fn splits_extent() {
        let s = Segment::new(
            ByteRange::new(100, 30).unwrap(),
            8,
            4,
            SegmentKind::Other,
            Name::FourCC(*b"tEXt"),
        )
        .unwrap();
        assert_eq!(s.header(), ByteRange::new(100, 8).unwrap());
        assert_eq!(s.payload(), ByteRange::new(108, 18).unwrap());
        assert_eq!(s.trailer(), ByteRange::new(126, 4).unwrap());
        let total: u64 = s.regions().map(|(_, r)| r.len()).sum();
        assert_eq!(total, 30);
    }

    #[test]
    fn rejects_bad_geometry() {
        let r = ByteRange::new(0, 10).unwrap();
        assert!(Segment::new(r, 8, 4, SegmentKind::Other, Name::None).is_err());
        let empty = ByteRange::new(0, 0).unwrap();
        assert!(Segment::new(empty, 0, 0, SegmentKind::Other, Name::None).is_err());
    }

    #[test]
    fn empty_payload_has_no_payload_region() {
        let s = Segment::new(
            ByteRange::new(0, 12).unwrap(),
            8,
            4,
            SegmentKind::Other,
            Name::FourCC(*b"IEND"),
        )
        .unwrap();
        assert!(s.payload().is_empty());
        assert_eq!(s.regions().count(), 2);
    }
}
