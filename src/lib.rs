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
//! if let Some(xmp) = handler.extract_xmp(&structure, &mut file)? {
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
mod formats;
mod media_type;
mod processing_writer;
pub use processing_writer::ProcessingWriter;
mod segment;
mod structure;
pub mod thumbnail;
#[cfg(feature = "exif")]
mod tiff;
#[cfg(feature = "xmp")]
pub mod xmp;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;

pub use asset::{Asset, AssetBuilder};
pub use error::{Error, Result};
pub use formats::ContainerIO;
pub use media_type::MediaType;
pub use segment::{
    ByteRange, ChunkedSegmentReader, ExclusionMode, LazyData, Location, Segment, SegmentKind,
    SegmentMetadata, DEFAULT_CHUNK_SIZE, MAX_SEGMENT_SIZE,
};
pub use structure::Structure;
pub use thumbnail::{EmbeddedThumbnailInfo, Thumbnail, ThumbnailFormat};
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;

// Container and handlers are exported by the register_containers! macro below

// Test utilities - only compiled for tests or when explicitly enabled
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

/// Options controlling how data is processed during read or write operations
///
/// These options configure streaming behavior, segment exclusions, and other
/// processing parameters. Used by both `read_with_processing()` and
/// `write_with_processing()` for symmetric read/write operations.
///
/// # Example
///
/// ```no_run
/// use asset_io::{ProcessingOptions, SegmentKind, ExclusionMode};
///
/// let options = ProcessingOptions::new()
///     .exclude(vec![SegmentKind::Jumbf])
///     .exclusion_mode(ExclusionMode::DataOnly)
///     .chunk_size(65536);
/// ```
#[derive(Debug, Clone, Default)]
pub struct ProcessingOptions {
    /// Chunk size for streaming operations (default: DEFAULT_CHUNK_SIZE = 64KB)
    pub chunk_size: Option<usize>,

    /// Segments to exclude from processing (e.g., for hashing)
    pub exclude_segments: Vec<SegmentKind>,

    /// How to handle exclusions (default: EntireSegment)
    pub exclusion_mode: ExclusionMode,
    // Future: include_segments for explicit inclusion
}

impl ProcessingOptions {
    /// Create new ProcessingOptions with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the chunk size for streaming operations
    pub fn chunk_size(mut self, size: usize) -> Self {
        self.chunk_size = Some(size);
        self
    }

    /// Set segments to exclude from processing
    pub fn exclude(mut self, segments: Vec<SegmentKind>) -> Self {
        self.exclude_segments = segments;
        self
    }

    /// Set the exclusion mode (EntireSegment or DataOnly)
    pub fn exclusion_mode(mut self, mode: ExclusionMode) -> Self {
        self.exclusion_mode = mode;
        self
    }

    /// Get the effective chunk size (uses DEFAULT_CHUNK_SIZE if not set)
    pub fn effective_chunk_size(&self) -> usize {
        self.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE)
    }
}

/// Type alias for backwards compatibility
#[deprecated(since = "0.2.0", note = "Use ProcessingOptions instead")]
pub type WriteOptions = ProcessingOptions;

/// Metadata update strategy
///
/// Specifies how to handle a particular type of metadata when writing an asset.
/// By default, all metadata is kept unchanged.
#[derive(Debug, Clone, Default)]
pub enum MetadataUpdate {
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
    /// XMP data update strategy
    pub xmp: MetadataUpdate,

    /// JUMBF data update strategy
    pub jumbf: MetadataUpdate,

    /// Processing options (chunk size, exclusions, etc.)
    /// Used by both read_with_processing() and write_with_processing()
    pub processing: ProcessingOptions,
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

    /// Set custom processing options
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::{Updates, ProcessingOptions, SegmentKind, ExclusionMode};
    ///
    /// let options = ProcessingOptions::new()
    ///     .exclude(vec![SegmentKind::Jumbf])
    ///     .exclusion_mode(ExclusionMode::DataOnly);
    ///
    /// let updates = Updates::new()
    ///     .set_jumbf(vec![0u8; 1000])
    ///     .with_processing(options);
    /// ```
    pub fn with_processing(mut self, options: ProcessingOptions) -> Self {
        self.processing = options;
        self
    }
}

// Re-export generated items from formats module
pub(crate) use formats::{detect_container, get_handler, Handler};
pub use formats::{detect_from_extension, detect_from_mime, Container};

// Re-export container handlers at crate root
#[cfg(feature = "bmff")]
pub use formats::bmff_io::BmffIO;
#[cfg(feature = "jpeg")]
pub use formats::jpeg_io::JpegIO;
#[cfg(feature = "png")]
pub use formats::png_io::PngIO;
/// Update a segment in an already-written stream using structure information
///
/// This is a low-level utility for updating specific segments after a file has been
/// written but before it's closed. It's designed for use with
/// [`Asset::write_with_processing`] to enable efficient workflows like:
/// - C2PA: Write with placeholder → hash → generate manifest → update in-place
/// - XMP: Write file → calculate derived metadata → update XMP in-place
///
/// The new data must fit within the existing segment's capacity. If smaller,
/// it will be zero-padded to maintain file structure.
///
/// # Arguments
/// - `writer`: An open, seekable writer with the written file
/// - `structure`: The destination structure (returned from `write_with_processing`)
/// - `kind`: The type of segment to update
/// - `data`: The new segment data
///
/// # Returns
/// Number of bytes written (including padding)
///
/// # Errors
/// - `InvalidFormat`: Segment not found or data too large
/// - I/O errors during seek/write
///
/// # Example
/// ```no_run
/// use asset_io::{Asset, Updates, SegmentKind, ExclusionMode, update_segment_with_structure};
/// use sha2::{Sha256, Digest};
/// use std::fs::File;
///
/// # fn main() -> asset_io::Result<()> {
/// let mut asset = Asset::open("input.jpg")?;
/// let mut output = File::create("output.jpg")?;
///
/// // Write and hash with C2PA-compliant exclusions
/// let placeholder = vec![0u8; 20000];
/// let updates = Updates::new()
///     .set_jumbf(placeholder)
///     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
///
/// let mut hasher = Sha256::new();
/// let structure = asset.write_with_processing(
///     &mut output,
///     &updates,
///     &mut |chunk| hasher.update(chunk),
/// )?;
///
/// // Generate manifest and update in-place
/// let manifest = vec![/* final manifest with hash */];
/// update_segment_with_structure(&mut output, &structure, SegmentKind::Jumbf, manifest)?;
/// # Ok(())
/// # }
/// ```
pub fn update_segment_with_structure<W: std::io::Write + std::io::Seek>(
    writer: &mut W,
    structure: &Structure,
    kind: SegmentKind,
    data: Vec<u8>,
) -> Result<usize> {
    use std::io::SeekFrom;

    // PNG requires special handling for CRC recalculation
    #[cfg(feature = "png")]
    if structure.container == Container::Png {
        return formats::png_io::update_png_segment_in_stream(writer, structure, kind, data);
    }

    // Find the segment
    let segment_idx = match kind {
        SegmentKind::Jumbf => structure.c2pa_jumbf_index(),
        SegmentKind::Xmp => structure.xmp_index(),
        // EXIF not yet fully implemented in Structure
        _ => {
            return Err(Error::InvalidFormat(format!(
                "Cannot update {:?} segments",
                kind
            )))
        }
    }
    .ok_or_else(|| Error::InvalidFormat(format!("No {:?} segment found", kind)))?;

    let segment = &structure.segments[segment_idx];

    // Calculate total capacity across all ranges
    let total_capacity: u64 = segment.ranges.iter().map(|r| r.size).sum();

    // Validate size
    if data.len() as u64 > total_capacity {
        return Err(Error::InvalidFormat(format!(
            "Data ({} bytes) exceeds capacity ({} bytes)",
            data.len(),
            total_capacity
        )));
    }

    // Pad to exact capacity (preserves file structure)
    let mut padded = data;
    padded.resize(total_capacity as usize, 0);

    // For non-PNG formats, just write the data directly
    let mut offset = 0;
    for range in &segment.ranges {
        writer.seek(SeekFrom::Start(range.offset))?;
        let to_write = (padded.len() - offset).min(range.size as usize);
        writer.write_all(&padded[offset..offset + to_write])?;
        offset += to_write;
        if offset >= padded.len() {
            break;
        }
    }

    writer.flush()?;
    Ok(padded.len())
}
