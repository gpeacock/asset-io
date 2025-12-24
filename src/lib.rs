//! High-performance streaming JUMBF and XMP I/O for media files.
//!
//! This crate provides efficient, single-pass parsing and writing of media files
//! with JUMBF (JPEG Universal Metadata Box Format) and XMP metadata.
//!
//! # Design Principles
//!
//! - **Streaming**: Process files without loading them entirely into memory
//! - **Lazy loading**: Only read data when explicitly accessed
//! - **Zero-copy**: Use memory-mapped files when beneficial
//! - **Format agnostic**: Easy to add support for new formats
//!
//! # Quick Start (Format-Agnostic API)
//!
//! The simplest way to use this library is with the [`Asset`] API,
//! which automatically detects the file format:
//!
//! ```no_run
//! use asset_io::{Asset, Updates, XmpUpdate, JumbfUpdate};
//!
//! # fn main() -> asset_io::Result<()> {
//! // Open any supported file - format is auto-detected
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
//! # Format-Specific API
//!
//! For more control, you can use format-specific handlers:
//!
//! ```no_run
//! use asset_io::{FormatHandler, JpegHandler, Updates};
//! use std::fs::File;
//!
//! # fn main() -> asset_io::Result<()> {
//! // Parse file structure in single pass
//! let mut file = File::open("image.jpg")?;
//! let handler = JpegHandler::new();
//! let mut structure = handler.parse(&mut file)?;
//!
//! // Access XMP data (loaded lazily)
//! if let Some(xmp) = structure.xmp(&mut file)? {
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
mod segment;
mod structure;
pub mod thumbnail;
#[cfg(feature = "thumbnails")]
mod tiff;

pub use asset::{Asset, AssetBuilder};
pub use error::{Error, Result};
pub use formats::FormatHandler;
pub use segment::{
    ByteRange, ChunkedSegmentReader, LazyData, Location, Segment, SegmentMetadata,
    DEFAULT_CHUNK_SIZE, MAX_SEGMENT_SIZE,
};
pub use structure::FileStructure;
pub use thumbnail::{
    format_hint, EmbeddedThumbnail, ThumbnailFormat, ThumbnailGenerator, ThumbnailOptions,
};

#[cfg(feature = "jpeg")]
pub use formats::jpeg::JpegHandler;

#[cfg(feature = "png")]
pub use formats::png::PngHandler;

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

    /// Request thumbnail generation
    #[cfg(feature = "thumbnails")]
    pub thumbnail: Option<ThumbnailRequest>,
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
            #[cfg(feature = "thumbnails")]
            thumbnail: None,
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

/// Request for thumbnail generation
#[cfg(feature = "thumbnails")]
#[derive(Debug, Clone)]
pub struct ThumbnailRequest {
    pub max_width: u32,
    pub max_height: u32,
    pub quality: u8,
}

/// Register all supported formats in one place
///
/// This macro generates:
/// - Format enum with variants
/// - detect_format() function
/// - get_handler() function  
/// - Extension and MIME type lookup
macro_rules! register_formats {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $handler:ty
    ),* $(,)?) => {
        /// Supported file formats
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Format {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        /// Detect format from file header
        pub(crate) fn detect_format<R: std::io::Read + std::io::Seek>(
            reader: &mut R
        ) -> Result<Format> {
            use std::io::SeekFrom;

            reader.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let n = reader.read(&mut header)?;
            let header = &header[..n];

            if n < 2 {
                return Err(Error::InvalidFormat("File too small".into()));
            }

            $(
                $(#[$meta])*
                if let Some(fmt) = <$handler>::detect(header) {
                    return Ok(fmt);
                }
            )*

            Err(Error::UnsupportedFormat)
        }

        /// Get handler for a format
        pub(crate) fn get_handler(format: Format) -> Result<$crate::asset::Handler> {
            match format {
                $(
                    $(#[$meta])*
                    Format::$variant => Ok($crate::asset::Handler::$variant(<$handler>::new())),
                )*
            }
        }

        /// Detect format from file extension
        pub fn detect_from_extension(ext: &str) -> Option<Format> {
            let ext_lower = ext.to_lowercase();
            $(
                $(#[$meta])*
                if <$handler>::extensions().contains(&ext_lower.as_str()) {
                    return <$handler>::supported_formats().first().copied();
                }
            )*
            None
        }

        /// Detect format from MIME type
        pub fn detect_from_mime(mime: &str) -> Option<Format> {
            $(
                $(#[$meta])*
                if <$handler>::mime_types().iter().any(|m| m.eq_ignore_ascii_case(mime)) {
                    return <$handler>::supported_formats().first().copied();
                }
            )*
            None
        }
    };
}

// ============================================================================
// SINGLE POINT OF REGISTRATION
// To add a new format, just add one line here!
// ============================================================================
register_formats! {
    #[cfg(feature = "jpeg")]
    Jpeg => formats::jpeg::JpegHandler,

    #[cfg(feature = "png")]
    Png => formats::png::PngHandler,
}
