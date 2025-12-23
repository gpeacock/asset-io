//! File structure representation

use crate::{
    error::Result,
    segment::{Location, Segment},
    Format,
};
use std::io::{Read, Seek, SeekFrom};

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
        }
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

    /// Get XMP data (loads lazily if needed)
    pub fn xmp<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<&[u8]>> {
        let Some(index) = self.xmp_index else {
            return Ok(None);
        };

        if let Segment::Xmp { offset, size, data } = &mut self.segments[index] {
            reader.seek(SeekFrom::Start(*offset))?;
            let location = Location {
                offset: *offset,
                size: *size,
            };
            Ok(Some(data.load(reader, location)?))
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

    /// Get all hashable segments
    pub fn hashable_segments(&self) -> Vec<usize> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, seg)| seg.is_hashable())
            .map(|(i, _)| i)
            .collect()
    }
}
