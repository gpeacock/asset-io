//! File structure representation

#[cfg(feature = "thumbnails")]
use crate::thumbnail::EmbeddedThumbnail;
use crate::{
    error::Result,
    segment::{ByteRange, ChunkedSegmentReader, Location, Segment},
    Format,
};
use std::io::{Read, Seek, SeekFrom, Take};

/// Represents the discovered structure of a file
#[derive(Debug)]
pub struct FileStructure {
    /// All segments in the file
    pub segments: Vec<Segment>,

    /// File format
    pub format: Format,

    /// Total file size
    pub total_size: u64,

    /// Quick lookup: index of XMP segment (if any)
    xmp_index: Option<usize>,

    /// Quick lookup: indices of JUMBF segments
    jumbf_indices: Vec<usize>,

    /// Memory-mapped file data (optional, for zero-copy access)
    #[cfg(feature = "memory-mapped")]
    mmap: Option<std::sync::Arc<memmap2::Mmap>>,
}

impl FileStructure {
    /// Create a new file structure
    pub fn new(format: Format) -> Self {
        Self {
            segments: Vec::new(),
            format,
            total_size: 0,
            xmp_index: None,
            jumbf_indices: Vec::new(),
            #[cfg(feature = "memory-mapped")]
            mmap: None,
        }
    }

    /// Attach memory-mapped data to this structure (zero-copy access)
    #[cfg(feature = "memory-mapped")]
    pub fn with_mmap(mut self, mmap: memmap2::Mmap) -> Self {
        self.mmap = Some(std::sync::Arc::new(mmap));
        self
    }

    /// Get a slice of data from memory-mapped file (zero-copy)
    ///
    /// Returns None if:
    /// - No memory map is attached
    /// - The range is out of bounds
    /// - Integer overflow would occur
    #[cfg(feature = "memory-mapped")]
    pub fn get_mmap_slice(&self, range: ByteRange) -> Option<&[u8]> {
        self.mmap.as_ref().and_then(|mmap| {
            let start = range.offset as usize;
            let end = start.checked_add(range.size as usize)?;
            mmap.get(start..end) // Returns None instead of panicking
        })
    }

    /// Add a segment and update indices
    pub fn add_segment(&mut self, segment: Segment) {
        let index = self.segments.len();

        match &segment {
            Segment::Xmp { .. } => {
                self.xmp_index = Some(index);
            }
            Segment::Jumbf { .. } => {
                self.jumbf_indices.push(index);
            }
            _ => {}
        }

        self.segments.push(segment);
    }

    /// Get the XMP index (if any) - for internal use during parsing
    pub(crate) fn xmp_index_mut(&mut self) -> &mut Option<usize> {
        &mut self.xmp_index
    }

    /// Get reference to XMP index (for format-specific handlers)
    pub fn xmp_index(&self) -> Option<usize> {
        self.xmp_index
    }

    /// Get reference to jumbf indices (for format-specific handlers)
    pub fn jumbf_indices(&self) -> &[usize] {
        &self.jumbf_indices
    }

    /// Get reference to segments (for format-specific handlers)
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Get XMP data (loads lazily if needed, assembles extended parts if present)
    /// Get XMP metadata (loads lazily, assembles extended parts if present)
    ///
    /// This method delegates to format-specific handlers for extraction,
    /// as each format has its own conventions (e.g., JPEG Extended XMP, PNG iTXt chunks).
    pub fn xmp<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
        if self.xmp_index.is_none() {
            return Ok(None);
        }

        // Delegate to format-specific XMP extraction
        match self.format {
            #[cfg(feature = "jpeg")]
            crate::Format::Jpeg => {
                use crate::formats::jpeg::JpegHandler;
                JpegHandler::extract_xmp_impl(self, reader)
            }
            #[cfg(feature = "png")]
            crate::Format::Png => {
                use crate::formats::png::PngHandler;
                PngHandler::extract_xmp_impl(self, reader)
            }
        }
    }

    /// Get JUMBF data (loads and assembles from multiple segments if needed)
    ///
    /// This method delegates to format-specific handlers for extraction,
    /// as each format has its own conventions (e.g., JPEG XT headers, PNG caBX chunks).
    pub fn jumbf<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
        if self.jumbf_indices.is_empty() {
            return Ok(None);
        }

        // Delegate to format-specific JUMBF extraction
        match self.format {
            #[cfg(feature = "jpeg")]
            crate::Format::Jpeg => {
                use crate::formats::jpeg::JpegHandler;
                JpegHandler::extract_jumbf_impl(self, reader)
            }
            #[cfg(feature = "png")]
            crate::Format::Png => {
                use crate::formats::png::PngHandler;
                PngHandler::extract_jumbf_impl(self, reader)
            }
        }
    }

    /// Calculate hash of specified segments without loading entire file
    #[cfg(feature = "hashing")]
    pub fn calculate_hash<R: Read + Seek, H: std::io::Write>(
        &self,
        reader: &mut R,
        segment_indices: &[usize],
        hasher: &mut H,
    ) -> Result<()> {
        for &index in segment_indices {
            let segment = &self.segments[index];
            let location = segment.location();

            reader.seek(SeekFrom::Start(location.offset))?;

            // Stream through segment in chunks
            let mut remaining = location.size;
            let mut buffer = vec![0u8; 8192];

            while remaining > 0 {
                let to_read = remaining.min(buffer.len() as u64) as usize;
                reader.read_exact(&mut buffer[..to_read])?;
                hasher.write_all(&buffer[..to_read])?;
                remaining -= to_read as u64;
            }
        }

        Ok(())
    }

    // ============================================================================
    // Range-based Data Access API (for C2PA hashing)
    // ============================================================================

    /// Read a specific byte range from the file
    ///
    /// This is useful for data hash models that need to hash arbitrary ranges.
    pub fn read_range<R: Read + Seek>(&self, reader: &mut R, range: ByteRange) -> Result<Vec<u8>> {
        reader.seek(SeekFrom::Start(range.offset))?;
        let mut buffer = vec![0u8; range.size as usize];
        reader.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    /// Create a chunked reader for a byte range
    ///
    /// This allows streaming through a range without loading it all into memory.
    /// Useful for hashing large ranges efficiently.
    pub fn read_range_chunked<'a, R: Read + Seek>(
        &self,
        reader: &'a mut R,
        range: ByteRange,
        chunk_size: usize,
    ) -> Result<ChunkedSegmentReader<Take<&'a mut R>>> {
        reader.seek(SeekFrom::Start(range.offset))?;
        let taken = reader.take(range.size);
        Ok(ChunkedSegmentReader::new(taken, range.size, chunk_size))
    }

    /// Get byte ranges for all segments EXCEPT those matching the exclusion patterns
    ///
    /// This is useful for C2PA data hash which needs to hash everything except
    /// the C2PA segment itself.
    ///
    /// # Example
    /// ```no_run
    /// # use asset_io::*;
    /// # fn example(structure: &FileStructure) -> Result<()> {
    /// // Hash everything except JUMBF segments
    /// let ranges = structure.hashable_ranges(&["jumbf"]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn hashable_ranges(&self, exclusions: &[&str]) -> Vec<ByteRange> {
        let mut ranges = Vec::new();
        let mut last_end = 0u64;

        for segment in &self.segments {
            let should_exclude = exclusions
                .iter()
                .any(|pattern| segment.path().contains(pattern));

            if should_exclude {
                let loc = segment.location();
                // Add range before this excluded segment
                if last_end < loc.offset {
                    ranges.push(ByteRange {
                        offset: last_end,
                        size: loc.offset - last_end,
                    });
                }
                last_end = loc.offset + loc.size;
            }
        }

        // Add final range to end of file
        if last_end < self.total_size {
            ranges.push(ByteRange {
                offset: last_end,
                size: self.total_size - last_end,
            });
        }

        ranges
    }

    /// Get segments matching a path pattern
    ///
    /// Useful for box-based hashing where specific segments are hashed by name.
    pub fn segments_by_path(&self, pattern: &str) -> Vec<(usize, &Segment)> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, seg)| seg.path().contains(pattern))
            .collect()
    }

    /// Get all segments except those matching exclusion patterns
    ///
    /// Returns (index, segment, location) for each segment to be hashed.
    pub fn segments_excluding(&self, exclusions: &[&str]) -> Vec<(usize, &Segment, Location)> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, seg)| {
                !exclusions
                    .iter()
                    .any(|pattern| seg.path().contains(pattern))
            })
            .map(|(i, seg)| (i, seg, seg.location()))
            .collect()
    }

    /// Create a chunked reader for a specific segment
    ///
    /// This allows streaming through segment data without loading it all into memory.
    pub fn read_segment_chunked<'a, R: Read + Seek>(
        &self,
        reader: &'a mut R,
        segment_index: usize,
        chunk_size: usize,
    ) -> Result<ChunkedSegmentReader<Take<&'a mut R>>> {
        let segment = &self.segments[segment_index];
        let loc = segment.location();
        reader.seek(SeekFrom::Start(loc.offset))?;
        let taken = reader.take(loc.size);
        Ok(ChunkedSegmentReader::new(taken, loc.size, chunk_size))
    }

    // ========================================================================
    // Thumbnail Generation Support
    // ========================================================================

    /// Get the byte range of the main image data
    ///
    /// This returns the location of the compressed image data in the file,
    /// which can be used for efficient thumbnail generation. The data can be:
    /// - Accessed via memory-mapping (zero-copy with `get_mmap_slice`)
    /// - Streamed in chunks (constant memory with `stream_image_data`)
    /// - Read all at once (for small images)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let range = structure.image_data_range().unwrap();
    ///
    /// // Zero-copy with memory mapping
    /// if let Some(slice) = structure.get_mmap_slice(range) {
    ///     // Pass directly to decoder
    ///     let thumbnail = decoder.decode_and_thumbnail(slice)?;
    /// }
    /// ```
    pub fn image_data_range(&self) -> Option<ByteRange> {
        self.segments.iter().find_map(|seg| match seg {
            Segment::ImageData { offset, size, .. } => Some(ByteRange {
                offset: *offset,
                size: *size,
            }),
            _ => None,
        })
    }

    /// Try to extract an embedded thumbnail from the file
    ///
    /// Many image formats include pre-rendered thumbnails for quick preview:
    /// - JPEG: EXIF thumbnail (typically 160x120)
    /// - HEIF/HEIC: 'thmb' item reference  
    /// - WebP: VP8L thumbnail chunk
    /// - TIFF: IFD0 thumbnail
    /// - PNG: No embedded thumbnails
    ///
    /// This is the fastest way to get a thumbnail if available.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Try embedded thumbnail first (fastest!)
    /// if let Some(thumb) = structure.embedded_thumbnail()? {
    ///     if thumb.fits(256, 256) {
    ///         return Ok(thumb.data);  // Perfect!
    ///     }
    /// }
    /// // Fall back to decoding main image
    /// ```
    #[cfg(feature = "thumbnails")]
    pub fn embedded_thumbnail(&self) -> Result<Option<EmbeddedThumbnail>> {
        // Delegate to format-specific extraction
        match self.format {
            #[cfg(feature = "jpeg")]
            Format::Jpeg => self.extract_jpeg_thumbnail(),
            #[cfg(feature = "png")]
            Format::Png => {
                // PNG doesn't have embedded thumbnails in metadata
                Ok(None)
            }
        }
    }

    /// Extract EXIF thumbnail from JPEG (if present)
    #[cfg(all(feature = "thumbnails", feature = "jpeg"))]
    fn extract_jpeg_thumbnail(&self) -> Result<Option<EmbeddedThumbnail>> {
        // Find EXIF segment
        for segment in &self.segments {
            if let Segment::Exif { thumbnail, .. } = segment {
                return Ok(thumbnail.clone());
            }
        }
        Ok(None)
    }
}
