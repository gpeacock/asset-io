//! Format-agnostic asset handling
//!
//! This module provides a unified API for working with media files
//! without needing to know the specific format.

use crate::{
    error::{Error, Result},
    structure::FileStructure,
    Format, FormatHandler, Updates,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

#[cfg(feature = "jpeg")]
use crate::formats::jpeg::JpegHandler;

/// A media asset that automatically detects and handles its format
///
/// # Example
///
/// ```no_run
/// use jumbf_io::{Asset, Updates, XmpUpdate};
///
/// # fn main() -> jumbf_io::Result<()> {
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
    structure: FileStructure,
    handler: Handler,
}

/// Internal enum to hold format-specific handlers
enum Handler {
    #[cfg(feature = "jpeg")]
    Jpeg(JpegHandler),

    #[cfg(feature = "png")]
    Png, // TODO: Add PNG handler

    #[cfg(feature = "bmff")]
    Bmff, // TODO: Add BMFF handler
}

impl Handler {
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<FileStructure> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.parse(reader),

            #[cfg(feature = "png")]
            Handler::Png => Err(Error::UnsupportedFormat),

            #[cfg(feature = "bmff")]
            Handler::Bmff => Err(Error::UnsupportedFormat),
        }
    }

    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        match self {
            #[cfg(feature = "jpeg")]
            Handler::Jpeg(h) => h.write(structure, reader, writer, updates),

            #[cfg(feature = "png")]
            Handler::Png => Err(Error::UnsupportedFormat),

            #[cfg(feature = "bmff")]
            Handler::Bmff => Err(Error::UnsupportedFormat),
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
        self.structure.xmp(&mut self.reader)
    }

    /// Get JUMBF data (loads and assembles lazily)
    pub fn jumbf(&mut self) -> Result<Option<Vec<u8>>> {
        self.structure.jumbf(&mut self.reader)
    }

    /// Get the file structure
    pub fn structure(&self) -> &FileStructure {
        &self.structure
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

/// Detect the format from the file header
fn detect_format<R: Read + Seek>(reader: &mut R) -> Result<Format> {
    reader.seek(SeekFrom::Start(0))?;

    let mut header = [0u8; 16];
    let n = reader.read(&mut header)?;

    if n < 2 {
        return Err(Error::InvalidFormat("File too small".into()));
    }

    // JPEG: FF D8
    #[cfg(feature = "jpeg")]
    if header[0] == 0xFF && header[1] == 0xD8 {
        return Ok(Format::Jpeg);
    }

    // PNG: 89 50 4E 47 0D 0A 1A 0A
    #[cfg(feature = "png")]
    if n >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
        return Ok(Format::Png);
    }

    // MP4/MOV: Check for ftyp box (offset 4-8)
    #[cfg(feature = "bmff")]
    if n >= 12 && &header[4..8] == b"ftyp" {
        return Ok(Format::Bmff);
    }

    Err(Error::UnsupportedFormat)
}

/// Get a handler for the detected format
fn get_handler(format: Format) -> Result<Handler> {
    match format {
        #[cfg(feature = "jpeg")]
        Format::Jpeg => Ok(Handler::Jpeg(JpegHandler::new())),

        #[cfg(feature = "png")]
        Format::Png => Err(Error::UnsupportedFormat), // TODO: Implement PNG handler

        #[cfg(feature = "bmff")]
        Format::Bmff => Err(Error::UnsupportedFormat), // TODO: Implement BMFF handler
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
