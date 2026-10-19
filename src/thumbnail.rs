//! Embedded thumbnail extraction
//!
//! This module provides types for working with pre-rendered thumbnails
//! embedded in image files. Many formats include small preview images:
//!
//! - JPEG: EXIF thumbnail (typically 160x120)
//! - HEIF/HEIC: 'thmb' item reference
//! - PNG: EXIF thumbnail (if eXIf chunk present)
//!
//! These types describe such a thumbnail; locating and extracting it is the job
//! of the container handler that knows where the format keeps it.

use crate::error::Result;
use crate::segment::SegmentKind;
use crate::structure::Structure;
use std::io::{Read, Seek};

/// Format of an embedded thumbnail
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailKind {
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
#[derive(Debug, Clone)]
pub struct Thumbnail {
    /// The raw thumbnail image data (typically JPEG)
    pub data: Vec<u8>,

    /// Format of the thumbnail
    pub format: ThumbnailKind,

    /// Width in pixels (if known)
    pub width: Option<u32>,

    /// Height in pixels (if known)
    pub height: Option<u32>,
}

impl Thumbnail {
    /// Create a new thumbnail with data
    pub fn new(data: Vec<u8>, format: ThumbnailKind) -> Self {
        Self {
            data,
            format,
            width: None,
            height: None,
        }
    }

    /// Create a new thumbnail with dimensions
    pub fn with_dimensions(data: Vec<u8>, format: ThumbnailKind, width: u32, height: u32) -> Self {
        Self {
            data,
            format,
            width: Some(width),
            height: Some(height),
        }
    }
}

impl Structure {
    /// The asset's embedded thumbnail, if it has one.
    ///
    /// A thumbnail the container stores as its own element
    /// ([`SegmentKind::Thumbnail`], a HEIF `thmb` item) is preferred. Its format is
    /// [`ThumbnailKind::Jpeg`] for a `jpeg` item; otherwise (`hvc1`, `av01`, ...)
    /// [`ThumbnailKind::Other`], and `data` is the coded item, which needs the
    /// codec configuration stored elsewhere in the file to decode. Failing that,
    /// with the `exif` feature, the JPEG thumbnail in the EXIF data (IFD1) is
    /// returned.
    pub fn embedded_thumbnail<R: Read + Seek>(&self, src: &mut R) -> Result<Option<Thumbnail>> {
        if let Some(e) = self.entities(SegmentKind::Thumbnail).next() {
            let format = match e.label() {
                Some("jpeg") => ThumbnailKind::Jpeg,
                _ => ThumbnailKind::Other,
            };
            return Ok(Some(Thumbnail::new(self.read_entity(src, e)?, format)));
        }
        #[cfg(feature = "exif")]
        {
            self.exif_thumbnail(src)
        }
        #[cfg(not(feature = "exif"))]
        {
            Ok(None)
        }
    }
}

/// Location info for an embedded thumbnail
///
/// This is used internally to track where thumbnail data is located.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Only constructed when exif/bmff features are enabled
pub struct EmbeddedThumbnailInfo {
    /// Offset of thumbnail data in the file
    pub offset: u64,

    /// Size of thumbnail data in bytes
    pub size: u64,

    /// Format of the thumbnail
    pub format: ThumbnailKind,

    /// Width in pixels (if known)
    pub width: Option<u32>,

    /// Height in pixels (if known)
    pub height: Option<u32>,
}

impl EmbeddedThumbnailInfo {
    /// Create a new embedded thumbnail info with location
    #[allow(dead_code)] // Only used when exif/bmff features are enabled
    pub fn new(
        offset: u64,
        size: u64,
        format: ThumbnailKind,
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
