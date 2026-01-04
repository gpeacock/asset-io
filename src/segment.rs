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
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for SegmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// How to handle exclusion when processing segments during write operations
///
/// This controls what portion of a segment is excluded from processing
/// (e.g., hashing) during [`write_with_processor`](crate::Asset::write_with_processing).
///
/// # C2PA Compliance
///
/// For C2PA manifest embedding, use [`DataOnly`](ExclusionMode::DataOnly) mode.
/// The C2PA specification requires that container headers (markers, length fields,
/// format-specific headers) are included in the hash to prevent insertion attacks.
/// Only the manifest data itself should be excluded.
///
/// # Example
///
/// ```rust
/// use asset_io::{ExclusionMode, SegmentKind};
///
/// // For C2PA: exclude only the JUMBF data, include headers in hash
/// let mode = ExclusionMode::DataOnly;
/// let exclude = &[SegmentKind::Jumbf];
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExclusionMode {
    /// Exclude the entire segment including container-specific headers
    ///
    /// For JPEG APP11: Excludes marker (FF EB) + length + JPEG XT headers + data
    /// For PNG caBX: Excludes length + type + data + CRC
    ///
    /// This is simpler but NOT recommended for C2PA as it allows insertion attacks.
    #[default]
    EntireSegment,

    /// Exclude only the data portion, include container headers in processing
    ///
    /// For JPEG APP11: Include marker + length + JPEG XT headers; exclude only JUMBF data
    /// For PNG caBX: Include length + type; exclude only data + CRC
    ///
    /// This is required for C2PA compliance per the specification, which states:
    /// "This is accomplished by including all the C2PA manifest segment headers
    /// (APP11) and 2-byte length fields in the data-hash-map for all
    /// manifest-containing segments."
    DataOnly,
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

    /// Embedded thumbnail location info (from EXIF or other metadata)
    #[cfg(feature = "exif")]
    Thumbnail(crate::thumbnail::EmbeddedThumbnailInfo),
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

    /// Get embedded thumbnail location info if this is that variant
    #[cfg(feature = "exif")]
    pub(crate) fn as_thumbnail_info(&self) -> Option<&crate::thumbnail::EmbeddedThumbnailInfo> {
        match self {
            Self::Thumbnail(info) => Some(info),
            _ => None,
        }
    }
}

/// Lazy-loaded data - only reads when accessed
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
    pub fn new(offset: u64, size: u64, kind: SegmentKind, path: Option<String>) -> Self {
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
    /// Returns an error if ranges is empty.
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
    /// )?;
    /// # Ok::<(), asset_io::Error>(())
    /// ```
    pub fn with_ranges(ranges: Vec<ByteRange>, kind: SegmentKind, path: Option<String>) -> crate::Result<Self> {
        if ranges.is_empty() {
            return Err(crate::Error::InvalidFormat("Segment must have at least one range".into()));
        }
        Ok(Self {
            ranges,
            kind,
            path,
            data: LazyData::NotLoaded,
            metadata: None,
        })
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
        let first = self
            .ranges
            .first()
            .expect("Segment must have at least one range");
        let last = self
            .ranges
            .last()
            .expect("Segment must have at least one range");
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

        self.ranges
            .windows(2)
            .all(|w| w[0].is_contiguous_with(&w[1]))
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

    /// Get embedded thumbnail location info if this segment has one
    #[cfg(feature = "exif")]
    pub(crate) fn thumbnail_info(&self) -> Option<&crate::thumbnail::EmbeddedThumbnailInfo> {
        self.metadata.as_ref().and_then(|m| m.as_thumbnail_info())
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

/// A chunk of data with position information for parallel processing
///
/// This is used by [`Asset::read_chunks()`](crate::Asset::read_chunks) to enable
/// parallel hashing of large files using libraries like `rayon`.
///
/// # Example with rayon
///
/// ```ignore
/// use rayon::prelude::*;
/// use sha2::{Sha256, Digest};
///
/// let chunks: Vec<_> = asset.read_chunks(&updates)?.collect::<Result<Vec<_>>>()?;
///
/// let chunk_hashes: Vec<[u8; 32]> = chunks
///     .par_iter()
///     .filter(|c| !c.excluded)
///     .map(|c| {
///         let mut hasher = Sha256::new();
///         hasher.update(&c.data);
///         hasher.finalize().into()
///     })
///     .collect();
/// ```
#[derive(Debug, Clone)]
pub struct ProcessingChunk {
    /// Index of this chunk (for ordering results)
    pub index: usize,
    /// Byte offset in the file
    pub offset: u64,
    /// The chunk data
    pub data: Vec<u8>,
    /// Whether this chunk overlaps with an exclusion range
    pub excluded: bool,
}

impl ProcessingChunk {
    /// Create a new processing chunk
    pub fn new(index: usize, offset: u64, data: Vec<u8>, excluded: bool) -> Self {
        Self {
            index,
            offset,
            data,
            excluded,
        }
    }

    /// Get the size of this chunk
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Get the byte range covered by this chunk
    pub fn range(&self) -> ByteRange {
        ByteRange::new(self.offset, self.data.len() as u64)
    }
}

/// Specification for a chunk to be processed - metadata only, no data
///
/// This is a lightweight alternative to [`ProcessingChunk`] that describes
/// what to read without actually reading it. This enables parallel I/O
/// where each worker can open its own file handle and read independently.
///
/// # Example
///
/// ```ignore
/// use asset_io::{Asset, Updates, SegmentKind, ExclusionMode};
/// use rayon::prelude::*;
/// use sha2::{Sha256, Digest};
/// use std::fs::File;
/// use std::io::{Read, Seek, SeekFrom};
///
/// let asset = Asset::open("large_video.mov")?;
/// let updates = Updates::new()
///     .with_chunk_size(1024 * 1024)
///     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
///
/// let specs = asset.chunk_specs(&updates);
/// let path = "large_video.mov".to_string();
///
/// // Each thread opens its own file handle - true parallel I/O!
/// let hashes: Vec<[u8; 32]> = specs
///     .into_par_iter()
///     .filter(|s| !s.excluded)
///     .map(|spec| {
///         let mut file = File::open(&path).unwrap();
///         file.seek(SeekFrom::Start(spec.offset)).unwrap();
///         let mut buffer = vec![0u8; spec.size];
///         file.read_exact(&mut buffer).unwrap();
///
///         let mut hasher = Sha256::new();
///         hasher.update(&buffer);
///         hasher.finalize().into()
///     })
///     .collect();
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ChunkSpec {
    /// Index of this chunk (for ordering results)
    pub index: usize,
    /// Byte offset in the file
    pub offset: u64,
    /// Size in bytes
    pub size: usize,
    /// Whether this chunk overlaps with an exclusion range
    pub excluded: bool,
}

impl ChunkSpec {
    /// Create a new chunk specification
    pub fn new(index: usize, offset: u64, size: usize, excluded: bool) -> Self {
        Self {
            index,
            offset,
            size,
            excluded,
        }
    }

    /// Get the byte range covered by this chunk
    pub fn range(&self) -> ByteRange {
        ByteRange::new(self.offset, self.size as u64)
    }
}

/// Build a Merkle tree root hash from a list of leaf hashes
///
/// This is useful for C2PA BMFF v3 hash assertions which use a Merkle tree
/// structure to enable parallel hash verification.
///
/// # Arguments
///
/// * `leaves` - The leaf hashes (individual chunk hashes)
///
/// # Returns
///
/// The root hash of the Merkle tree
///
/// # Example
///
/// ```ignore
/// use sha2::{Sha256, Digest};
/// use asset_io::merkle_root;
///
/// // Hash chunks in parallel
/// let chunk_hashes: Vec<[u8; 32]> = chunks
///     .par_iter()
///     .map(|c| sha256(&c.data))
///     .collect();
///
/// // Compute Merkle root
/// let root = merkle_root::<Sha256>(&chunk_hashes);
/// ```
#[cfg(feature = "parallel")]
pub fn merkle_root<H>(leaves: &[[u8; 32]]) -> [u8; 32]
where
    H: sha2::Digest + Default,
{
    if leaves.is_empty() {
        return [0u8; 32];
    }
    if leaves.len() == 1 {
        return leaves[0];
    }

    let mut current_level: Vec<[u8; 32]> = leaves.to_vec();

    while current_level.len() > 1 {
        let mut next_level = Vec::with_capacity((current_level.len() + 1) / 2);

        for pair in current_level.chunks(2) {
            let mut hasher = H::new();
            hasher.update(&pair[0]);
            if pair.len() > 1 {
                hasher.update(&pair[1]);
            } else {
                // Odd number: hash with itself
                hasher.update(&pair[0]);
            }
            let result = hasher.finalize();
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&result[..32]);
            next_level.push(hash);
        }

        current_level = next_level;
    }

    current_level[0]
}
