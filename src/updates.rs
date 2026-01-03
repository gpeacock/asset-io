//! Updates and processing options for asset modifications

use crate::segment::{ExclusionMode, SegmentKind, DEFAULT_CHUNK_SIZE};

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
