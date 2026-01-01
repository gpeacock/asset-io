//! Container-specific I/O implementations
//!
//! Each container format (JPEG, PNG, BMFF, etc.) has an I/O implementation that knows how to
//! parse and write that specific file structure.

use crate::{error::Result, structure::Structure, MediaType, Updates};
use std::io::{Read, Seek, Write};

/// Trait for container-specific I/O operations
///
/// Each implementation handles one container format (e.g., JPEG, PNG, BMFF) and can
/// support multiple media types within that container.
pub trait ContainerIO: Send + Sync {
    /// Container type this I/O implementation manages
    ///
    /// Note: Container is defined in the register_containers! macro
    fn container_type() -> crate::Container
    where
        Self: Sized;

    /// Media types this I/O implementation can read/write
    fn supported_media_types() -> &'static [MediaType]
    where
        Self: Sized;

    /// File extensions this I/O implementation accepts (e.g., ["jpg", "jpeg"])
    fn extensions() -> &'static [&'static str]
    where
        Self: Sized;

    /// MIME types this I/O implementation accepts
    fn mime_types() -> &'static [&'static str]
    where
        Self: Sized;

    /// Try to detect if this I/O implementation can parse the given header
    /// Returns Some(Container) if confident, None if unsure
    fn detect(header: &[u8]) -> Option<crate::Container>
    where
        Self: Sized;

    /// Parse file structure in single pass
    ///
    /// This discovers all segments, XMP, and JUMBF locations without
    /// loading the actual data into memory. It also determines the specific
    /// media type within the container.
    fn parse<R: Read + Seek>(&self, source: &mut R) -> Result<Structure>;

    /// Write file with updates in single streaming pass
    ///
    /// This streams from the source to destination, applying updates
    /// without loading the entire file into memory.
    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()>;

    /// Write file with processor callback for single-pass processing
    ///
    /// This is an optimized version of `write` that allows processing data
    /// (e.g., hashing) as it's being written, without needing to re-read the output.
    ///
    /// Handlers can override this to provide true single-pass operation by:
    /// 1. Wrapping the writer in a `ProcessingWriter`
    /// 2. Controlling exclude mode based on `exclude_segments`
    ///
    /// # Default Implementation
    ///
    /// The default implementation wraps the writer in a `ProcessingWriter` and
    /// calls the regular `write` method. This provides some benefit (no re-read)
    /// but cannot intelligently exclude specific segments. Handlers should override
    /// this for optimal performance.
    ///
    /// # Arguments
    ///
    /// * `structure` - Source file structure
    /// * `source` - Source data reader
    /// * `writer` - Destination writer
    /// * `updates` - Metadata updates to apply
    /// * `exclude_segments` - Segment kinds to exclude from processing
    /// * `processor` - Callback function that processes each data chunk
    ///
    /// # Example Handler Override
    ///
    /// ```rust,ignore
    /// fn write_with_processor<R: Read + Seek, W: Write, F: FnMut(&[u8])>(
    ///     &self,
    ///     structure: &Structure,
    ///     source: &mut R,
    ///     writer: &mut W,
    ///     updates: &Updates,
    ///     exclude_segments: &[SegmentKind],
    ///     processor: F,
    /// ) -> Result<()> {
    ///     use crate::processing_writer::ProcessingWriter;
    ///     
    ///     let mut pw = ProcessingWriter::new(writer, processor);
    ///     
    ///     // ... write logic ...
    ///     
    ///     // Before writing excluded segment
    ///     if exclude_segments.contains(&SegmentKind::Jumbf) {
    ///         pw.set_exclude_mode(true);
    ///     }
    ///     self.write_jumbf(&mut pw, jumbf_data)?;
    ///     pw.set_exclude_mode(false);
    ///     
    ///     Ok(())
    /// }
    /// ```
    fn write_with_processor<R: Read + Seek, W: Write, F: FnMut(&[u8])>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
        _exclude_segments: &[crate::segment::SegmentKind],
        processor: F,
    ) -> Result<()> {
        use crate::processing_writer::ProcessingWriter;
        
        // Default implementation: wrap writer and process everything
        // This doesn't intelligently exclude segments, but handlers can override
        let mut processing_writer = ProcessingWriter::new(writer, processor);
        self.write(structure, source, &mut processing_writer, updates)?;
        Ok(())
    }

    /// Calculate the structure that would result from applying updates
    ///
    /// This computes the destination file's structure (segment locations, offsets)
    /// WITHOUT actually writing the file. This enables:
    /// - Pre-calculating offsets for C2PA data hashing
    /// - Validating updates before writing
    /// - VirtualAsset workflow (hash before writing)
    ///
    /// The returned Structure should match what `write()` would produce.
    fn calculate_updated_structure(
        &self,
        source_structure: &Structure,
        updates: &Updates,
    ) -> Result<Structure>;

    /// Extract XMP data from file (container-specific)
    ///
    /// This handles container-specific details like JPEG's extended XMP
    /// with multi-segment assembly, or PNG's simple iTXt chunks.
    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Extract JUMBF data from file (container-specific)
    ///
    /// This handles container-specific details like JPEG XT headers,
    /// multi-segment assembly, etc.
    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Extract embedded thumbnail from container-specific metadata
    ///
    /// Some containers embed pre-rendered thumbnails in their metadata:
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
        source: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>>;
}

// Container I/O modules - pub(crate) so register_containers! macro can access them
#[cfg(feature = "jpeg")]
pub(crate) mod jpeg_io;

#[cfg(feature = "png")]
pub(crate) mod png_io;

#[cfg(feature = "bmff")]
pub(crate) mod bmff_io;

// ============================================================================
// Container Registration Macro
// ============================================================================

/// Register all supported container formats in one place
///
/// This macro generates:
/// - Container enum with variants
/// - Handler enum for internal use (zero-cost dispatch)
/// - Handler implementation with container delegation
/// - detect_container() function
/// - get_handler() function  
/// - Extension and MIME type lookup
/// - Container methods for MIME types and extensions
macro_rules! register_containers {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $module:ident :: $io:ident
    ),* $(,)?) => {
        /// Container format - defines how a file is structured on disk
        ///
        /// A container format determines the parsing strategy and file structure.
        /// Multiple media types can share the same container (e.g., BMFF container
        /// holds HEIC, AVIF, MP4, MOV, etc.).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Container {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        // Generate Handler enum for internal use (zero-cost dispatch)
        pub(crate) enum Handler {
            $(
                $(#[$meta])*
                $variant($module::$io),
            )*
        }

        // Generate Handler implementation - delegates to specific I/O implementations
        impl Handler {
            #[allow(unreachable_patterns)]
            pub(crate) fn parse<R: std::io::Read + std::io::Seek>(&self, source: &mut R) -> $crate::Result<$crate::Structure> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.parse(source),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn write<R: std::io::Read + std::io::Seek, W: std::io::Write>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
                writer: &mut W,
                updates: &$crate::Updates,
            ) -> $crate::Result<()> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.write(structure, source, writer, updates),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn write_with_processor<R: std::io::Read + std::io::Seek, W: std::io::Write, F: FnMut(&[u8])>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
                writer: &mut W,
                updates: &$crate::Updates,
                exclude_segments: &[$crate::segment::SegmentKind],
                processor: F,
            ) -> $crate::Result<()> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.write_with_processor(structure, source, writer, updates, exclude_segments, processor),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn calculate_updated_structure(
                &self,
                source_structure: &$crate::Structure,
                updates: &$crate::Updates,
            ) -> $crate::Result<$crate::Structure> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.calculate_updated_structure(source_structure, updates),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn extract_xmp<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.extract_xmp(structure, source),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn extract_jumbf<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.extract_jumbf(structure, source),
                    )*
                }
            }

            #[cfg(feature = "exif")]
            #[allow(unreachable_patterns)]
            pub(crate) fn extract_embedded_thumbnail<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<$crate::EmbeddedThumbnail>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.extract_embedded_thumbnail(structure, source),
                    )*
                }
            }
        }

        /// Detect container from file header
        pub(crate) fn detect_container<R: std::io::Read + std::io::Seek>(
            source: &mut R
        ) -> $crate::Result<Container> {
            use std::io::SeekFrom;

            source.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let n = source.read(&mut header)?;
            let header = &header[..n];

            if n < 2 {
                return Err($crate::Error::InvalidFormat("File too small".into()));
            }

            $(
                $(#[$meta])*
                if let Some(container) = $module::$io::detect(header) {
                    return Ok(container);
                }
            )*

            Err($crate::Error::UnsupportedFormat)
        }

        /// Get handler for a container
        pub(crate) fn get_handler(container: Container) -> $crate::Result<Handler> {
            match container {
                $(
                    $(#[$meta])*
                    Container::$variant => Ok(Handler::$variant($module::$io::new())),
                )*
            }
        }

        /// Detect container from file extension
        pub fn detect_from_extension(ext: &str) -> Option<Container> {
            let ext_lower = ext.to_lowercase();
            $(
                $(#[$meta])*
                if $module::$io::extensions().contains(&ext_lower.as_str()) {
                    return Some($module::$io::container_type());
                }
            )*
            None
        }

        /// Detect container from MIME type
        pub fn detect_from_mime(mime: &str) -> Option<Container> {
            $(
                $(#[$meta])*
                if $module::$io::mime_types().iter().any(|m| m.eq_ignore_ascii_case(mime)) {
                    return Some($module::$io::container_type());
                }
            )*
            None
        }

        // Generate Container methods
        impl Container {
            /// Get the primary MIME type for this container
            ///
            /// Returns the most common/primary MIME type for this container.
            pub fn to_mime(&self) -> &'static str {
                self.mime_types()[0]
            }

            /// Get the primary file extension for this container
            ///
            /// Returns the most common file extension (without dot prefix).
            pub fn to_extension(&self) -> &'static str {
                self.extensions()[0]
            }

            /// Get all supported media types for this container
            ///
            /// Returns the media types that can be stored in this container format.
            pub fn supported_media_types(&self) -> &'static [$crate::MediaType] {
                match self {
                    $(
                        $(#[$meta])*
                        Container::$variant => $module::$io::supported_media_types(),
                    )*
                }
            }

            /// Get all supported MIME types for this container
            ///
            /// Returns all MIME types that can be detected/written for this container.
            pub fn mime_types(&self) -> &'static [&'static str] {
                match self {
                    $(
                        $(#[$meta])*
                        Container::$variant => $module::$io::mime_types(),
                    )*
                }
            }

            /// Get all supported file extensions for this container
            ///
            /// Returns all file extensions (without dot prefix) for this container.
            pub fn extensions(&self) -> &'static [&'static str] {
                match self {
                    $(
                        $(#[$meta])*
                        Container::$variant => $module::$io::extensions(),
                    )*
                }
            }
        }

        impl std::fmt::Display for Container {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.to_mime())
            }
        }
    };
}

// ============================================================================
// SINGLE POINT OF REGISTRATION
// To add a new container, just add one line here!
// ============================================================================
register_containers! {
    #[cfg(feature = "jpeg")]
    Jpeg => jpeg_io::JpegIO,

    #[cfg(feature = "png")]
    Png => png_io::PngIO,

    #[cfg(feature = "bmff")]
    Bmff => bmff_io::BmffIO,
}
