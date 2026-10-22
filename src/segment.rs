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

//! Physical container elements, their logical classification, and shared size limits.

use crate::error::{Error, Result};
use crate::range::ByteRange;

/// Item labels that mean the same in every container.
///
/// A container that stores a JUMBF store in more than one place labels the
/// items so a caller can tell them apart (see [`Item::label`](crate::Item::label)).
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

/// The name a box-based hash gives the bytes beyond the end of a container's own
/// structure, listed as its last box (C2PA 18.7.2). See
/// [`Structure::trailing`](crate::Structure::trailing).
pub const AFTER_BOX: &str = "c2pa.after";

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
#[non_exhaustive]
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
#[non_exhaustive]
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
            return Err(Error::invalid_segment(extent.offset(), "empty segment"));
        }
        let overhead = header_len as u64 + trailer_len as u64;
        if overhead > extent.len() {
            return Err(Error::invalid_segment(
                extent.offset(),
                format!(
                    "header ({header_len}) + trailer ({trailer_len}) exceed extent ({})",
                    extent.len()
                ),
            ));
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
    fn none_name() {
        assert!(Name::None.is_none());
        assert!(!Name::Marker(0xD8).is_none());
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
