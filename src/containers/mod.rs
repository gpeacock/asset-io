//! Container-specific I/O implementations
//!
//! Each container format (JPEG, PNG, BMFF, etc.) has an I/O implementation that knows how to
//! parse and write that specific file structure.

use crate::{error::Result, structure::Structure, MediaType, Updates};
use std::io::{Read, Seek, Write};

/// Container format - defines how a file is structured on disk
///
/// A container format determines the parsing strategy and file structure.
/// Multiple media types can share the same container (e.g., BMFF container
/// holds HEIC, AVIF, MP4, MOV, etc.).
///
/// Note: The actual variants are determined by enabled features.
/// Use `"Kind"` suffix to distinguish this from content-type concepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    /// JPEG container (JFIF/Exif structure)
    #[cfg(feature = "jpeg")]
    Jpeg,

    /// PNG container (chunk-based structure)
    #[cfg(feature = "png")]
    Png,

    /// BMFF container (ISO Base Media File Format: HEIC, HEIF, AVIF, MP4, MOV)
    #[cfg(feature = "bmff")]
    Bmff,
}

/// Trait for container-specific I/O operations
///
/// Each implementation handles one container format (e.g., JPEG, PNG, BMFF) and can
/// support multiple media types within that container.
pub trait ContainerIO: Send + Sync {
    /// ContainerKind type this I/O implementation manages
    fn container_type() -> ContainerKind
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
    /// Returns Some(ContainerKind) if confident, None if unsure
    fn detect(header: &[u8]) -> Option<ContainerKind>
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
    /// 2. Controlling exclude mode based on `exclude_segments` and `exclusion_mode`
    ///
    /// # Exclusion Modes
    ///
    /// - [`ExclusionMode::EntireSegment`]: Exclude entire segment including headers
    /// - [`ExclusionMode::DataOnly`]: Exclude only data, include headers in processing
    ///   (required for C2PA compliance)
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
    /// * `updates` - Metadata updates to apply (includes processing options for exclusions)
    /// * `processor` - Callback function that processes each data chunk
    ///
    /// # Processing Options
    ///
    /// The `updates.processing` field controls processing behavior:
    /// - `exclude_segments` - Segment kinds to exclude from processing
    /// - `exclusion_mode` - How to handle excluded segments (DataOnly vs EntireSegment)
    /// - `chunk_size` - Buffer size for streaming (optional)
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
    ///     processor: F,
    /// ) -> Result<()> {
    ///     use crate::processing_writer::ProcessingWriter;
    ///     
    ///     let exclude_segments = &updates.processing.exclude_segments;
    ///     let exclusion_mode = updates.processing.exclusion_mode;
    ///     
    ///     let mut pw = ProcessingWriter::new(writer, processor);
    ///     
    ///     // For C2PA (DataOnly mode): only exclude the data portion
    ///     if exclude_segments.contains(&SegmentKind::Jumbf) {
    ///         if exclusion_mode == ExclusionMode::DataOnly {
    ///             pw.set_exclude_mode(true);
    ///             self.write_jumbf_data(&mut pw, jumbf_data)?;
    ///             pw.set_exclude_mode(false);
    ///         }
    ///     }
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

    /// Read XMP data from file (container-specific)
    ///
    /// This handles container-specific details like JPEG's extended XMP
    /// with multi-segment assembly, or PNG's simple iTXt chunks.
    fn read_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Read JUMBF data from file (container-specific)
    ///
    /// This handles container-specific details like JPEG XT headers,
    /// multi-segment assembly, etc.
    fn read_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>>;

    /// Read embedded thumbnail location from container-specific metadata
    ///
    /// Some containers embed pre-rendered thumbnails in their metadata:
    /// - JPEG: EXIF IFD1 thumbnail (typically ~160x120)
    /// - HEIF: 'thmb' item reference
    /// - WebP: VP8L thumbnail chunk
    /// - PNG: No embedded thumbnails (returns None)
    ///
    /// This is the fastest thumbnail path - no decoding needed!
    /// Returns location info; use Asset::read_embedded_thumbnail() to get actual bytes.
    #[cfg(feature = "exif")]
    fn read_embedded_thumbnail_info<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>>;

    /// Read EXIF metadata as parsed info (container-specific)
    ///
    /// Handles container-specific EXIF storage formats:
    /// - JPEG: APP1 segment with "Exif\0\0" prefix
    /// - HEIF/HEIC: Exif item in meta box with 4-byte offset prefix
    /// - PNG: eXIf chunk (raw TIFF data)
    ///
    /// Returns parsed EXIF info (Make, Model, DateTime, etc.) or None if not present.
    #[cfg(feature = "exif")]
    fn read_exif_info<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<crate::tiff::ExifInfo>>;
}

// ContainerKind I/O modules - pub(crate) so register_containers! macro can access them
#[cfg(feature = "jpeg")]
pub(crate) mod jpeg_io;

#[cfg(feature = "png")]
pub(crate) mod png_io;

#[cfg(feature = "bmff")]
pub(crate) mod bmff_io;

// ============================================================================
// ContainerKind Registration Macro
// ============================================================================

/// Register all supported container formats in one place
///
/// This macro generates:
/// - Handler enum for internal use (zero-cost dispatch)
/// - Handler implementation with container delegation
/// - detect_container() function
/// - get_handler() function  
/// - Extension and MIME type lookup
/// - ContainerKind methods for MIME types and extensions
///
/// Note: ContainerKind enum is defined separately above to avoid circular dependencies
macro_rules! register_containers {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $module:ident :: $io:ident
    ),* $(,)?) => {
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
                processor: F,
            ) -> $crate::Result<()> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.write_with_processor(structure, source, writer, updates, processor),
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
            pub(crate) fn read_xmp<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_xmp(structure, source),
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn read_jumbf<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_jumbf(structure, source),
                    )*
                }
            }

            #[cfg(feature = "exif")]
            #[allow(unreachable_patterns)]
            pub(crate) fn read_embedded_thumbnail_info<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<$crate::thumbnail::EmbeddedThumbnailInfo>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_embedded_thumbnail_info(structure, source),
                    )*
                }
            }

            #[cfg(feature = "exif")]
            #[allow(unreachable_patterns)]
            pub(crate) fn read_exif_info<R: std::io::Read + std::io::Seek>(
                &self,
                structure: &$crate::Structure,
                source: &mut R,
            ) -> $crate::Result<Option<$crate::tiff::ExifInfo>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_exif_info(structure, source),
                    )*
                }
            }
        }

        /// Detect container from file header
        pub(crate) fn detect_container<R: std::io::Read + std::io::Seek>(
            source: &mut R
        ) -> $crate::Result<ContainerKind> {
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
        pub(crate) fn get_handler(container: ContainerKind) -> $crate::Result<Handler> {
            match container {
                $(
                    $(#[$meta])*
                    ContainerKind::$variant => Ok(Handler::$variant($module::$io::new())),
                )*
            }
        }

        /// Detect container from file extension
        pub fn detect_from_extension(ext: &str) -> Option<ContainerKind> {
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
        pub fn detect_from_mime(mime: &str) -> Option<ContainerKind> {
            $(
                $(#[$meta])*
                if $module::$io::mime_types().iter().any(|m| m.eq_ignore_ascii_case(mime)) {
                    return Some($module::$io::container_type());
                }
            )*
            None
        }

        // Generate ContainerKind methods
        impl ContainerKind {
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
                        ContainerKind::$variant => $module::$io::supported_media_types(),
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
                        ContainerKind::$variant => $module::$io::mime_types(),
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
                        ContainerKind::$variant => $module::$io::extensions(),
                    )*
                }
            }
        }

        impl std::fmt::Display for ContainerKind {
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
