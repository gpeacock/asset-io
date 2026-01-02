//! Embedded thumbnail extraction
//!
//! This module provides types for working with pre-rendered thumbnails
//! embedded in image files. Many formats include small preview images:
//!
//! - JPEG: EXIF thumbnail (typically 160x120)
//! - HEIF/HEIC: 'thmb' item reference
//! - PNG: EXIF thumbnail (if eXIf chunk present)
//!
//! Use `Asset::read_embedded_thumbnail()` to extract these thumbnails.
//!
//! # Example
//!
//! ```no_run
//! use asset_io::Asset;
//!
//! # fn main() -> asset_io::Result<()> {
//! let mut asset = Asset::open("photo.jpg")?;
//!
//! if let Some(thumb) = asset.read_embedded_thumbnail()? {
//!     println!("Found {:?} thumbnail, {} bytes", thumb.format, thumb.data.len());
//!     std::fs::write("thumbnail.jpg", &thumb.data)?;
//! }
//! # Ok(())
//! # }
//! ```

/// Format of an embedded thumbnail
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailFormat {
    /// JPEG thumbnail
    Jpeg,
    /// PNG thumbnail
    Png,
    /// WebP thumbnail
    WebP,
    /// Other/unknown format
    Other,
}

/// An embedded thumbnail with its data
///
/// This struct contains the actual thumbnail bytes extracted from an image file.
/// Use `Asset::read_embedded_thumbnail()` to obtain one.
#[derive(Debug, Clone)]
pub struct Thumbnail {
    /// The raw thumbnail image data (typically JPEG)
    pub data: Vec<u8>,

    /// Format of the thumbnail
    pub format: ThumbnailFormat,

    /// Width in pixels (if known)
    pub width: Option<u32>,

    /// Height in pixels (if known)
    pub height: Option<u32>,
}

impl Thumbnail {
    /// Create a new thumbnail with data
    pub fn new(data: Vec<u8>, format: ThumbnailFormat) -> Self {
        Self {
            data,
            format,
            width: None,
            height: None,
        }
    }

    /// Create a new thumbnail with dimensions
    pub fn with_dimensions(data: Vec<u8>, format: ThumbnailFormat, width: u32, height: u32) -> Self {
        Self {
            data,
            format,
            width: Some(width),
            height: Some(height),
        }
    }
}

/// Location info for an embedded thumbnail
///
/// This is used internally to track where thumbnail data is located.
/// Use `Asset::read_embedded_thumbnail()` to get the actual bytes.
#[derive(Debug, Clone)]
pub struct EmbeddedThumbnailInfo {
    /// Offset of thumbnail data in the file
    pub offset: u64,

    /// Size of thumbnail data in bytes
    pub size: u64,

    /// Format of the thumbnail
    pub format: ThumbnailFormat,

    /// Width in pixels (if known)
    pub width: Option<u32>,

    /// Height in pixels (if known)
    pub height: Option<u32>,
}

impl EmbeddedThumbnailInfo {
    /// Create a new embedded thumbnail info with location
    pub fn new(
        offset: u64,
        size: u64,
        format: ThumbnailFormat,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self {
            offset,
            size,
            format,
            width,
            height,
        }
    }
}
