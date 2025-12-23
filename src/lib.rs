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
//! # Example
//!
//! ```no_run
//! use jumbf_io::{FormatHandler, JpegHandler, Updates};
//! use std::fs::File;
//!
//! # fn main() -> jumbf_io::Result<()> {
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

mod error;
mod structure;
mod segment;
mod formats;

pub use error::{Error, Result};
pub use structure::FileStructure;
pub use segment::{Segment, Location, LazyData};
pub use formats::FormatHandler;

#[cfg(feature = "jpeg")]
pub use formats::jpeg::JpegHandler;

/// Updates to apply when writing a file
#[derive(Debug, Default)]
pub struct Updates {
    /// New XMP data to write (None = keep existing)
    pub new_xmp: Option<Vec<u8>>,
    
    /// New JUMBF data to write (None = keep existing)
    pub new_jumbf: Option<Vec<u8>>,
    
    /// Remove existing JUMBF data
    pub remove_existing_jumbf: bool,
    
    /// Request thumbnail generation
    #[cfg(feature = "thumbnails")]
    pub thumbnail: Option<ThumbnailRequest>,
}

/// Request for thumbnail generation
#[cfg(feature = "thumbnails")]
#[derive(Debug, Clone)]
pub struct ThumbnailRequest {
    pub max_width: u32,
    pub max_height: u32,
    pub quality: u8,
}

/// Supported file formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// JPEG/JPG format
    #[cfg(feature = "jpeg")]
    Jpeg,
    
    /// PNG format
    #[cfg(feature = "png")]
    Png,
    
    /// BMFF (MP4, MOV, etc.)
    #[cfg(feature = "bmff")]
    Bmff,
}
