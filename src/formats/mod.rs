//! Format-specific handlers

use crate::{error::Result, structure::FileStructure, Updates};
use std::io::{Read, Seek, Write};

/// Trait for format-specific file handlers
pub trait FormatHandler: Send + Sync {
    /// Parse file structure in single pass
    ///
    /// This discovers all segments, XMP, and JUMBF locations without
    /// loading the actual data into memory.
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<FileStructure>;

    /// Write file with updates in single streaming pass
    ///
    /// This streams from the source to destination, applying updates
    /// without loading the entire file into memory.
    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()>;

    /// Extract XMP data from file (format-specific)
    ///
    /// This handles format-specific details like JPEG's extended XMP
    /// with multi-segment assembly, or PNG's simple iTXt chunks.
    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Extract JUMBF data from file (format-specific)
    ///
    /// This handles format-specific details like JPEG XT headers,
    /// multi-segment assembly, etc.
    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Generate thumbnail if supported by this format
    #[cfg(feature = "thumbnails")]
    fn generate_thumbnail<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
        request: &crate::ThumbnailRequest,
    ) -> Result<Option<Vec<u8>>>;
}

#[cfg(feature = "jpeg")]
pub mod jpeg;

#[cfg(feature = "png")]
pub mod png;
