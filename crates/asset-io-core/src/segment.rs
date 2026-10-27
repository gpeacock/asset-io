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

//! Segments, the units that tile an asset, and shared size limits.

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

/// The purpose of a segment in the tiling of an asset.
///
/// A kind describes a segment's dominant purpose only. What its bytes belong to
/// is given by the [items](crate::Item) whose extents lie in it: a HEIF `mdat`
/// holds media and items, and mixed segments (`moov`, `meta`) are
/// [`Other`](Self::Other).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SegmentKind {
    /// Leading bytes that identify the format (JPEG `SOI`, the first eight bytes
    /// of a PNG, a BMFF `ftyp` box).
    Preamble,
    /// A segment whose payload is predominantly metadata (XMP, JUMBF, EXIF, ...).
    Metadata,
    /// Coded media of any type.
    MediaData,
    /// Anything else.
    Other,
}

impl SegmentKind {
    /// `"preamble"`, `"metadata"`, `"media_data"` or `"other"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preamble => "preamble",
            Self::Metadata => "metadata",
            Self::MediaData => "media_data",
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
    /// Unnamed (fill, trailing data, ...).
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
    /// True for unnamed elements (fill, trailing data).
    pub const fn is_none(&self) -> bool {
        matches!(self, Name::None)
    }
}

/// One unit of the tiling of an asset: the unit a writer copies, replaces or moves.
///
/// A segment's `range` is split into `header | payload | trailer`: framing, then
/// content, then framing. Segments carry geometry only; they own no data and no
/// heap memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    range: ByteRange,
    header_len: u32,
    trailer_len: u16,
    kind: SegmentKind,
    name: Name,
    subdivides: bool,
}

impl Segment {
    /// Create a segment, validating that header and trailer fit in the range.
    pub fn new(
        range: ByteRange,
        header_len: u32,
        trailer_len: u16,
        kind: SegmentKind,
        name: Name,
    ) -> Result<Self> {
        if range.is_empty() {
            return Err(Error::invalid_segment(range.offset(), "empty segment"));
        }
        let overhead = header_len as u64 + trailer_len as u64;
        if overhead > range.len() {
            return Err(Error::invalid_segment(
                range.offset(),
                format!(
                    "header ({header_len}) + trailer ({trailer_len}) exceed range ({})",
                    range.len()
                ),
            ));
        }
        Ok(Self {
            range,
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

    /// Whole segment, header through trailer.
    pub const fn range(&self) -> ByteRange {
        self.range
    }

    /// Purpose in the tiling.
    pub const fn kind(&self) -> SegmentKind {
        self.kind
    }

    /// Container-defined identity.
    pub const fn name(&self) -> Name {
        self.name
    }

    /// Header bytes (possibly empty).
    pub const fn header(&self) -> ByteRange {
        ByteRange::raw(self.range.offset(), self.header_len as u64)
    }

    /// Payload bytes (possibly empty).
    pub const fn payload(&self) -> ByteRange {
        ByteRange::raw(
            self.range.offset() + self.header_len as u64,
            self.range.len() - self.header_len as u64 - self.trailer_len as u64,
        )
    }

    /// Trailer bytes (possibly empty).
    pub const fn trailer(&self) -> ByteRange {
        ByteRange::raw(
            self.range.end() - self.trailer_len as u64,
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
    fn splits_range() {
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
