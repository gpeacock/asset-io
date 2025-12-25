//! Format-specific handlers

use crate::{error::Result, structure::Structure, Updates};
use std::io::{Read, Seek, Write};

/// Trait for format-specific file handlers
pub trait FormatHandler: Send + Sync {
    /// Formats this handler supports
    fn supported_formats() -> &'static [Format]
    where
        Self: Sized;

    /// File extensions this handler accepts (e.g., ["jpg", "jpeg"])
    fn extensions() -> &'static [&'static str]
    where
        Self: Sized;

    /// MIME types this handler accepts
    fn mime_types() -> &'static [&'static str]
    where
        Self: Sized;

    /// Try to detect if this handler can parse the given header
    /// Returns Some(Format) if confident, None if unsure
    fn detect(header: &[u8]) -> Option<Format>
    where
        Self: Sized;

    /// Parse file structure in single pass
    ///
    /// This discovers all segments, XMP, and JUMBF locations without
    /// loading the actual data into memory.
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<Structure>;

    /// Write file with updates in single streaming pass
    ///
    /// This streams from the source to destination, applying updates
    /// without loading the entire file into memory.
    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
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
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Extract JUMBF data from file (format-specific)
    ///
    /// This handles format-specific details like JPEG XT headers,
    /// multi-segment assembly, etc.
    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Extract embedded thumbnail from format-specific metadata
    ///
    /// Some formats embed pre-rendered thumbnails in their metadata:
    /// - JPEG: EXIF IFD1 thumbnail (typically ~160x120)
    /// - HEIF: 'thmb' item reference
    /// - WebP: VP8L thumbnail chunk
    /// - PNG: No embedded thumbnails (returns None)
    ///
    /// This is the fastest thumbnail path - no decoding needed!
    #[cfg(feature = "exif")]
    fn extract_embedded_thumbnail<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>>;
}

// Format handler modules - pub(crate) so register_formats! macro can access them
#[cfg(feature = "jpeg")]
pub(crate) mod jpeg;

#[cfg(feature = "png")]
pub(crate) mod png;

// ============================================================================
// Format Registration Macro
// ============================================================================

/// Register all supported formats in one place
///
/// This macro generates:
/// - Format enum with variants
/// - Handler enum for internal use
/// - Handler implementation with format delegation
/// - detect_format() function
/// - get_handler() function  
/// - Extension and MIME type lookup
/// - Public exports of format handlers at crate root
macro_rules! register_formats {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $module:ident :: $handler:ident
    ),* $(,)?) => {
        /// Supported file formats
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Format {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        // Export format handlers at crate root (not in formats module)
        // This is done in lib.rs via pub use formats::*;

        // Generate Handler enum for internal use
        pub(crate) enum Handler {
            $(
                $(#[$meta])*
                $variant($module::$handler),
            )*
        }

        // Generate Handler implementation
        impl Handler {
            #[allow(unreachable_patterns)]
            pub(crate) fn parse<R: std::io::Read + std::io::Seek>(&self, reader: &mut R) -> $crate::Result<$crate::Structure> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.parse(reader),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn write<R: std::io::Read + std::io::Seek, W: std::io::Write>(
                &self,
                structure: &$crate::Structure,
                reader: &mut R,
                writer: &mut W,
                updates: &$crate::Updates,
            ) -> $crate::Result<()> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.write(structure, reader, writer, updates),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn extract_xmp<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                reader: &mut R,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.extract_xmp(structure, reader),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn extract_jumbf<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                reader: &mut R,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.extract_jumbf(structure, reader),
                    )*
                }
            }

            #[cfg(feature = "exif")]
            #[allow(unreachable_patterns)]
            pub(crate) fn extract_embedded_thumbnail<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                reader: &mut R,
            ) -> $crate::Result<Option<$crate::EmbeddedThumbnail>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.extract_embedded_thumbnail(structure, reader),
                    )*
                }
            }
        }

        /// Detect format from file header
        pub(crate) fn detect_format<R: std::io::Read + std::io::Seek>(
            reader: &mut R
        ) -> $crate::Result<Format> {
            use std::io::SeekFrom;

            reader.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let n = reader.read(&mut header)?;
            let header = &header[..n];

            if n < 2 {
                return Err($crate::Error::InvalidFormat("File too small".into()));
            }

            $(
                $(#[$meta])*
                if let Some(fmt) = $module::$handler::detect(header) {
                    return Ok(fmt);
                }
            )*

            Err($crate::Error::UnsupportedFormat)
        }

        /// Get handler for a format
        pub(crate) fn get_handler(format: Format) -> $crate::Result<Handler> {
            match format {
                $(
                    $(#[$meta])*
                    Format::$variant => Ok(Handler::$variant($module::$handler::new())),
                )*
            }
        }

        /// Detect format from file extension
        pub fn detect_from_extension(ext: &str) -> Option<Format> {
            let ext_lower = ext.to_lowercase();
            $(
                $(#[$meta])*
                if $module::$handler::extensions().contains(&ext_lower.as_str()) {
                    return $module::$handler::supported_formats().first().copied();
                }
            )*
            None
        }

        /// Detect format from MIME type
        pub fn detect_from_mime(mime: &str) -> Option<Format> {
            $(
                $(#[$meta])*
                if $module::$handler::mime_types().iter().any(|m| m.eq_ignore_ascii_case(mime)) {
                    return $module::$handler::supported_formats().first().copied();
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
    Jpeg => jpeg::JpegHandler,

    #[cfg(feature = "png")]
    Png => png::PngHandler,
}
