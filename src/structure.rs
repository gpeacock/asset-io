//! File structure representation

use crate::{
    error::{Error, Result},
    segment::{ByteRange, ChunkedSegmentReader, Location, Segment},
    thumbnail::EmbeddedThumbnail,
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
    #[cfg(feature = "memory-mapped")]
    pub fn get_mmap_slice(&self, range: ByteRange) -> Option<&[u8]> {
        self.mmap.as_ref().map(|mmap| {
            let start = range.offset as usize;
            let end = start + range.size as usize;
            &mmap[start..end]
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

    /// Get XMP data (loads lazily if needed, assembles extended parts if present)
    pub fn xmp<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
        let Some(index) = self.xmp_index else {
            return Ok(None);
        };

        if let Segment::Xmp {
            offset,
            size,
            data,
            extended_parts,
        } = &mut self.segments[index]
        {
            reader.seek(SeekFrom::Start(*offset))?;
            let location = Location {
                offset: *offset,
                size: *size,
            };
            let main_xmp = data.load(reader, location)?.to_vec();

            // If no extended parts, return main XMP
            if extended_parts.is_empty() {
                return Ok(Some(main_xmp));
            }

            // Assemble extended XMP
            // Sort parts by chunk_offset
            let mut parts = extended_parts.clone();
            parts.sort_by_key(|p| p.chunk_offset);

            // Validate all parts have same GUID and total_size
            if parts.is_empty() {
                return Ok(Some(main_xmp));
            }

            let first_guid = &parts[0].guid;
            let total_size = parts[0].total_size;

            for part in &parts {
                if &part.guid != first_guid {
                    return Err(Error::InvalidFormat(
                        "Extended XMP parts have mismatched GUIDs".into(),
                    ));
                }
                if part.total_size != total_size {
                    return Err(Error::InvalidFormat(
                        "Extended XMP parts have mismatched total sizes".into(),
                    ));
                }
            }

            // Allocate buffer for complete extended XMP
            let mut extended_xmp = vec![0u8; total_size as usize];

            // Read each chunk into the correct position
            for part in &parts {
                reader.seek(SeekFrom::Start(part.location.offset))?;
                let end_pos = (part.chunk_offset as usize + part.location.size as usize)
                    .min(extended_xmp.len());
                if part.chunk_offset as usize >= extended_xmp.len() {
                    continue; // Skip malformed chunks
                }
                let chunk_data = &mut extended_xmp[part.chunk_offset as usize..end_pos];
                reader.read_exact(chunk_data)?;
            }

            // According to XMP spec, extended XMP is the complete XMP
            // (the main XMP just has a pointer to it via xmpNote:HasExtendedXMP)
            // So we return the extended XMP, which contains everything
            Ok(Some(extended_xmp))
        } else {
            Ok(None)
        }
    }

    /// Get JUMBF data (loads and assembles from multiple segments if needed)
    pub fn jumbf<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
        if self.jumbf_indices.is_empty() {
            return Ok(None);
        }

        let mut result = Vec::new();

        for &index in &self.jumbf_indices {
            if let Segment::Jumbf {
                offset,
                size,
                segments,
                data: _,
            } = &mut self.segments[index]
            {
                // If there are multiple segments, assemble them
                // Each segment has: JPEG XT header (8 bytes: CI + En + Z) followed by JUMBF data
                // We need to strip the JPEG XT header from each segment
                const JPEG_XT_HEADER_SIZE: usize = 8;

                if segments.len() > 1 {
                    for (i, loc) in segments.iter().enumerate() {
                        reader.seek(SeekFrom::Start(loc.offset))?;

                        // First segment: skip JPEG XT header, keep everything else
                        // Continuation segments: skip JPEG XT header + LBox + TBox (16 bytes total)
                        let skip_bytes = if i == 0 {
                            JPEG_XT_HEADER_SIZE
                        } else {
                            JPEG_XT_HEADER_SIZE + 8 // Skip JPEG XT header + repeated LBox/TBox
                        };

                        let data_size = loc.size.saturating_sub(skip_bytes as u64);
                        if data_size > 0 {
                            let mut buf = vec![0u8; skip_bytes];
                            reader.read_exact(&mut buf)?; // Skip the header

                            let mut buf = vec![0u8; data_size as usize];
                            reader.read_exact(&mut buf)?;
                            result.extend_from_slice(&buf);
                        }
                    }
                } else {
                    // Single segment: skip JPEG XT header
                    reader.seek(SeekFrom::Start(*offset))?;

                    // Skip JPEG XT header
                    let mut skip_buf = [0u8; JPEG_XT_HEADER_SIZE];
                    reader.read_exact(&mut skip_buf)?;

                    let data_size = size.saturating_sub(JPEG_XT_HEADER_SIZE as u64);
                    if data_size > 0 {
                        let mut buf = vec![0u8; data_size as usize];
                        reader.read_exact(&mut buf)?;
                        result.extend_from_slice(&buf);
                    }
                }
            }
        }

        Ok(if result.is_empty() {
            None
        } else {
            Some(result)
        })
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

    /// Get all hashable segments (legacy method - only checks ImageData)
    pub fn hashable_segments(&self) -> Vec<usize> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, seg)| seg.is_hashable())
            .map(|(i, _)| i)
            .collect()
    }

    // ============================================================================
    // Range-based Data Access API (for C2PA hashing)
    // ============================================================================

    /// Read a specific byte range from the file
    /// 
    /// This is useful for data hash models that need to hash arbitrary ranges.
    pub fn read_range<R: Read + Seek>(
        &self,
        reader: &mut R,
        range: ByteRange,
    ) -> Result<Vec<u8>> {
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
                !exclusions.iter().any(|pattern| seg.path().contains(pattern))
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
    pub fn embedded_thumbnail(&self) -> Result<Option<EmbeddedThumbnail>> {
        // Dispatch to format-specific extraction
        match self.format {
            Format::Jpeg => self.jpeg_embedded_thumbnail(),
            #[cfg(feature = "png")]
            Format::Png => Ok(None), // PNG doesn't have embedded thumbnails
            #[cfg(feature = "bmff")]
            Format::Bmff => Ok(None), // TODO: Implement BMFF thumbnail extraction
        }
    }
    
    /// Extract EXIF thumbnail from JPEG (if present)
    ///
    /// JPEG files often contain a thumbnail in their EXIF metadata.
    /// This is typically 160x120 pixels and encoded as JPEG.
    ///
    /// Note: This currently returns None. Full implementation requires
    /// parsing the EXIF/TIFF structure, which will be added when
    /// EXIF segment support is implemented.
    fn jpeg_embedded_thumbnail(&self) -> Result<Option<EmbeddedThumbnail>> {
        // TODO: When EXIF support is added, parse the EXIF segment
        // and extract the thumbnail from IFD1 if present.
        //
        // The EXIF thumbnail is typically at:
        // - IFD1 (the thumbnail IFD)
        // - Tags: JPEGInterchangeFormat (offset) and JPEGInterchangeFormatLength (size)
        //
        // For now, return None
        Ok(None)
    }
}
