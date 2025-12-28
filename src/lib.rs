//! High-performance, media type-agnostic streaming I/O for media asset metadata.
//!
//! This crate provides efficient, single-pass parsing and writing of media files
//! with JUMBF (JPEG Universal Metadata Box Format) and XMP metadata.
//!
//! # Design Principles
//!
//! - **Streaming**: Process files without loading them entirely into memory
//! - **Lazy loading**: Only read data when explicitly accessed
//! - **Zero-copy**: Use memory-mapped files when beneficial
//! - **Media type agnostic**: Unified API across JPEG, PNG, MP4, and more
//!
//! # Quick Start (Media Type-Agnostic API)
//!
//! The simplest way to use this library is with the [`Asset`] API,
//! which automatically detects the media type:
//!
//! ```no_run
//! use asset_io::{Asset, Updates, XmpUpdate, JumbfUpdate};
//!
//! # fn main() -> asset_io::Result<()> {
//! // Open any supported file - media type is auto-detected
//! let mut asset = Asset::open("image.jpg")?;
//!
//! // Read metadata
//! if let Some(xmp) = asset.xmp()? {
//!     println!("XMP: {} bytes", xmp.len());
//! }
//! if let Some(jumbf) = asset.jumbf()? {
//!     println!("JUMBF: {} bytes", jumbf.len());
//! }
//!
//! // Modify and write
//! let updates = Updates {
//!     xmp: XmpUpdate::Set(b"<new>metadata</new>".to_vec()),
//!     jumbf: JumbfUpdate::Remove,
//!     ..Default::default()
//! };
//! asset.write_to("output.jpg", &updates)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Handler-Specific API
//!
//! For more control, you can use media type-specific handlers:
//!
//! ```no_run
//! use asset_io::{ContainerIO, JpegIO, Updates};
//! use std::fs::File;
//!
//! # fn main() -> asset_io::Result<()> {
//! // Parse file structure in single pass
//! let mut file = File::open("image.jpg")?;
//! let handler = JpegIO::new();
//! let structure = handler.parse(&mut file)?;
//!
//! // Access XMP data (loaded lazily via handler)
//! if let Some(xmp) = handler.extract_xmp(&structure, &mut file)? {
//!     println!("Found XMP: {} bytes", xmp.len());
//! }
//!
//! // Write with updates
//! let updates = Updates::default();
//! let mut output = File::create("output.jpg")?;
//! handler.write(&structure, &mut file, &mut output, &updates)?;
//! # Ok(())
//! # }
//! ```

mod asset;
mod error;
mod formats;
mod media_type;
mod segment;
mod structure;
pub mod thumbnail;
#[cfg(feature = "exif")]
mod tiff;
#[cfg(feature = "xmp")]
pub mod xmp;

pub use asset::{Asset, AssetBuilder};
pub use error::{Error, Result};
pub use formats::ContainerIO;
pub use media_type::MediaType;
pub use segment::{
    ByteRange, ChunkedSegmentReader, LazyData, Location, Segment, SegmentMetadata,
    DEFAULT_CHUNK_SIZE, MAX_SEGMENT_SIZE,
};
pub use structure::Structure;
pub use thumbnail::{EmbeddedThumbnail, ThumbnailFormat, ThumbnailGenerator, ThumbnailOptions};

// Container and handlers are exported by the register_containers! macro below

// Test utilities - only compiled for tests or when explicitly enabled
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

/// Updates to apply when writing a file
#[derive(Debug, Default)]
pub struct Updates {
    /// XMP data update strategy
    pub xmp: XmpUpdate,

    /// JUMBF data update strategy
    pub jumbf: JumbfUpdate,
}

/// XMP metadata update strategy
#[derive(Debug, Clone, Default)]
pub enum XmpUpdate {
    /// Keep existing XMP (default)
    #[default]
    Keep,
    /// Remove existing XMP
    Remove,
    /// Replace or add XMP
    Set(Vec<u8>),
}

/// JUMBF data update strategy
#[derive(Debug, Clone, Default)]
pub enum JumbfUpdate {
    /// Keep existing JUMBF (default)
    #[default]
    Keep,
    /// Remove existing JUMBF
    Remove,
    /// Replace or add JUMBF
    Set(Vec<u8>),
}

// Legacy convenience constructors for backward compatibility
impl Updates {
    /// Create updates that keep existing metadata (no changes)
    pub fn keep_all() -> Self {
        Self::default()
    }

    /// Create updates that remove all metadata
    pub fn remove_all() -> Self {
        Self {
            xmp: XmpUpdate::Remove,
            jumbf: JumbfUpdate::Remove,
        }
    }

    /// Create updates that set new XMP
    pub fn with_xmp(xmp: Vec<u8>) -> Self {
        Self {
            xmp: XmpUpdate::Set(xmp),
            ..Default::default()
        }
    }

    /// Create updates that set new JUMBF
    pub fn with_jumbf(jumbf: Vec<u8>) -> Self {
        Self {
            jumbf: JumbfUpdate::Set(jumbf),
            ..Default::default()
        }
    }
}

// Re-export generated items from formats module
pub(crate) use formats::{detect_container, get_handler, Handler};
pub use formats::{detect_from_extension, detect_from_mime, Container};

// Re-export container handlers at crate root
#[cfg(feature = "jpeg")]
pub use formats::jpeg_io::JpegIO;
#[cfg(feature = "png")]
pub use formats::png_io::PngIO;
