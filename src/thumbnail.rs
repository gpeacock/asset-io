//! Thumbnail generation interface
//!
//! This module provides a format-agnostic interface for thumbnail generation.
//! The core library provides efficient access to image data (with zero-copy
//! when memory-mapped), and external crates implement the actual decoding
//! and thumbnail generation.
//!
//! # Design Philosophy
//!
//! The `jumbf-io` crate does NOT include image decoding or thumbnail generation
//! to keep dependencies minimal. Instead, it provides:
//!
//! 1. **Embedded thumbnail extraction** - Fast path for formats with pre-rendered thumbnails
//! 2. **Zero-copy image access** - Direct memory slices via memory-mapping
//! 3. **Streaming access** - Constant memory usage for large files
//!
//! # Example
//!
//! External crates implement the `ThumbnailGenerator` trait:
//!
//! ```rust,ignore
//! use asset_io::{ThumbnailGenerator, ThumbnailOptions, Asset};
//! use image::DynamicImage;
//!
//! pub struct ImageThumbnailGenerator;
//!
//! impl ThumbnailGenerator for ImageThumbnailGenerator {
//!     fn generate(&self, data: &[u8], format_hint: Option<&str>) -> Result<Vec<u8>> {
//!         let img = image::load_from_memory(data)?;
//!         let thumb = img.thumbnail(256, 256);
//!         // ... encode as JPEG
//!     }
//! }
//! ```

use crate::{error::Result, Format};

/// Options for thumbnail generation
#[derive(Debug, Clone)]
pub struct ThumbnailOptions {
    /// Maximum thumbnail width in pixels
    pub max_width: u32,
    
    /// Maximum thumbnail height in pixels
    pub max_height: u32,
    
    /// JPEG quality for output (1-100)
    pub quality: u8,
    
    /// Prefer embedded thumbnails if available (faster)
    pub prefer_embedded: bool,
}

impl Default for ThumbnailOptions {
    fn default() -> Self {
        Self {
            max_width: 256,
            max_height: 256,
            quality: 85,
            prefer_embedded: true,
        }
    }
}

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

/// An embedded thumbnail extracted from a file
///
/// Many image formats store pre-rendered thumbnails for quick preview:
/// - JPEG: EXIF thumbnail (typically 160x120)
/// - HEIF/HEIC: 'thmb' item reference
/// - WebP: VP8L thumbnail chunk
/// - TIFF: IFD0 thumbnail
#[derive(Debug, Clone)]
pub struct EmbeddedThumbnail {
    /// Raw thumbnail data (already encoded)
    pub data: Vec<u8>,
    
    /// Format of the thumbnail
    pub format: ThumbnailFormat,
    
    /// Width in pixels (if known)
    pub width: Option<u32>,
    
    /// Height in pixels (if known)
    pub height: Option<u32>,
}

impl EmbeddedThumbnail {
    /// Create a new embedded thumbnail
    pub fn new(data: Vec<u8>, format: ThumbnailFormat) -> Self {
        Self {
            data,
            format,
            width: None,
            height: None,
        }
    }
    
    /// Create a new embedded thumbnail with dimensions
    pub fn with_dimensions(
        data: Vec<u8>,
        format: ThumbnailFormat,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            data,
            format,
            width: Some(width),
            height: Some(height),
        }
    }
    
    /// Check if this thumbnail is within the requested size
    pub fn fits(&self, max_width: u32, max_height: u32) -> bool {
        match (self.width, self.height) {
            (Some(w), Some(h)) => w <= max_width && h <= max_height,
            _ => false, // Unknown size - assume it doesn't fit
        }
    }
}

/// Trait for implementing thumbnail generation
///
/// External crates implement this trait to provide thumbnail generation
/// without adding dependencies to the core `jumbf-io` library.
///
/// # Example
///
/// ```rust,ignore
/// use asset_io::{ThumbnailGenerator, Result};
/// use image::DynamicImage;
///
/// pub struct FastThumbnailGenerator;
///
/// impl ThumbnailGenerator for FastThumbnailGenerator {
///     fn generate(&self, data: &[u8], format_hint: Option<&str>) -> Result<Vec<u8>> {
///         // Use 'image' crate to decode and generate thumbnail
///         let img = image::load_from_memory(data)?;
///         let thumb = img.thumbnail(256, 256);
///         
///         // Encode as JPEG
///         let mut buf = Vec::new();
///         thumb.write_to(&mut std::io::Cursor::new(&mut buf), 
///                        image::ImageFormat::Jpeg)?;
///         Ok(buf)
///     }
/// }
/// ```
pub trait ThumbnailGenerator {
    /// Generate a thumbnail from raw image data
    ///
    /// # Arguments
    ///
    /// * `data` - Raw compressed image data (JPEG, PNG, WebP, etc.)
    /// * `format_hint` - Optional format hint ("jpeg", "png", "webp", etc.)
    ///
    /// # Returns
    ///
    /// Thumbnail encoded as JPEG (or other format) as raw bytes
    fn generate(&self, data: &[u8], format_hint: Option<&str>) -> Result<Vec<u8>>;
}

/// Get a format hint string for a Format enum
pub fn format_hint(format: Format) -> &'static str {
    match format {
        Format::Jpeg => "jpeg",
        #[cfg(feature = "png")]
        Format::Png => "png",
        #[cfg(feature = "bmff")]
        Format::Bmff => "mp4",
    }
}

