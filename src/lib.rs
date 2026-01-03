//! High-performance, media type-agnostic streaming I/O for media asset metadata.
//!
//! This crate provides efficient, single-pass parsing and writing of media files
//! with JUMBF (JPEG Universal Metadata Box Format) and XMP metadata.
//!
//! # Design Principles
//!
//! - **Streaming**: Process files without loading them entirely into memory
//! - **Lazy loading**: Only read data when explicitly accessed
//! - **Zero-copy**: Use memory-mapped files when beneficial
//! - **Media type agnostic**: Unified API across JPEG, PNG, MP4, and more
//!
//! # Quick Start (Media Type-Agnostic API)
//!
//! The simplest way to use this library is with the [`Asset`] API,
//! which automatically detects the media type:
//!
//! ```no_run
//! use asset_io::{Asset, Updates};
//!
//! # fn main() -> asset_io::Result<()> {
//! // Open any supported file - media type is auto-detected
//! let mut asset = Asset::open("image.jpg")?;
//!
//! // Read metadata
//! if let Some(xmp) = asset.xmp()? {
//!     println!("XMP: {} bytes", xmp.len());
//! }
//! if let Some(jumbf) = asset.jumbf()? {
//!     println!("JUMBF: {} bytes", jumbf.len());
//! }
//!
//! // Modify and write using builder pattern
//! let updates = Updates::new()
//!     .set_xmp(b"<new>metadata</new>".to_vec())
//!     .remove_jumbf();
//! asset.write_to("output.jpg", &updates)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Handler-Specific API
//!
//! For more control, you can use media type-specific handlers:
//!
//! ```no_run
//! use asset_io::{ContainerIO, JpegIO, Updates};
//! use std::fs::File;
//!
//! # fn main() -> asset_io::Result<()> {
//! // Parse file structure in single pass
//! let mut file = File::open("image.jpg")?;
//! let handler = JpegIO::new();
//! let structure = handler.parse(&mut file)?;
//!
//! // Access XMP data (loaded lazily via handler)
//! if let Some(xmp) = handler.read_xmp(&structure, &mut file)? {
//!     println!("Found XMP: {} bytes", xmp.len());
//! }
//!
//! // Write with updates
//! let updates = Updates::default();
//! let mut output = File::create("output.jpg")?;
//! handler.write(&structure, &mut file, &mut output, &updates)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Processing During Read/Write
//!
//! For C2PA workflows and similar use cases, you can process data (e.g., hash it)
//! while reading or writing:
//!
//! ```no_run
//! use asset_io::{Asset, Updates, SegmentKind, ExclusionMode};
//! use sha2::{Sha256, Digest};
//!
//! # fn main() -> asset_io::Result<()> {
//! let mut asset = Asset::open("signed.jpg")?;
//!
//! // Hash while reading, excluding specific segments
//! let updates = Updates::new()
//!     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
//!
//! let mut hasher = Sha256::new();
//! asset.read_with_processing(&updates, &mut |chunk| hasher.update(chunk))?;
//! let hash = hasher.finalize();
//! println!("Asset hash: {:x}", hash);
//!
//! // Or hash while writing (single-pass workflow)
//! let mut output = std::fs::File::create("output.jpg")?;
//! let updates = Updates::new()
//!     .set_jumbf(b"placeholder".to_vec())
//!     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
//!
//! let mut hasher = Sha256::new();
//! let dest_structure = asset.write_with_processing(
//!     &mut output,
//!     &updates,
//!     &mut |chunk| hasher.update(chunk),
//! )?;
//! // Now you have the hash and can update the JUMBF in-place
//! # Ok(())
//! # }
//! ```

mod asset;
mod error;
mod containers;
mod media_type;
mod processing_writer;
mod segment;
mod structure;
mod thumbnail;
#[cfg(feature = "exif")]
mod tiff;
#[cfg(feature = "xmp")]
mod xmp;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;

pub use asset::{Asset, AssetBuilder};
pub use error::{Error, Result};
pub use segment::{ByteRange, ExclusionMode, Segment, SegmentKind};
pub use structure::Structure;
pub use thumbnail::{Thumbnail, ThumbnailKind};
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;

// Internal re-exports
pub(crate) use containers::ContainerIO;
pub(crate) use media_type::MediaType;
pub(crate) use segment::{ChunkedSegmentReader, SegmentMetadata, DEFAULT_CHUNK_SIZE};

// ContainerKind and handlers are exported by the register_containers! macro below

// Test utilities - only compiled for tests or when explicitly enabled
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

/// Options controlling how data is processed during read or write operations
///
/// These options configure streaming behavior, segment exclusions, and other
/// processing parameters. Used by both `read_with_processing()` and
/// `write_with_processing()` for symmetric read/write operations.
///
/// Use the builder methods on `Updates` to configure these options.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProcessingOptions {
    /// Chunk size for streaming operations (default: DEFAULT_CHUNK_SIZE = 64KB)
    pub(crate) chunk_size: Option<usize>,

    /// Segments to exclude from processing (e.g., for hashing)
    pub(crate) exclude_segments: Vec<SegmentKind>,

    /// How to handle exclusions (default: EntireSegment)
    pub(crate) exclusion_mode: ExclusionMode,
    // Future: include_segments for explicit inclusion
}

impl ProcessingOptions {
    /// Get the effective chunk size (uses DEFAULT_CHUNK_SIZE if not set)
    pub(crate) fn effective_chunk_size(&self) -> usize {
        self.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE)
    }
}

/// Metadata update strategy (internal)
///
/// Specifies how to handle a particular type of metadata when writing an asset.
/// By default, all metadata is kept unchanged.
#[derive(Debug, Clone, Default)]
pub(crate) enum MetadataUpdate {
    /// Keep existing metadata (default)
    #[default]
    Keep,
    /// Remove existing metadata
    Remove,
    /// Replace or add metadata
    Set(Vec<u8>),
}

/// Updates to apply when writing a file
///
/// This struct uses a builder pattern where the default is to keep all existing
/// metadata unchanged. Use the builder methods to explicitly specify changes.
///
/// # Example
///
/// ```no_run
/// use asset_io::{Asset, Updates};
///
/// # fn main() -> asset_io::Result<()> {
/// let mut asset = Asset::open("image.jpg")?;
///
/// // Default: keep everything
/// let updates = Updates::new();
/// asset.write_to("output1.jpg", &updates)?;
///
/// // Remove XMP, keep everything else
/// let updates = Updates::new().remove_xmp();
/// asset.write_to("output2.jpg", &updates)?;
///
/// // Set new JUMBF, remove XMP, keep everything else
/// let updates = Updates::new()
///     .set_jumbf(b"new jumbf data".to_vec())
///     .remove_xmp();
/// asset.write_to("output3.jpg", &updates)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct Updates {
    /// XMP data update strategy (use builder methods to modify)
    pub(crate) xmp: MetadataUpdate,

    /// JUMBF data update strategy (use builder methods to modify)
    pub(crate) jumbf: MetadataUpdate,

    /// Processing options (chunk size, exclusions, etc.)
    /// Used by both read_with_processing() and write_with_processing()
    pub(crate) processing: ProcessingOptions,
}

impl Updates {
    /// Create a new `Updates` builder with all metadata set to keep (no changes)
    ///
    /// This is the same as `Updates::default()` but more explicit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set XMP metadata to a new value
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new()
    ///     .set_xmp(b"<xmp>...</xmp>".to_vec());
    /// ```
    pub fn set_xmp(mut self, xmp: Vec<u8>) -> Self {
        self.xmp = MetadataUpdate::Set(xmp);
        self
    }

    /// Remove XMP metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().remove_xmp();
    /// ```
    pub fn remove_xmp(mut self) -> Self {
        self.xmp = MetadataUpdate::Remove;
        self
    }

    /// Keep existing XMP metadata (explicit, same as default)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().keep_xmp();
    /// ```
    pub fn keep_xmp(mut self) -> Self {
        self.xmp = MetadataUpdate::Keep;
        self
    }

    /// Set JUMBF metadata to a new value
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new()
    ///     .set_jumbf(b"jumbf data".to_vec());
    /// ```
    pub fn set_jumbf(mut self, jumbf: Vec<u8>) -> Self {
        self.jumbf = MetadataUpdate::Set(jumbf);
        self
    }

    /// Remove JUMBF metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().remove_jumbf();
    /// ```
    pub fn remove_jumbf(mut self) -> Self {
        self.jumbf = MetadataUpdate::Remove;
        self
    }

    /// Keep existing JUMBF metadata (explicit, same as default)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().keep_jumbf();
    /// ```
    pub fn keep_jumbf(mut self) -> Self {
        self.jumbf = MetadataUpdate::Keep;
        self
    }

    /// Create updates that keep all existing metadata (no changes)
    ///
    /// This is an alias for `Updates::new()` or `Updates::default()`.
    pub fn keep_all() -> Self {
        Self::default()
    }

    /// Create updates that remove all metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::remove_all();
    /// ```
    pub fn remove_all() -> Self {
        Self::new().remove_xmp().remove_jumbf()
    }

    /// Create updates that set new XMP (legacy constructor)
    ///
    /// Prefer using `Updates::new().set_xmp(xmp)` for consistency with
    /// the builder pattern.
    pub fn with_xmp(xmp: Vec<u8>) -> Self {
        Self::new().set_xmp(xmp)
    }

    /// Create updates that set new JUMBF (legacy constructor)
    ///
    /// Prefer using `Updates::new().set_jumbf(jumbf)` for consistency with
    /// the builder pattern.
    pub fn with_jumbf(jumbf: Vec<u8>) -> Self {
        Self::new().set_jumbf(jumbf)
    }

    // ========================================================================
    // Processing Options Builder Methods
    // ========================================================================

    /// Set segments to exclude from processing (e.g., for C2PA hashing)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::{Updates, SegmentKind, ExclusionMode};
    ///
    /// let updates = Updates::new()
    ///     .set_jumbf(vec![0u8; 1000])
    ///     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
    /// ```
    pub fn exclude_from_processing(
        mut self,
        segments: Vec<SegmentKind>,
        mode: ExclusionMode,
    ) -> Self {
        self.processing.exclude_segments = segments;
        self.processing.exclusion_mode = mode;
        self
    }

    /// Set the chunk size for streaming operations
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().with_chunk_size(65536);
    /// ```
    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.processing.chunk_size = Some(size);
        self
    }

}

// Re-export generated items from formats module
pub(crate) use containers::{detect_container, get_handler, Handler};
pub use containers::ContainerKind;
/// Update a segment in an already-written stream using structure information
///
/// **Deprecated**: Use [`Structure::update_segment`] instead for a cleaner API.
///
/// This function is a compatibility wrapper that calls `structure.update_segment()`.
#[deprecated(since = "0.2.0", note = "Use structure.update_segment() instead")]
pub fn update_segment_with_structure<W: std::io::Write + std::io::Seek>(
    writer: &mut W,
    structure: &Structure,
    kind: SegmentKind,
    data: Vec<u8>,
) -> Result<usize> {
    structure.update_segment(writer, kind, data)
}
