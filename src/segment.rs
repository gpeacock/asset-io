//! Segment types and location tracking

use crate::error::Result;
use std::io::Read;

/// Location of data in a file
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// Offset from start of file
    pub offset: u64,
    /// Size in bytes
    pub size: u64,
}

/// A byte range in a file (alias for Location for clarity in hashing contexts)
pub type ByteRange = Location;

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

/// Information about an extended XMP chunk
#[derive(Debug, Clone)]
pub struct XmpExtendedPart {
    /// Location of the chunk data (after headers)
    pub location: Location,
    /// GUID identifying this XMP set (all parts share same GUID)
    pub guid: String,
    /// Offset where this chunk belongs in the reassembled XMP
    pub chunk_offset: u32,
    /// Total size of the complete extended XMP when reassembled
    pub total_size: u32,
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
    pub fn from_mmap(
        mmap: std::sync::Arc<memmap2::Mmap>,
        offset: usize,
        size: usize,
    ) -> Self {
        Self::MemoryMapped { mmap, offset, size }
    }

    /// Load data from reader at given location
    pub fn load<R: Read>(&mut self, reader: &mut R, location: Location) -> Result<&[u8]> {
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
                reader.read_exact(&mut buffer)?;
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
                let end = offset.checked_add(*size)
                    .ok_or_else(|| crate::Error::InvalidSegment {
                        offset: 0,
                        reason: "Memory-mapped region overflow".into(),
                    })?;
                
                if end > mmap.len() {
                    return Err(crate::Error::InvalidSegment {
                        offset: *offset as u64,
                        reason: format!(
                            "Memory-mapped region out of bounds: {}..{} (file size: {})",
                            offset, end, mmap.len()
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

/// A segment of a file
#[derive(Debug)]
pub enum Segment {
    /// File header/metadata
    Header { offset: u64, size: u64 },

    /// XMP metadata
    Xmp {
        offset: u64,
        size: u64,
        /// Lazy-loaded data
        data: LazyData,
        /// Extended XMP segments if this is a multi-part XMP (JPEG-specific)
        /// 
        /// Note: This is format-specific (JPEG Extended XMP) and should eventually
        /// be moved to a JPEG-specific structure. For now, it's always empty for non-JPEG formats.
        extended_parts: Vec<XmpExtendedPart>,
    },

    /// JUMBF/C2PA data
    Jumbf {
        offset: u64,
        size: u64,
        /// Multiple segments (e.g., JPEG where JUMBF spans multiple APP11)
        segments: Vec<Location>,
        /// Lazy-loaded data
        data: LazyData,
    },

    /// Image data (can be hashed without loading)
    ImageData {
        offset: u64,
        size: u64,
    },

    /// EXIF metadata (JPEG-specific, but could apply to other formats)
    #[cfg(feature = "thumbnails")]
    Exif {
        offset: u64,
        size: u64,
        /// Embedded thumbnail location (if present in IFD1)
        thumbnail: Option<crate::thumbnail::EmbeddedThumbnail>,
    },

    /// Other format-specific segments
    Other {
        offset: u64,
        size: u64,
        /// Format-specific marker/type
        marker: u8,
    },
}

impl Segment {
    /// Get the location of this segment
    pub fn location(&self) -> Location {
        match self {
            Self::Header { offset, size }
            | Self::Xmp { offset, size, .. }
            | Self::Jumbf { offset, size, .. }
            | Self::ImageData { offset, size, .. }
            | Self::Other { offset, size, .. } => Location {
                offset: *offset,
                size: *size,
            },
            #[cfg(feature = "thumbnails")]
            Self::Exif { offset, size, .. } => Location {
                offset: *offset,
                size: *size,
            },
        }
    }

    /// Check if this segment is hashable (DEPRECATED)
    /// 
    /// This method is deprecated. Hashing policy should be determined by the caller
    /// using `hashable_ranges()` with exclusion patterns, not by the parser.
    /// 
    /// This always returns false now. Use `segments_by_path("image_data")` or
    /// `hashable_ranges()` with appropriate exclusions instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use hashable_ranges() with exclusion patterns instead"
    )]
    pub fn is_hashable(&self) -> bool {
        false
    }
    
    /// Get a human-readable path/identifier for this segment
    /// Used for box-based hashing and segment identification
    pub fn path(&self) -> &str {
        match self {
            Self::Header { .. } => "header",
            Self::Xmp { .. } => "xmp",
            Self::Jumbf { .. } => "jumbf",
            Self::ImageData { .. } => "image_data",
            #[cfg(feature = "thumbnails")]
            Self::Exif { .. } => "exif",
            Self::Other { marker, .. } => {
                // For JPEG markers, use hex representation
                match *marker {
                    0xE1 => "APP1",
                    0xEB => "APP11",
                    0xFE => "COM",
                    _ => "other",
                }
            }
        }
    }
}

/// Iterator over chunks of segment data for streaming
/// 
/// This allows hashing large segments without loading them entirely into memory.
pub struct ChunkedSegmentReader<R: Read> {
    reader: R,
    remaining: u64,
    chunk_size: usize,
}

impl<R: Read> ChunkedSegmentReader<R> {
    /// Create a new chunked reader for a segment
    pub fn new(reader: R, size: u64, chunk_size: usize) -> Self {
        Self {
            reader,
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
        self.reader.read_exact(&mut buffer)?;
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
