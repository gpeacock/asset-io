//! Segment types and location tracking

use crate::error::Result;
use std::io::Read;

/// A byte range in a file (offset and size)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Offset from start of file
    pub offset: u64,
    /// Size in bytes
    pub size: u64,
}

impl ByteRange {
    /// Create a new byte range
    pub fn new(offset: u64, size: u64) -> Self {
        Self { offset, size }
    }

    /// Get the end offset of this range
    pub fn end_offset(&self) -> u64 {
        self.offset + self.size
    }

    /// Check if this range is immediately followed by another (contiguous)
    pub fn is_contiguous_with(&self, other: &ByteRange) -> bool {
        self.end_offset() == other.offset
    }
}

/// Alias for ByteRange for backward compatibility
pub type Location = ByteRange;

/// Chunk size for streaming large segments (64KB)
pub const DEFAULT_CHUNK_SIZE: usize = 65536;

/// Maximum size for a single segment to prevent DOS attacks (256 MB)
///
/// This prevents malicious files from requesting multi-GB allocations.
/// Legitimate segments are typically much smaller:
/// - XMP: Usually < 1 MB
/// - JUMBF: Usually < 10 MB  
/// - Image data: Handled via streaming, not single allocation
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
    /// Embedded thumbnail
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

/// Format-specific metadata for segments
///
/// This allows storing format-specific information needed for proper
/// reassembly or interpretation of multi-part segments.
#[derive(Debug, Clone)]
pub enum SegmentMetadata {
    /// JPEG Extended XMP reassembly information
    ///
    /// JPEG Extended XMP uses a special format where chunks have explicit offsets
    /// and need to be reassembled in a specific order (not just concatenated).
    JpegExtendedXmp {
        /// GUID identifying this XMP set (all parts share same GUID)
        guid: String,
        /// Offset where each chunk belongs in the reassembled XMP
        /// Indexed by segment index in the segments Vec
        chunk_offsets: Vec<u32>,
        /// Total size of the complete extended XMP when reassembled
        total_size: u32,
    },

    /// Embedded thumbnail (from EXIF or other metadata)
    #[cfg(feature = "exif")]
    Thumbnail(crate::thumbnail::EmbeddedThumbnail),
}

impl SegmentMetadata {
    /// Get JPEG Extended XMP metadata if this is that variant
    pub fn as_jpeg_extended_xmp(&self) -> Option<(&str, &[u32], u32)> {
        match self {
            Self::JpegExtendedXmp {
                guid,
                chunk_offsets,
                total_size,
            } => Some((guid.as_str(), chunk_offsets.as_slice(), *total_size)),
            #[cfg(feature = "exif")]
            _ => None,
        }
    }

    /// Get embedded thumbnail if this is that variant
    #[cfg(feature = "exif")]
    pub fn as_thumbnail(&self) -> Option<&crate::thumbnail::EmbeddedThumbnail> {
        match self {
            Self::Thumbnail(thumb) => Some(thumb),
            _ => None,
        }
    }
}

/// Lazy-loaded data - only reads when accessed
#[derive(Debug)]
pub enum LazyData {
    /// Data not yet loaded
    NotLoaded,
    /// Data loaded into memory
    Loaded(Vec<u8>),
    /// Memory-mapped data (zero-copy)
    #[cfg(feature = "memory-mapped")]
    MemoryMapped {
        /// Reference to the memory map
        mmap: std::sync::Arc<memmap2::Mmap>,
        /// Offset into the mmap
        offset: usize,
        /// Size of this segment
        size: usize,
    },
}

impl LazyData {
    /// Create from a memory-mapped slice
    #[cfg(feature = "memory-mapped")]
    pub fn from_mmap(mmap: std::sync::Arc<memmap2::Mmap>, offset: usize, size: usize) -> Self {
        Self::MemoryMapped { mmap, offset, size }
    }

    /// Load data from source at given location
    pub fn load<R: Read>(&mut self, source: &mut R, location: ByteRange) -> Result<&[u8]> {
        match self {
            Self::NotLoaded => {
                // Validate segment size to prevent DOS attacks
                if location.size > MAX_SEGMENT_SIZE {
                    return Err(crate::Error::InvalidSegment {
                        offset: location.offset,
                        reason: format!(
                            "Segment too large: {} bytes (max {} MB)",
                            location.size,
                            MAX_SEGMENT_SIZE / (1024 * 1024)
                        ),
                    });
                }

                let mut buffer = vec![0u8; location.size as usize];
                source.read_exact(&mut buffer)?;
                *self = Self::Loaded(buffer);
                match self {
                    Self::Loaded(data) => Ok(data),
                    _ => unreachable!(),
                }
            }
            Self::Loaded(data) => Ok(data),
            #[cfg(feature = "memory-mapped")]
            Self::MemoryMapped { mmap, offset, size } => {
                // Validate bounds for memory-mapped access
                let end =
                    offset
                        .checked_add(*size)
                        .ok_or_else(|| crate::Error::InvalidSegment {
                            offset: 0,
                            reason: "Memory-mapped region overflow".into(),
                        })?;

                if end > mmap.len() {
                    return Err(crate::Error::InvalidSegment {
                        offset: *offset as u64,
                        reason: format!(
                            "Memory-mapped region out of bounds: {}..{} (file size: {})",
                            offset,
                            end,
                            mmap.len()
                        ),
                    });
                }

                // Return slice from mmap (zero-copy!)
                Ok(&mmap[*offset..end])
            }
        }
    }

    /// Get data if already loaded (or memory-mapped)
    pub fn get(&self) -> Option<&[u8]> {
        match self {
            Self::Loaded(data) => Some(data),
            #[cfg(feature = "memory-mapped")]
            Self::MemoryMapped { mmap, offset, size } => Some(&mmap[*offset..*offset + *size]),
            _ => None,
        }
    }
}

/// A logical segment of a file
///
/// This represents the SDK's interpretation/abstraction of file data,
/// which may span multiple physical locations or structures in the file.
///
/// For example, JPEG Extended XMP appears as multiple APP1 markers in the
/// physical file, but is exposed as a single logical XMP segment.
///
/// # Examples
///
/// ```
/// use asset_io::{Segment, SegmentKind, ByteRange};
///
/// // Single range segment (most common)
/// let header = Segment::new(0, 100, SegmentKind::Header, None);
///
/// // Segment with format-specific path
/// let xmp = Segment::new(1000, 500, SegmentKind::Xmp, Some("app1".to_string()));
///
/// // Multi-range segment (e.g., JPEG Extended XMP)
/// let xmp_ext = Segment::with_ranges(
///     vec![
///         ByteRange::new(1000, 500),
///         ByteRange::new(2000, 500),
///         ByteRange::new(3000, 500),
///     ],
///     SegmentKind::Xmp,
///     Some("app1/extended".to_string())
/// );
/// ```
#[derive(Debug)]
pub struct Segment {
    /// One or more byte ranges in the physical file
    ///
    /// Most segments have a single range, but some (like JPEG Extended XMP
    /// or multi-part JUMBF) span multiple non-contiguous ranges.
    pub ranges: Vec<ByteRange>,

    /// Logical classification assigned by the SDK
    ///
    /// This represents WHAT the segment is from a cross-format perspective.
    pub kind: SegmentKind,

    /// Physical path in the format's structure (optional)
    ///
    /// This is a breadcrumb back to WHERE the segment is in the file's
    /// physical structure. Format-specific and optional.
    ///
    /// Examples:
    /// - JPEG: "app1", "app11", "sos"
    /// - PNG: "ihdr", "iTXt[xmp]", "caBX"
    /// - MP4/BMFF: "ftyp", "moov/trak[0]/mdia", "moov/uuid/c2pa"
    /// - TIFF: "ifd0", "ifd1/strips[0]"
    pub path: Option<String>,

    /// Lazy-loaded data for this segment
    ///
    /// Data is only loaded when accessed. For multi-range segments, this contains
    /// the assembled/merged data.
    pub data: LazyData,

    /// Optional format-specific metadata
    ///
    /// Used for things like JPEG Extended XMP reassembly info, embedded thumbnails, etc.
    pub metadata: Option<SegmentMetadata>,
}

impl Segment {
    /// Create a new segment with a single range
    ///
    /// # Example
    ///
    /// ```
    /// use asset_io::{Segment, SegmentKind};
    ///
    /// let header = Segment::new(0, 100, SegmentKind::Header, None);
    /// let xmp = Segment::new(1000, 500, SegmentKind::Xmp, Some("app1".to_string()));
    /// ```
    pub fn new(
        offset: u64,
        size: u64,
        kind: SegmentKind,
        path: Option<String>,
    ) -> Self {
        Self {
            ranges: vec![ByteRange::new(offset, size)],
            kind,
            path,
            data: LazyData::NotLoaded,
            metadata: None,
        }
    }

    /// Create a new segment with multiple ranges
    ///
    /// # Example
    ///
    /// ```
    /// use asset_io::{Segment, SegmentKind, ByteRange};
    ///
    /// let striped_image = Segment::with_ranges(
    ///     vec![
    ///         ByteRange::new(1000, 4096),
    ///         ByteRange::new(5000, 4096),
    ///         ByteRange::new(9000, 4096),
    ///     ],
    ///     SegmentKind::ImageData,
    ///     Some("ifd0/strips".to_string())
    /// );
    /// ```
    pub fn with_ranges(
        ranges: Vec<ByteRange>,
        kind: SegmentKind,
        path: Option<String>,
    ) -> Self {
        Self {
            ranges,
            kind,
            path,
            data: LazyData::NotLoaded,
            metadata: None,
        }
    }

    /// Add data to this segment
    pub fn with_data(mut self, data: LazyData) -> Self {
        self.data = data;
        self
    }

    /// Add metadata to this segment
    pub fn with_metadata(mut self, metadata: SegmentMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Get the primary location (first range) of this segment
    ///
    /// For single-range segments (most common), this is the full location.
    /// For multi-range segments, this is the location of the first range.
    pub fn primary_location(&self) -> ByteRange {
        self.ranges[0]
    }

    /// Get the location of this segment (alias for primary_location for backward compatibility)
    pub fn location(&self) -> ByteRange {
        self.primary_location()
    }

    /// Get the total size across all ranges
    pub fn total_size(&self) -> u64 {
        self.ranges.iter().map(|r| r.size).sum()
    }

    /// Get the span (from start of first range to end of last range)
    ///
    /// Note: For non-contiguous multi-range segments, this includes gaps!
    pub fn span(&self) -> ByteRange {
        let first = self.ranges.first().expect("Segment must have at least one range");
        let last = self.ranges.last().expect("Segment must have at least one range");
        ByteRange {
            offset: first.offset,
            size: last.end_offset() - first.offset,
        }
    }

    /// Check if all ranges are contiguous
    pub fn is_contiguous(&self) -> bool {
        if self.ranges.len() <= 1 {
            return true;
        }

        self.ranges.windows(2).all(|w| w[0].is_contiguous_with(&w[1]))
    }

    // Convenience methods for checking segment kind

    /// Check if this segment has a specific kind
    pub fn is_type(&self, kind: SegmentKind) -> bool {
        self.kind == kind
    }

    /// Check if this is an XMP segment
    pub fn is_xmp(&self) -> bool {
        self.kind == SegmentKind::Xmp
    }

    /// Check if this is a JUMBF segment
    pub fn is_jumbf(&self) -> bool {
        self.kind == SegmentKind::Jumbf
    }

    /// Check if this is image data
    pub fn is_image_data(&self) -> bool {
        self.kind == SegmentKind::ImageData
    }

    /// Check if this is EXIF metadata
    pub fn is_exif(&self) -> bool {
        self.kind == SegmentKind::Exif
    }

    /// Check if this is a header
    pub fn is_header(&self) -> bool {
        self.kind == SegmentKind::Header
    }

    /// Get embedded thumbnail if this segment has one
    #[cfg(feature = "exif")]
    pub fn thumbnail(&self) -> Option<&crate::thumbnail::EmbeddedThumbnail> {
        self.metadata.as_ref().and_then(|m| m.as_thumbnail())
    }
}

/// Iterator over chunks of segment data for streaming
///
/// This allows hashing large segments without loading them entirely into memory.
pub struct ChunkedSegmentReader<R: Read> {
    source: R,
    remaining: u64,
    chunk_size: usize,
}

impl<R: Read> ChunkedSegmentReader<R> {
    /// Create a new chunked reader for a segment
    pub fn new(source: R, size: u64, chunk_size: usize) -> Self {
        Self {
            source,
            remaining: size,
            chunk_size,
        }
    }

    /// Read the next chunk
    pub fn read_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if self.remaining == 0 {
            return Ok(None);
        }

        let to_read = (self.remaining as usize).min(self.chunk_size);
        let mut buffer = vec![0u8; to_read];
        self.source.read_exact(&mut buffer)?;
        self.remaining -= to_read as u64;

        Ok(Some(buffer))
    }

    /// Get remaining bytes
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

impl<R: Read> Iterator for ChunkedSegmentReader<R> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.read_chunk().transpose()
    }
}
