//! Format-agnostic asset handling
//!
//! This module provides a unified API for working with media files
//! without needing to know the specific format.

use crate::{
    detect_format, error::Result, get_handler, structure::Structure, Format, FormatHandler,
    Updates,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

#[cfg(feature = "jpeg")]
use crate::formats::jpeg::JpegHandler;

#[cfg(feature = "png")]
use crate::formats::png::PngHandler;

/// A media asset that automatically detects and handles its format
///
/// # Example
///
/// ```no_run
/// use asset_io::{Asset, Updates, XmpUpdate};
///
/// # fn main() -> asset_io::Result<()> {
/// // Open any supported media file - format is auto-detected
/// let mut asset = Asset::open("image.jpg")?;
///
/// // Read metadata
/// if let Some(xmp) = asset.xmp()? {
///     println!("XMP: {} bytes", xmp.len());
/// }
///
/// // Modify and write
/// let updates = Updates {
///     xmp: XmpUpdate::Set(b"<new>metadata</new>".to_vec()),
///     ..Default::default()
/// };
/// asset.write_to("output.jpg", &updates)?;
/// # Ok(())
/// # }
/// ```
pub struct Asset<R: Read + Seek> {
    reader: R,
    structure: Structure,
    handler: Handler,
}

/// Internal enum to hold format-specific handlers
pub(crate) enum Handler {
    #[cfg(feature = "jpeg")]
    Jpeg(JpegHandler),

    #[cfg(feature = "png")]
    Png(PngHandler),
}

impl Handler {
    #[allow(unreachable_patterns)]
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<Structure> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.parse(reader),

            #[cfg(feature = "png")]
            Handler::Png(h) => h.parse(reader),
        }
    }

    #[allow(unreachable_patterns)]
    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
        reader: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.write(structure, reader, writer, updates),

            #[cfg(feature = "png")]
            Handler::Png(h) => h.write(structure, reader, writer, updates),
        }
    }

    #[allow(unreachable_patterns)]
    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.extract_xmp(structure, reader),

            #[cfg(feature = "png")]
            Handler::Png(h) => h.extract_xmp(structure, reader),
        }
    }

    #[allow(unreachable_patterns)]
    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.extract_jumbf(structure, reader),

            #[cfg(feature = "png")]
            Handler::Png(h) => h.extract_jumbf(structure, reader),
        }
    }

    #[cfg(feature = "exif")]
    #[allow(unreachable_patterns)]
    fn extract_embedded_thumbnail<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.extract_embedded_thumbnail(structure, reader),

            #[cfg(feature = "png")]
            Handler::Png(h) => h.extract_embedded_thumbnail(structure, reader),
        }
    }
}

impl Asset<File> {
    /// Open a media file from a path
    ///
    /// The format is automatically detected from the file header.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(path)?;
        Self::from_file(file)
    }

    /// Create an Asset from an owned File
    pub fn from_file(mut file: File) -> Result<Self> {
        file.seek(SeekFrom::Start(0))?;
        let format = detect_format(&mut file)?;
        file.seek(SeekFrom::Start(0))?;

        let handler = get_handler(format)?;
        let structure = handler.parse(&mut file)?;

        Ok(Asset {
            reader: file,
            structure,
            handler,
        })
    }
}

impl<R: Read + Seek> Asset<R> {
    /// Create an Asset from a reader with a known format
    ///
    /// This is useful when you already know the format or want to parse
    /// a reader that isn't a File.
    pub fn from_reader_with_format(mut reader: R, format: Format) -> Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let handler = get_handler(format)?;
        let structure = handler.parse(&mut reader)?;

        Ok(Asset {
            reader,
            structure,
            handler,
        })
    }

    /// Get the detected format
    pub fn format(&self) -> Format {
        self.structure.format
    }

    /// Get XMP metadata (loads lazily, assembles extended parts if present)
    pub fn xmp(&mut self) -> Result<Option<Vec<u8>>> {
        self.handler.extract_xmp(&self.structure, &mut self.reader)
    }

    /// Get JUMBF data (loads and assembles lazily)
    pub fn jumbf(&mut self) -> Result<Option<Vec<u8>>> {
        self.handler.extract_jumbf(&self.structure, &mut self.reader)
    }

    /// Extract an embedded thumbnail if available
    ///
    /// Many image formats include pre-rendered thumbnails for quick preview:
    /// - JPEG: EXIF thumbnail (typically 160x120)
    /// - PNG: EXIF thumbnail (if eXIf chunk present)
    ///
    /// This is the fastest way to get a thumbnail if available - no decoding needed!
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Try embedded thumbnail first (fastest!)
    /// if let Some(thumb) = asset.embedded_thumbnail()? {
    ///     println!("Found {}x{} thumbnail", thumb.width, thumb.height);
    ///     return Ok(thumb.data);
    /// }
    /// // Fall back to decoding main image
    /// ```
    #[cfg(feature = "exif")]
    pub fn embedded_thumbnail(&mut self) -> Result<Option<crate::EmbeddedThumbnail>> {
        self.handler
            .extract_embedded_thumbnail(&self.structure, &mut self.reader)
    }

    /// Get the file structure
    pub fn structure(&self) -> &Structure {
        &self.structure
    }

    /// Get a mutable reference to the reader
    ///
    /// This allows advanced operations like chunked reading for hashing
    pub fn reader_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Create a chunked reader for a byte range (convenience method)
    pub fn read_range_chunked(
        &mut self,
        range: crate::ByteRange,
        chunk_size: usize,
    ) -> Result<crate::ChunkedSegmentReader<std::io::Take<&mut R>>> {
        self.structure
            .read_range_chunked(&mut self.reader, range, chunk_size)
    }

    /// Create a chunked reader for a segment (convenience method)
    pub fn read_segment_chunked(
        &mut self,
        segment_index: usize,
        chunk_size: usize,
    ) -> Result<crate::ChunkedSegmentReader<std::io::Take<&mut R>>> {
        self.structure
            .read_segment_chunked(&mut self.reader, segment_index, chunk_size)
    }
}

impl Asset<File> {
    /// Write to a new file with updates
    pub fn write_to<P: AsRef<Path>>(&mut self, path: P, updates: &Updates) -> Result<()> {
        let mut output = File::create(path)?;
        self.reader.seek(SeekFrom::Start(0))?;
        self.handler
            .write(&self.structure, &mut self.reader, &mut output, updates)
    }

    /// Write to an existing writer with updates
    pub fn write<W: Write>(&mut self, writer: &mut W, updates: &Updates) -> Result<()> {
        self.reader.seek(SeekFrom::Start(0))?;
        self.handler
            .write(&self.structure, &mut self.reader, writer, updates)
    }
}

/// Builder for creating assets with custom options
pub struct AssetBuilder {
    // Future: Add options like memory mapping, buffer sizes, etc.
}

impl AssetBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {}
    }

    /// Open an asset with the configured options
    pub fn open<P: AsRef<Path>>(self, path: P) -> Result<Asset<File>> {
        Asset::open(path)
    }
}

impl Default for AssetBuilder {
    fn default() -> Self {
        Self::new()
    }
}
