//! Container-specific I/O implementations
//!
//! Each container format (JPEG, PNG, BMFF, etc.) has an I/O implementation that knows how to
//! parse and write that specific file structure.

use crate::{error::Result, structure::Structure, MediaType, Updates};

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

    /// RIFF container (Resource Interchange File Format: WebP, WAV, AVI)
    #[cfg(feature = "riff")]
    Riff,

    /// PDF document (bounded metadata access and incremental revisions)
    #[cfg(feature = "pdf")]
    Pdf,
}

/// Trait for container-specific I/O operations
///
/// Each implementation handles one container format (e.g., JPEG, PNG, BMFF) and can
/// support multiple media types within that container.
#[allow(dead_code)]
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

    /// Calculate the C2PA exclusion range for a segment kind
    ///
    /// This encapsulates container-specific details about what bytes must be excluded
    /// from hashing when generating C2PA DataHash assertions. For example:
    /// - PNG: Excludes data + CRC (4 extra bytes after data)
    /// - JPEG: Excludes only the JUMBF data (headers are hashed)
    /// - BMFF: Excludes only the manifest data (box headers are hashed)
    ///
    /// Returns (offset, size) for the range to exclude from the hash, or None
    /// if the segment kind is not found.
    fn exclusion_range_for_segment(
        structure: &Structure,
        kind: crate::segment::SegmentKind,
    ) -> Option<(u64, u64)>
    where
        Self: Sized;
}

// ContainerKind I/O modules - pub(crate) so register_containers! macro can access them
#[cfg(feature = "jpeg")]
pub(crate) mod jpeg_io;

#[cfg(feature = "png")]
pub(crate) mod png_io;

#[cfg(feature = "bmff")]
pub mod bmff_io;

#[cfg(feature = "riff")]
pub(crate) mod riff_io;

#[cfg(feature = "pdf")]
pub(crate) mod pdf_io;

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
            pub(crate) async fn parse_async(
                &self,
                src: &mut $crate::sans_io::io::Src,
            ) -> $crate::Result<$crate::Structure> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.parse_async(src).await,
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) async fn write_async(
                &self,
                structure: &$crate::Structure,
                src: &mut $crate::sans_io::io::Src,
                pw: &mut $crate::sans_io::io::PDst<'_>,
                updates: &$crate::Updates,
            ) -> $crate::Result<()> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.write_async(structure, src, pw, updates).await,
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) async fn read_xmp_async(
                &self,
                structure: &$crate::Structure,
                src: &mut $crate::sans_io::io::Src,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_xmp_async(structure, src).await,
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) async fn read_jumbf_async(
                &self,
                structure: &$crate::Structure,
                src: &mut $crate::sans_io::io::Src,
            ) -> $crate::Result<Option<Vec<u8>>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_jumbf_async(structure, src).await,
                    )*
                }
            }

            #[cfg(feature = "exif")]
            #[allow(unreachable_patterns)]
            pub(crate) async fn read_embedded_thumbnail_info_async(
                &self,
                structure: &$crate::Structure,
                src: &mut $crate::sans_io::io::Src,
            ) -> $crate::Result<Option<$crate::thumbnail::EmbeddedThumbnailInfo>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_embedded_thumbnail_info_async(structure, src).await,
                    )*
                }
            }

            #[cfg(feature = "exif")]
            #[allow(unreachable_patterns)]
            pub(crate) async fn read_exif_info_async(
                &self,
                structure: &$crate::Structure,
                src: &mut $crate::sans_io::io::Src,
            ) -> $crate::Result<Option<$crate::tiff::ExifInfo>> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(h) => h.read_exif_info_async(structure, src).await,
                    )*
                }
            }

            #[allow(unreachable_patterns)]
            pub(crate) fn exclusion_range_for_segment(
                &self,
                structure: &$crate::Structure,
                kind: $crate::segment::SegmentKind,
            ) -> Option<(u64, u64)> {
                match self {
                    $(
                        $(#[$meta])*
                        Handler::$variant(_) => $module::$io::exclusion_range_for_segment(structure, kind),
                    )*
                }
            }
        }

        /// Detect container from the first bytes of `src` (reads at most 16 bytes at offset 0).
        pub(crate) async fn detect_container_async(
            src: &mut $crate::sans_io::io::Src,
        ) -> $crate::Result<ContainerKind> {
            src.seek(std::io::SeekFrom::Start(0))?;
            let want = src.len().min(16) as usize;
            let mut header = [0u8; 16];
            src.read_exact_direct(&mut header[..want]).await?;
            let header = &header[..want];

            if want < 2 {
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

        // Blocking shims over the async container methods, for unit tests only.
        $(
            $(#[$meta])*
            #[cfg(test)]
            #[allow(dead_code)]
            impl $module::$io {
                pub(crate) fn parse<R: std::io::Read + std::io::Seek>(&self, source: &mut R) -> $crate::Result<$crate::Structure> {
                    let len = source.seek(std::io::SeekFrom::End(0))?;
                    let m = $crate::sans_io::Machine::new(|io| async move {
                        let mut src = $crate::sans_io::io::Src::new(io, len);
                        self.parse_async(&mut src).await
                    });
                    $crate::sans_io::drive(m, Some(source), None, 0, None)
                }

                pub(crate) fn read_xmp<R: std::io::Read + std::io::Seek>(&self, structure: &$crate::Structure, source: &mut R) -> $crate::Result<Option<Vec<u8>>> {
                    let len = source.seek(std::io::SeekFrom::End(0))?;
                    let m = $crate::sans_io::Machine::new(|io| async move {
                        let mut src = $crate::sans_io::io::Src::new(io, len);
                        self.read_xmp_async(structure, &mut src).await
                    });
                    $crate::sans_io::drive(m, Some(source), None, 0, None)
                }

                pub(crate) fn read_jumbf<R: std::io::Read + std::io::Seek>(&self, structure: &$crate::Structure, source: &mut R) -> $crate::Result<Option<Vec<u8>>> {
                    let len = source.seek(std::io::SeekFrom::End(0))?;
                    let m = $crate::sans_io::Machine::new(|io| async move {
                        let mut src = $crate::sans_io::io::Src::new(io, len);
                        self.read_jumbf_async(structure, &mut src).await
                    });
                    $crate::sans_io::drive(m, Some(source), None, 0, None)
                }

                #[cfg(feature = "exif")]
                pub(crate) fn read_exif_info<R: std::io::Read + std::io::Seek>(&self, structure: &$crate::Structure, source: &mut R) -> $crate::Result<Option<$crate::tiff::ExifInfo>> {
                    let len = source.seek(std::io::SeekFrom::End(0))?;
                    let m = $crate::sans_io::Machine::new(|io| async move {
                        let mut src = $crate::sans_io::io::Src::new(io, len);
                        self.read_exif_info_async(structure, &mut src).await
                    });
                    $crate::sans_io::drive(m, Some(source), None, 0, None)
                }

                #[cfg(feature = "exif")]
                pub(crate) fn read_embedded_thumbnail_info<R: std::io::Read + std::io::Seek>(&self, structure: &$crate::Structure, source: &mut R) -> $crate::Result<Option<$crate::thumbnail::EmbeddedThumbnailInfo>> {
                    let len = source.seek(std::io::SeekFrom::End(0))?;
                    let m = $crate::sans_io::Machine::new(|io| async move {
                        let mut src = $crate::sans_io::io::Src::new(io, len);
                        self.read_embedded_thumbnail_info_async(structure, &mut src).await
                    });
                    $crate::sans_io::drive(m, Some(source), None, 0, None)
                }

                pub(crate) fn write<R: std::io::Read + std::io::Seek, W: std::io::Write + std::io::Seek>(&self, structure: &$crate::Structure, source: &mut R, writer: &mut W, updates: &$crate::Updates) -> $crate::Result<()> {
                    self.write_inner(structure, source, writer, updates, None)
                }

                pub(crate) fn write_with_processor<R: std::io::Read + std::io::Seek, W: std::io::Write + std::io::Seek, F: $crate::ProcessChunkFn>(&self, structure: &$crate::Structure, source: &mut R, writer: &mut W, updates: &$crate::Updates, processor: &mut F) -> $crate::Result<()> {
                    let mut p = |c: &dyn $crate::ProcessChunk| processor(c);
                    self.write_inner(structure, source, writer, updates, Some(&mut p))
                }

                fn write_inner<R: std::io::Read + std::io::Seek, W: std::io::Write + std::io::Seek>(&self, structure: &$crate::Structure, source: &mut R, writer: &mut W, updates: &$crate::Updates, processor: Option<&mut dyn FnMut(&dyn $crate::ProcessChunk) -> $crate::Result<()>>) -> $crate::Result<()> {
                    let len = source.seek(std::io::SeekFrom::End(0))?;
                    let base = writer.stream_position()?;
                    let process = processor.is_some();
                    let m = $crate::sans_io::Machine::new(|io| async move {
                        let mut src = $crate::sans_io::io::Src::new(io.clone(), len);
                        let mut dst = $crate::sans_io::io::Dst::new(io);
                        let mut pw = $crate::sans_io::io::PDst::new(&mut dst, process);
                        self.write_async(structure, &mut src, &mut pw, updates).await?;
                        pw.flush().await
                    });
                    $crate::sans_io::drive(m, Some(source), Some(writer), base, processor)
                }
            }
        )*

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
        #[allow(dead_code)]
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
        #[allow(dead_code)]
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

    #[cfg(feature = "riff")]
    Riff => riff_io::RiffIO,

    #[cfg(feature = "pdf")]
    Pdf => pdf_io::PdfIO,
}
