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
        // TODO: Add mmap handle
    },
}

impl LazyData {
    /// Load data from reader at given location
    pub fn load<R: Read>(&mut self, reader: &mut R, location: Location) -> Result<&[u8]> {
        match self {
            Self::NotLoaded => {
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
            Self::MemoryMapped { .. } => {
                // TODO: Return slice from mmap
                unimplemented!("Memory-mapped support")
            }
        }
    }

    /// Get data if already loaded
    pub fn get(&self) -> Option<&[u8]> {
        match self {
            Self::Loaded(data) => Some(data),
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
        /// Whether this should be included in hash calculations
        hashable: bool,
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
        }
    }

    /// Check if this segment is hashable
    pub fn is_hashable(&self) -> bool {
        match self {
            Self::ImageData { hashable, .. } => *hashable,
            _ => false,
        }
    }
}
