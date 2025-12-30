//! Structure representation for parsed assets

use crate::{
    error::Result,
    segment::{ByteRange, ChunkedSegmentReader, Location, Segment},
    Container, MediaType,
};
use std::io::{Read, Seek, SeekFrom, Take};

/// Represents the discovered structure of a parsed asset
///
/// This structure works with any `Read + Seek` source (files, buffers, streams).
/// It contains the parsed segment layout and provides efficient access methods
/// for metadata extraction and hashing operations.
#[derive(Debug)]
pub struct Structure {
    /// All segments in the asset
    pub segments: Vec<Segment>,

    /// Container format - how the file is structured
    pub container: Container,

    /// Media type - what the content represents
    pub media_type: MediaType,

    /// Total asset size
    pub total_size: u64,

    /// Quick lookup: index of XMP segment (if any)
    xmp_index: Option<usize>,

    /// Quick lookup: indices of JUMBF segments
    jumbf_indices: Vec<usize>,

    /// Memory-mapped file data (optional, for zero-copy access)
    #[cfg(feature = "memory-mapped")]
    mmap: Option<std::sync::Arc<memmap2::Mmap>>,
}

impl Structure {
    /// Create a new structure for the given container and media type
    pub fn new(container: Container, media_type: MediaType) -> Self {
        Self {
            segments: Vec::new(),
            container,
            media_type,
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

    /// Attach a memory map to this structure (in-place, zero-copy)
    ///
    /// This is more efficient than `with_mmap()` when you already have a mutable reference.
    #[cfg(feature = "memory-mapped")]
    pub fn set_mmap(&mut self, mmap: memmap2::Mmap) {
        self.mmap = Some(std::sync::Arc::new(mmap));
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

        if segment.is_xmp() {
            self.xmp_index = Some(index);
        } else if segment.is_jumbf() {
            self.jumbf_indices.push(index);
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

    /// Get reference to JUMBF indices (for format-specific handlers)
    pub fn jumbf_indices(&self) -> &[usize] {
        &self.jumbf_indices
    }

    /// Get the first C2PA JUMBF index (most common case for C2PA workflows)
    pub fn c2pa_jumbf_index(&self) -> Option<usize> {
        // For now, return the first JUMBF segment
        // TODO: Check segment metadata to identify C2PA-specific JUMBF
        self.jumbf_indices.first().copied()
    }

    /// Get reference to segments (for format-specific handlers)
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Calculate hash of specified segments without loading entire file
    #[cfg(feature = "hashing")]
    pub fn calculate_hash<R: Read + Seek, H: std::io::Write>(
        &self,
        source: &mut R,
        segment_indices: &[usize],
        hasher: &mut H,
    ) -> Result<()> {
        for &index in segment_indices {
            let segment = &self.segments[index];
            let location = segment.location();

            source.seek(SeekFrom::Start(location.offset))?;

            // Stream through segment in chunks
            let mut remaining = location.size;
            let mut buffer = vec![0u8; 8192];

            while remaining > 0 {
                let to_read = remaining.min(buffer.len() as u64) as usize;
                source.read_exact(&mut buffer[..to_read])?;
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
    pub fn read_range<R: Read + Seek>(&self, source: &mut R, range: ByteRange) -> Result<Vec<u8>> {
        source.seek(SeekFrom::Start(range.offset))?;
        let mut buffer = vec![0u8; range.size as usize];
        source.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    /// Create a chunked stream for a byte range
    ///
    /// This allows streaming through a range without loading it all into memory.
    /// Useful for hashing large ranges efficiently.
    pub fn read_range_chunked<'a, R: Read + Seek>(
        &self,
        source: &'a mut R,
        range: ByteRange,
        chunk_size: usize,
    ) -> Result<ChunkedSegmentReader<Take<&'a mut R>>> {
        source.seek(SeekFrom::Start(range.offset))?;
        let taken = source.take(range.size);
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
    /// # fn example(structure: &Structure) -> Result<()> {
    /// // Hash everything except JUMBF segments
    /// let ranges = structure.hashable_ranges(&["jumbf"]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn hashable_ranges(&self, exclusions: &[&str]) -> Vec<ByteRange> {
        let mut ranges = Vec::new();
        let mut last_end = 0u64;

        for segment in &self.segments {
            let should_exclude = exclusions.iter().any(|pattern| {
                segment
                    .path
                    .as_deref()
                    .map(|p| p.contains(pattern))
                    .unwrap_or(false)
            });

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
            .filter(|(_, seg)| {
                seg.path
                    .as_deref()
                    .map(|p| p.contains(pattern))
                    .unwrap_or(false)
            })
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
                !exclusions.iter().any(|pattern| {
                    seg.path
                        .as_deref()
                        .map(|p| p.contains(pattern))
                        .unwrap_or(false)
                })
            })
            .map(|(i, seg)| (i, seg, seg.location()))
            .collect()
    }

    /// Calculate hash over all ranges except excluded segments (zero-copy with mmap)
    ///
    /// This is optimized for C2PA workflows where you need to hash the entire file
    /// except the C2PA manifest itself. Correctly handles multi-range segments.
    ///
    /// # Zero-Copy Behavior
    /// - With memory mapping: Direct hash from mapped memory (no allocation)
    /// - Without memory mapping: Streams through source in chunks
    ///
    /// # Example
    /// ```no_run
    /// # use asset_io::*;
    /// # use std::fs::File;
    /// # fn example() -> Result<()> {
    /// # let file = File::open("test.jpg")?;
    /// # let mut asset = Asset::from_source(file)?;
    ///
    /// // Hash everything except C2PA JUMBF
    /// use sha2::{Sha256, Digest};
    /// let mut hasher = Sha256::new();
    ///
    /// // Get the excluded segments
    /// let jumbf_index = asset.structure().c2pa_jumbf_index();
    ///
    /// // Use the Asset's hash_excluding_segments method instead
    /// asset.hash_excluding_segments(&[jumbf_index], &mut hasher)?;
    /// let hash = hasher.finalize();
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "hashing")]
    pub fn hash_excluding_segments<R: Read + Seek, H: std::io::Write>(
        &self,
        source: &mut R,
        excluded_indices: &[Option<usize>],
        hasher: &mut H,
    ) -> Result<()> {
        use crate::segment::ByteRange;

        // Build exclusion ranges from all segments and ALL their ranges
        let mut exclusion_ranges: Vec<ByteRange> = Vec::new();
        for &idx_opt in excluded_indices {
            if let Some(idx) = idx_opt {
                if idx < self.segments.len() {
                    let segment = &self.segments[idx];
                    // IMPORTANT: Include ALL ranges, not just the first one
                    // This is critical for multi-part JUMBF in JPEG
                    exclusion_ranges.extend_from_slice(&segment.ranges);
                }
            }
        }

        // Sort exclusions by offset and merge overlapping/contiguous ranges
        exclusion_ranges.sort_by_key(|r| r.offset);
        let mut merged_exclusions: Vec<ByteRange> = Vec::new();
        for range in exclusion_ranges {
            if let Some(last) = merged_exclusions.last_mut() {
                if last.end_offset() >= range.offset {
                    // Overlapping or contiguous - merge
                    let new_end = last.end_offset().max(range.end_offset());
                    last.size = new_end - last.offset;
                    continue;
                }
            }
            merged_exclusions.push(range);
        }

        // Calculate hashable ranges (everything except exclusions)
        let mut ranges = Vec::new();
        let mut last_end = 0u64;

        for exclusion in &merged_exclusions {
            if last_end < exclusion.offset {
                ranges.push(ByteRange {
                    offset: last_end,
                    size: exclusion.offset - last_end,
                });
            }
            last_end = exclusion.end_offset();
        }

        // Add final range to end of file
        if last_end < self.total_size {
            ranges.push(ByteRange {
                offset: last_end,
                size: self.total_size - last_end,
            });
        }

        // Hash the ranges (zero-copy if mmap available)
        #[cfg(feature = "memory-mapped")]
        if self.mmap.is_some() {
            // Zero-copy path: hash directly from memory map
            for range in ranges {
                if let Some(slice) = self.get_mmap_slice(range) {
                    hasher.write_all(slice)?;
                } else {
                    // Fallback to streaming if mmap slice unavailable
                    self.hash_range_from_source(source, range, hasher)?;
                }
            }
            return Ok(());
        }

        // Streaming path: read in chunks
        for range in ranges {
            self.hash_range_from_source(source, range, hasher)?;
        }

        Ok(())
    }

    /// Helper to hash a single range from source (streaming)
    #[cfg(feature = "hashing")]
    fn hash_range_from_source<R: Read + Seek, H: std::io::Write>(
        &self,
        source: &mut R,
        range: ByteRange,
        hasher: &mut H,
    ) -> Result<()> {
        use std::io::SeekFrom;

        source.seek(SeekFrom::Start(range.offset))?;

        let mut remaining = range.size;
        let mut buffer = vec![0u8; 8192];

        while remaining > 0 {
            let to_read = remaining.min(buffer.len() as u64) as usize;
            source.read_exact(&mut buffer[..to_read])?;
            hasher.write_all(&buffer[..to_read])?;
            remaining -= to_read as u64;
        }

        Ok(())
    }

    /// Create a chunked stream for a specific segment
    ///
    /// This allows streaming through segment data without loading it all into memory.
    pub fn read_segment_chunked<'a, R: Read + Seek>(
        &self,
        source: &'a mut R,
        segment_index: usize,
        chunk_size: usize,
    ) -> Result<ChunkedSegmentReader<Take<&'a mut R>>> {
        let segment = &self.segments[segment_index];
        let loc = segment.location();
        source.seek(SeekFrom::Start(loc.offset))?;
        let taken = source.take(loc.size);
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
        self.segments.iter().find_map(|seg| {
            if seg.is_image_data() {
                Some(seg.location())
            } else {
                None
            }
        })
    }
}
