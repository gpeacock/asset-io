//! JPEG container I/O implementation

use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind},
    structure::Structure,
    Container, ContainerIO, Updates,
};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{copy, Read, Seek, SeekFrom, Write};

// JPEG markers
const SOI: u8 = 0xD8; // Start of Image
const EOI: u8 = 0xD9; // End of Image
const APP1: u8 = 0xE1; // XMP / EXIF
const APP11: u8 = 0xEB; // JUMBF
const SOS: u8 = 0xDA; // Start of Scan (image data follows)

// Special markers without length
const RST0: u8 = 0xD0;
const RST7: u8 = 0xD7;

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXTENDED_SIGNATURE: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const EXIF_SIGNATURE: &[u8] = b"Exif\0\0";
const C2PA_MARKER: &[u8] = b"c2pa";
const MAX_MARKER_SIZE: usize = 65533; // Max size for JPEG marker segment

/// Get human-readable label for a JPEG marker
fn marker_label(marker: u8) -> &'static str {
    match marker {
        0xD8 => "SOI",
        0xD9 => "EOI",
        0xDA => "SOS",
        0xDB => "DQT",
        0xC0 => "SOF0",
        0xC4 => "DHT",
        0xDD => "DRI",
        0xFE => "COM",
        0xE0 => "APP0",
        0xE1 => "APP1",
        0xE2 => "APP2",
        0xE3 => "APP3",
        0xE4 => "APP4",
        0xE5 => "APP5",
        0xE6 => "APP6",
        0xE7 => "APP7",
        0xE8 => "APP8",
        0xE9 => "APP9",
        0xEA => "APP10",
        0xEB => "APP11",
        0xEC => "APP12",
        0xED => "APP13",
        0xEE => "APP14",
        0xEF => "APP15",
        _ => "OTHER",
    }
}

/// JPEG container I/O implementation
pub struct JpegIO;

impl JpegIO {
    /// Create a new JPEG I/O implementation
    pub fn new() -> Self {
        Self
    }

    /// Formats this handler supports
    pub fn container_type() -> Container {
        Container::Jpeg
    }

    /// Media types this handler supports
    pub fn supported_media_types() -> &'static [crate::MediaType] {
        &[crate::MediaType::Jpeg]
    }

    /// File extensions this handler accepts
    pub fn extensions() -> &'static [&'static str] {
        &["jpg", "jpeg", "jpe", "jfif"]
    }

    /// MIME types this handler accepts
    pub fn mime_types() -> &'static [&'static str] {
        &["image/jpeg", "image/jpg"]
    }

    /// Detect if this is a JPEG file from header
    pub fn detect(header: &[u8]) -> Option<crate::Container> {
        // JPEG magic bytes: FF D8
        if header.len() >= 2 && header[0] == 0xFF && header[1] == 0xD8 {
            Some(Container::Jpeg)
        } else {
            None
        }
    }

    /// Extract XMP data from JPEG file (handles extended XMP with multi-segment assembly)
    pub fn extract_xmp_impl<R: Read + Seek>(
        structure: &crate::structure::Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        let Some(index) = structure.xmp_index() else {
            return Ok(None);
        };

        let segment = &structure.segments()[index];
        if !segment.is_xmp() {
            return Ok(None);
        }

        // Check if this has extended XMP metadata
        if let Some(meta) = &segment.metadata {
            if let Some((guid, chunk_offsets, total_size)) = meta.as_jpeg_extended_xmp() {
                // JPEG Extended XMP - reassemble from parts
                return Self::reassemble_extended_xmp(
                    source,
                    &segment.ranges,
                    guid,
                    chunk_offsets,
                    total_size,
                );
            }
        }

        // Simple case: single range or concatenated ranges
        if segment.ranges.len() == 1 {
            // Single XMP range
            let range = segment.ranges[0];
            source.seek(SeekFrom::Start(range.offset))?;
            let mut xmp_data = vec![0u8; range.size as usize];
            source.read_exact(&mut xmp_data)?;
            Ok(Some(xmp_data))
        } else {
            // Multiple ranges - concatenate them
            let total_size: u64 = segment.ranges.iter().map(|r| r.size).sum();
            let mut xmp_data = Vec::with_capacity(total_size as usize);
            for range in &segment.ranges {
                source.seek(SeekFrom::Start(range.offset))?;
                let mut chunk = vec![0u8; range.size as usize];
                source.read_exact(&mut chunk)?;
                xmp_data.extend_from_slice(&chunk);
            }
            Ok(Some(xmp_data))
        }
    }

    /// Reassemble JPEG Extended XMP from multiple parts using chunk offsets
    fn reassemble_extended_xmp<R: Read + Seek>(
        source: &mut R,
        ranges: &[ByteRange],
        _guid: &str,
        chunk_offsets: &[u32],
        total_size: u32,
    ) -> Result<Option<Vec<u8>>> {
        // Validate total_size to prevent DOS attacks
        const MAX_XMP_SIZE: u32 = 100 * 1024 * 1024; // 100 MB
        if total_size > MAX_XMP_SIZE {
            return Err(Error::InvalidSegment {
                offset: 0,
                reason: format!(
                    "Extended XMP too large: {} bytes (max {} MB)",
                    total_size,
                    MAX_XMP_SIZE / (1024 * 1024)
                ),
            });
        }

        // Skip first range (it's the main XMP with pointer)
        if ranges.len() < 2 || chunk_offsets.len() != ranges.len() - 1 {
            // Malformed - should have main range + extended ranges
            // Fall back to reading just the first range
            if !ranges.is_empty() {
                source.seek(SeekFrom::Start(ranges[0].offset))?;
                let mut xmp_data = vec![0u8; ranges[0].size as usize];
                source.read_exact(&mut xmp_data)?;
                return Ok(Some(xmp_data));
            }
            return Ok(None);
        }

        // Allocate buffer for complete extended XMP
        let mut extended_xmp = vec![0u8; total_size as usize];

        // Read each extended chunk into the correct position
        // Note: ranges[0] is main XMP, ranges[1..] are extended parts
        for (range, &chunk_offset) in ranges[1..].iter().zip(chunk_offsets) {
            source.seek(SeekFrom::Start(range.offset))?;
            let end_pos = (chunk_offset as usize + range.size as usize).min(extended_xmp.len());
            if chunk_offset as usize >= extended_xmp.len() {
                continue; // Skip malformed chunks
            }
            let chunk_data = &mut extended_xmp[chunk_offset as usize..end_pos];
            source.read_exact(chunk_data)?;
        }

        // According to XMP spec, extended XMP is the complete XMP
        // (the main XMP just has a pointer to it via xmpNote:HasExtendedXMP)
        Ok(Some(extended_xmp))
    }

    /// Extract JUMBF data from JPEG file (handles JPEG XT headers and multi-segment assembly)
    pub fn extract_jumbf_impl<R: Read + Seek>(
        structure: &crate::structure::Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        if structure.jumbf_indices().is_empty() {
            return Ok(None);
        }

        let mut result = Vec::new();

        // Extract all JUMBF segments and concatenate them
        // Note: The parser already skips JPEG XT headers when creating ranges,
        // so we just read the raw JUMBF data directly from the ranges
        for &index in structure.jumbf_indices() {
            let segment = &structure.segments()[index];
            if !segment.is_jumbf() {
                continue;
            }

            for range in &segment.ranges {
                // Validate size to prevent memory exhaustion attacks
                if range.size > crate::segment::MAX_SEGMENT_SIZE {
                    return Err(crate::Error::InvalidSegment {
                        offset: range.offset,
                        reason: format!(
                            "JUMBF range too large: {} bytes (max {} MB)",
                            range.size,
                            crate::segment::MAX_SEGMENT_SIZE / (1024 * 1024)
                        ),
                    });
                }
                
                source.seek(SeekFrom::Start(range.offset))?;
                let mut buf = vec![0u8; range.size as usize];
                source.read_exact(&mut buf)?;
                result.extend_from_slice(&buf);
            }
        }

        Ok(if result.is_empty() {
            None
        } else {
            Some(result)
        })
    }

    /// Fast single-pass parser
    fn parse_impl<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        let mut structure = Structure::new(Container::Jpeg, crate::MediaType::Jpeg);

        // Check SOI marker
        if source.read_u8()? != 0xFF || source.read_u8()? != SOI {
            return Err(Error::InvalidFormat("Not a JPEG file".into()));
        }

        structure.add_segment(Segment::new(0, 2, SegmentKind::Header, None));

        let mut offset = 2u64;

        loop {
            // Read marker
            let marker_prefix = source.read_u8()?;
            let marker = source.read_u8()?;

            if marker_prefix != 0xFF {
                return Err(Error::InvalidSegment {
                    offset,
                    reason: format!("Expected 0xFF, got 0x{:02X}", marker_prefix),
                });
            }

            // Handle padding bytes
            if marker == 0xFF {
                continue;
            }

            match marker {
                EOI => {
                    structure.add_segment(Segment::new(
                        offset,
                        2,
                        SegmentKind::Other,
                        Some(marker_label(EOI).to_string()),
                    ));
                    structure.total_size = offset + 2;
                    break;
                }

                SOS => {
                    // Start of scan - image data follows
                    let size = source.read_u16::<BigEndian>()? as u64;
                    let sos_start = offset; // Start of FF DA marker

                    // Skip SOS header
                    source.seek(SeekFrom::Current((size - 2) as i64))?;

                    // Find end of image data (scan for FFD9)
                    let image_end = find_eoi(source)?;

                    // ImageData includes SOS marker + header + compressed data
                    // This makes writing easier - just copy the whole thing
                    structure.add_segment(Segment::new(
                        sos_start,
                        image_end - sos_start,
                        SegmentKind::ImageData,
                        Some("sos".to_string()),
                    ));

                    // Add EOI segment
                    structure.add_segment(Segment::new(
                        image_end,
                        2,
                        SegmentKind::Other,
                        Some(marker_label(EOI).to_string()),
                    ));

                    structure.total_size = image_end + 2;
                    break;
                }

                APP1 => {
                    let size = source.read_u16::<BigEndian>()? as u64;
                    let data_size = size - 2;
                    let segment_start = offset;

                    // Read enough bytes to check both standard and extended XMP signatures
                    let sig_len = XMP_EXTENDED_SIGNATURE.len().max(XMP_SIGNATURE.len());
                    let bytes_to_read = sig_len.min(data_size as usize);
                    let mut sig_buf = vec![0u8; bytes_to_read];
                    source.read_exact(&mut sig_buf)?;

                    if sig_buf.len() >= XMP_SIGNATURE.len()
                        && &sig_buf[..XMP_SIGNATURE.len()] == XMP_SIGNATURE
                    {
                        // Standard XMP segment
                        let xmp_offset = offset + 4 + XMP_SIGNATURE.len() as u64;
                        let xmp_size = data_size - XMP_SIGNATURE.len() as u64;
                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(xmp_offset, xmp_size)],
                            SegmentKind::Xmp,
                            Some("app1".to_string()),
                        ));
                        let remaining = (data_size as usize) - sig_buf.len();
                        source.seek(SeekFrom::Current(remaining as i64))?;
                    } else if sig_buf.len() >= XMP_EXTENDED_SIGNATURE.len()
                        && &sig_buf[..XMP_EXTENDED_SIGNATURE.len()] == XMP_EXTENDED_SIGNATURE
                    {
                        // Extended XMP segment
                        // Format: signature (35) + GUID (32) + full_length (4) + offset (4) + data
                        const HEADER_SIZE: u64 = 32 + 4 + 4; // GUID + full_length + offset

                        if data_size < XMP_EXTENDED_SIGNATURE.len() as u64 + HEADER_SIZE {
                            // Malformed extended XMP - skip it
                            let remaining = (data_size as usize) - sig_buf.len();
                            source.seek(SeekFrom::Current(remaining as i64))?;
                            structure.add_segment(Segment::new(
                                segment_start,
                                size + 2,
                                SegmentKind::Other,
                                Some(marker_label(APP1).to_string()),
                            ));
                        } else {
                            // Read GUID (32 bytes as ASCII hex string)
                            let mut guid_bytes = [0u8; 32];
                            source.read_exact(&mut guid_bytes)?;
                            let guid = String::from_utf8_lossy(&guid_bytes).to_string();

                            // Read full length (4 bytes, big-endian)
                            let total_size = source.read_u32::<BigEndian>()?;

                            // Read chunk offset (4 bytes, big-endian)
                            let chunk_offset = source.read_u32::<BigEndian>()?;

                            // Data starts after all headers
                            let chunk_data_offset =
                                offset + 4 + XMP_EXTENDED_SIGNATURE.len() as u64 + HEADER_SIZE;
                            let chunk_data_size =
                                data_size - XMP_EXTENDED_SIGNATURE.len() as u64 - HEADER_SIZE;

                            // Find the main XMP segment to attach this to
                            let xmp_index = *structure.xmp_index_mut();

                            if let Some(idx) = xmp_index {
                                let segment = &mut structure.segments[idx];
                                if segment.is_xmp() {
                                    // Add range to existing segment
                                    segment
                                        .ranges
                                        .push(ByteRange::new(chunk_data_offset, chunk_data_size));

                                    // Update or create metadata
                                    match &mut segment.metadata {
                                        Some(crate::SegmentMetadata::JpegExtendedXmp {
                                            guid: existing_guid,
                                            chunk_offsets,
                                            total_size: existing_total,
                                        }) => {
                                            // Validate GUID and total_size match
                                            if existing_guid != &guid {
                                                // GUID mismatch - this shouldn't happen
                                                // but handle gracefully by skipping
                                            } else if *existing_total != total_size {
                                                // total_size mismatch
                                            } else {
                                                chunk_offsets.push(chunk_offset);
                                            }
                                        }
                                        None => {
                                            // First extended part - create metadata
                                            segment.metadata =
                                                Some(crate::SegmentMetadata::JpegExtendedXmp {
                                                    guid,
                                                    chunk_offsets: vec![chunk_offset],
                                                    total_size,
                                                });
                                        }
                                        #[cfg(feature = "exif")]
                                        Some(crate::SegmentMetadata::Thumbnail(_)) => {
                                            // XMP segment shouldn't have thumbnail metadata, but handle gracefully
                                        }
                                    }
                                }
                            } else {
                                // Extended XMP without main XMP - malformed but handle gracefully
                                structure.add_segment(Segment::new(
                                    segment_start,
                                    size + 2,
                                    SegmentKind::Other,
                                    Some(marker_label(APP1).to_string()),
                                ));
                            }

                            // Skip to next marker
                            let remaining = chunk_data_size;
                            source.seek(SeekFrom::Current(remaining as i64))?;
                        }
                    } else {
                        // Check for EXIF segment
                        // Check for EXIF segment
                        if sig_buf.len() >= EXIF_SIGNATURE.len()
                            && &sig_buf[..EXIF_SIGNATURE.len()] == EXIF_SIGNATURE
                        {
                            // EXIF segment
                            #[cfg(feature = "exif")]
                            {
                                // Parse for embedded thumbnail
                                // Note: We already read sig_buf, so only read the remaining EXIF data
                                let remaining_to_read = (data_size as usize) - sig_buf.len();
                                let mut remaining_data = vec![0u8; remaining_to_read];
                                source.read_exact(&mut remaining_data)?;

                                // Reconstruct full EXIF data (TIFF part only, without "Exif\0\0")
                                let exif_tiff_start = EXIF_SIGNATURE.len();
                                let mut exif_data = Vec::new();
                                exif_data.extend_from_slice(&sig_buf[exif_tiff_start..]);
                                exif_data.extend_from_slice(&remaining_data);

                                // Parse TIFF structure to find thumbnail
                                let thumbnail = match crate::tiff::parse_thumbnail_info(&exif_data)
                                {
                                    Ok(Some(thumb_info)) => {
                                        // Create EmbeddedThumbnailInfo with location relative to EXIF segment start
                                        let thumb_offset = segment_start
                                            + 4
                                            + EXIF_SIGNATURE.len() as u64
                                            + thumb_info.offset as u64;
                                        Some(crate::thumbnail::EmbeddedThumbnailInfo::new(
                                            thumb_offset,
                                            thumb_info.size as u64,
                                            crate::thumbnail::ThumbnailFormat::Jpeg,
                                            thumb_info.width,
                                            thumb_info.height,
                                        ))
                                    }
                                    Ok(None) => None,
                                    Err(_) => None, // Ignore EXIF parsing errors
                                };

                                let thumbnail_meta =
                                    thumbnail.map(crate::SegmentMetadata::Thumbnail);
                                let mut segment = Segment::new(
                                    segment_start,
                                    size + 2,
                                    SegmentKind::Exif,
                                    Some("app1".to_string()),
                                );
                                if let Some(meta) = thumbnail_meta {
                                    segment = segment.with_metadata(meta);
                                }
                                structure.add_segment(segment);
                            }

                            #[cfg(not(feature = "exif"))]
                            {
                                // Just record the EXIF segment without parsing thumbnails
                                structure.add_segment(Segment::new(
                                    segment_start,
                                    size + 2,
                                    SegmentKind::Exif,
                                    Some("app1".to_string()),
                                ));

                                // Skip remaining EXIF data
                                let remaining = (data_size as usize) - sig_buf.len();
                                source.seek(SeekFrom::Current(remaining as i64))?;
                            }
                        } else {
                            // Other APP1 segment
                            let remaining = (data_size as usize) - sig_buf.len();
                            source.seek(SeekFrom::Current(remaining as i64))?;
                            structure.add_segment(Segment::new(
                                segment_start,
                                size + 2,
                                SegmentKind::Other,
                                Some(marker_label(APP1).to_string()),
                            ));
                        }

                        // If not XMP or EXIF, treat as Other APP1 segment
                        if sig_buf.len() < EXIF_SIGNATURE.len()
                            || &sig_buf[..EXIF_SIGNATURE.len()] != EXIF_SIGNATURE
                        {
                            // Other APP1 segment (probably EXIF)
                            let remaining = (data_size as usize) - sig_buf.len();
                            source.seek(SeekFrom::Current(remaining as i64))?;
                            structure.add_segment(Segment::new(
                                segment_start,
                                size + 2,
                                SegmentKind::Other,
                                Some(marker_label(APP1).to_string()),
                            ));
                        }
                    }

                    offset += 2 + size;
                }

                APP11 => {
                    let size = source.read_u16::<BigEndian>()? as u64;
                    let data_size = size - 2;
                    let data_start = offset + 4;
                    let segment_start = offset;

                    // Check for JPEG XT + JUMBF structure OR raw JUMBF
                    // Format 1: JPEG XT: CI(2) + En(2) + Z(4) = 8 bytes, then JUMBF superbox
                    // Format 2: Raw JUMBF superbox directly (used by c2pa crate)
                    let mut header = [0u8; 32];
                    let bytes_to_read = header.len().min(data_size as usize);
                    source.read_exact(&mut header[..bytes_to_read])?;

                    // Check if this is JPEG XT with JUMBF
                    let is_jpeg_xt = bytes_to_read >= 8 && &header[0..2] == b"JP";
                    let has_jumb_box_after_xt = bytes_to_read >= 16 && &header[12..16] == b"jumb";
                    let has_c2pa_after_xt = bytes_to_read >= 32 && &header[28..32] == C2PA_MARKER;

                    // Check if this is raw JUMBF (no JPEG XT wrapper)
                    let has_jumb_box_direct = bytes_to_read >= 8 && &header[4..8] == b"jumb";
                    let has_c2pa_direct = bytes_to_read >= 24 && &header[20..24] == C2PA_MARKER;

                    let is_jumbf = (is_jpeg_xt && (has_jumb_box_after_xt || has_c2pa_after_xt))
                        || (has_jumb_box_direct || has_c2pa_direct);

                    if is_jumbf {
                        // Calculate the actual JUMBF data offset and size
                        // For JPEG XT format:
                        // - First segment: skip 8-byte header (JP + En + Z)
                        // - Continuation segments: skip 8-byte header + 8-byte repeated LBox/TBox
                        // For raw JUMBF, data starts immediately
                        let (jumbf_data_offset, jumbf_data_size) = if is_jpeg_xt {
                            const JPEG_XT_HEADER_SIZE: u64 = 8;
                            const REPEATED_LBOX_TBOX_SIZE: u64 = 8;

                            // Extract sequence number to detect continuation
                            let seq_num = if bytes_to_read >= 8 {
                                u32::from_be_bytes([header[4], header[5], header[6], header[7]])
                            } else {
                                1
                            };

                            // Continuation segments have extra overhead
                            let overhead = if seq_num > 1 {
                                JPEG_XT_HEADER_SIZE + REPEATED_LBOX_TBOX_SIZE
                            } else {
                                JPEG_XT_HEADER_SIZE
                            };

                            (data_start + overhead, data_size - overhead)
                        } else {
                            (data_start, data_size)
                        };

                        // Extract sequence number (only for JPEG XT format)
                        let seq_num = if is_jpeg_xt && bytes_to_read >= 8 {
                            u32::from_be_bytes([header[4], header[5], header[6], header[7]])
                        } else {
                            1 // Raw JUMBF always treated as first/only segment
                        };

                        // Check if this is a continuation of the previous JUMBF segment
                        let mut is_continuation = false;
                        if seq_num > 1 {
                            if let Some(last_segment) = structure.segments.last_mut() {
                                if last_segment.is_jumbf() {
                                    // Add this range to the existing JUMBF
                                    last_segment
                                        .ranges
                                        .push(ByteRange::new(jumbf_data_offset, jumbf_data_size));
                                    is_continuation = true;
                                }
                            }
                        }

                        if !is_continuation {
                            // New JUMBF segment
                            structure.add_segment(Segment::with_ranges(
                                vec![ByteRange::new(jumbf_data_offset, jumbf_data_size)],
                                SegmentKind::Jumbf,
                                Some("app11".to_string()),
                            ));
                        }

                        // Skip remaining JUMBF data
                        let remaining = data_size - bytes_to_read as u64;
                        source.seek(SeekFrom::Current(remaining as i64))?;
                    } else {
                        // Other APP11 segment - skip remaining data
                        let remaining = data_size - bytes_to_read as u64;
                        source.seek(SeekFrom::Current(remaining as i64))?;
                        structure.add_segment(Segment::new(
                            segment_start,
                            size + 2,
                            SegmentKind::Other,
                            Some(marker_label(APP11).to_string()),
                        ));
                    }

                    offset += 2 + size;
                }

                // RST markers have no length
                RST0..=RST7 => {
                    structure.add_segment(Segment::new(
                        offset,
                        2,
                        SegmentKind::Other,
                        Some(marker_label(marker).to_string()),
                    ));
                    offset += 2;
                }

                _ => {
                    // Standard marker with length
                    let size = source.read_u16::<BigEndian>()? as u64;
                    structure.add_segment(Segment::new(
                        offset,
                        size + 2,
                        SegmentKind::Other,
                        Some(marker_label(marker).to_string()),
                    ));
                    offset += 2 + size;
                    source.seek(SeekFrom::Start(offset))?;
                }
            }
        }

        Ok(structure)
    }
}

impl Default for JpegIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerIO for JpegIO {
    fn container_type() -> Container {
        Container::Jpeg
    }

    fn supported_media_types() -> &'static [crate::MediaType] {
        &[crate::MediaType::Jpeg]
    }

    fn extensions() -> &'static [&'static str] {
        &["jpg", "jpeg", "jpe", "jfif"]
    }

    fn mime_types() -> &'static [&'static str] {
        &["image/jpeg", "image/jpg"]
    }

    fn detect(header: &[u8]) -> Option<crate::Container> {
        if header.len() >= 2 && header[0] == 0xFF && header[1] == 0xD8 {
            Some(Container::Jpeg)
        } else {
            None
        }
    }

    fn parse<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        self.parse_impl(source)
    }

    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::extract_xmp_impl(structure, source)
    }

    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::extract_jumbf_impl(structure, source)
    }

    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        // Calculate the destination structure first - this tells us exactly what to write
        let dest_structure = self.calculate_updated_structure(structure, updates)?;

        source.seek(SeekFrom::Start(0))?;

        // Write SOI
        writer.write_u8(0xFF)?;
        writer.write_u8(SOI)?;

        // Iterate through destination structure and write each segment
        for dest_segment in &dest_structure.segments {
            match dest_segment {
                seg if seg.is_type(SegmentKind::Header) => {
                    // Already wrote SOI
                    continue;
                }

                seg if seg.is_xmp() => {
                    // Write XMP based on updates
                    match &updates.xmp {
                        crate::MetadataUpdate::Set(new_xmp) => {
                            write_xmp_segment(writer, new_xmp)?;
                        }
                        crate::MetadataUpdate::Keep => {
                            // Find corresponding source segment and copy it
                            if let Some(source_seg) = structure.segments.iter().find(|s| s.is_xmp())
                            {
                                // Handle extended XMP if present
                                if let Some(meta) = &source_seg.metadata {
                                    if let Some((guid, chunk_offsets, _total_size)) =
                                        meta.as_jpeg_extended_xmp()
                                    {
                                        // Write main XMP segment
                                        if !source_seg.ranges.is_empty() {
                                            writer.write_u8(0xFF)?;
                                            writer.write_u8(APP1)?;
                                            writer.write_u16::<BigEndian>(
                                                (source_seg.ranges[0].size
                                                    + XMP_SIGNATURE.len() as u64
                                                    + 2)
                                                    as u16,
                                            )?;
                                            writer.write_all(XMP_SIGNATURE)?;

                                            source.seek(SeekFrom::Start(
                                                source_seg.ranges[0].offset,
                                            ))?;
                                            let mut limited =
                                                source.take(source_seg.ranges[0].size);
                                            copy(&mut limited, writer)?;
                                        }

                                        // Write extended XMP segments
                                        for (i, range) in source_seg.ranges[1..].iter().enumerate()
                                        {
                                            let chunk_offset =
                                                chunk_offsets.get(i).copied().unwrap_or(0);

                                            writer.write_u8(0xFF)?;
                                            writer.write_u8(APP1)?;

                                            let seg_size = XMP_EXTENDED_SIGNATURE.len()
                                                + 32
                                                + 4
                                                + 4
                                                + range.size as usize
                                                + 2;
                                            writer.write_u16::<BigEndian>(seg_size as u16)?;

                                            writer.write_all(XMP_EXTENDED_SIGNATURE)?;

                                            let guid_bytes = guid.as_bytes();
                                            writer.write_all(
                                                &guid_bytes[..guid_bytes.len().min(32)],
                                            )?;
                                            for _ in guid_bytes.len()..32 {
                                                writer.write_u8(0)?;
                                            }

                                            writer.write_u32::<BigEndian>(
                                                source_seg.ranges[1..]
                                                    .iter()
                                                    .map(|r| r.size as u32)
                                                    .sum(),
                                            )?;
                                            writer.write_u32::<BigEndian>(chunk_offset)?;

                                            source.seek(SeekFrom::Start(range.offset))?;
                                            let mut limited = source.take(range.size);
                                            copy(&mut limited, writer)?;
                                        }
                                        continue;
                                    }
                                }

                                // Simple XMP - just copy
                                writer.write_u8(0xFF)?;
                                writer.write_u8(APP1)?;
                                writer.write_u16::<BigEndian>(
                                    (source_seg.ranges[0].size + XMP_SIGNATURE.len() as u64 + 2)
                                        as u16,
                                )?;
                                writer.write_all(XMP_SIGNATURE)?;

                                source.seek(SeekFrom::Start(source_seg.ranges[0].offset))?;
                                let mut limited = source.take(source_seg.ranges[0].size);
                                copy(&mut limited, writer)?;
                            }
                        }
                        crate::MetadataUpdate::Remove => {
                            // Skip - segment not in destination
                        }
                    }
                }

                seg if seg.is_jumbf() => {
                    // Write JUMBF based on updates
                    match &updates.jumbf {
                        crate::MetadataUpdate::Set(new_jumbf) => {
                            write_jumbf_segments(writer, new_jumbf)?;
                        }
                        crate::MetadataUpdate::Keep => {
                            // Read existing JUMBF data and re-write it
                            // (This ensures consistent JPEG XT formatting)
                            if let Some(source_seg) =
                                structure.segments.iter().find(|s| s.is_jumbf())
                            {
                                // Read the JUMBF data (without JPEG XT headers)
                                let total_size: u64 =
                                    source_seg.ranges.iter().map(|r| r.size).sum();
                                let mut jumbf_data = vec![0u8; total_size as usize];
                                let mut offset = 0;

                                for range in &source_seg.ranges {
                                    source.seek(SeekFrom::Start(range.offset))?;
                                    source.read_exact(
                                        &mut jumbf_data[offset..offset + range.size as usize],
                                    )?;
                                    offset += range.size as usize;
                                }

                                // Re-write with proper JPEG XT formatting
                                write_jumbf_segments(writer, &jumbf_data)?;
                            }
                        }
                        crate::MetadataUpdate::Remove => {
                            // Skip - segment not in destination
                        }
                    }
                }

                seg if seg.is_type(SegmentKind::ImageData) => {
                    // Find corresponding source segment
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.is_type(SegmentKind::ImageData))
                    {
                        // Copy SOS marker + image data + any RST markers + EOI
                        let location = source_seg.location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        let mut limited = source.take(location.size);
                        copy(&mut limited, writer)?;
                    }
                }

                seg if seg.path.as_deref() == Some("EOI") => {
                    // Write EOI marker
                    writer.write_u8(0xFF)?;
                    writer.write_u8(EOI)?;
                }

                _ => {
                    // Copy other segments from source
                    // Find corresponding source segment by kind and path
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.kind == dest_segment.kind && s.path == dest_segment.path)
                    {
                        let location = source_seg.location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        let mut limited = source.take(location.size);
                        copy(&mut limited, writer)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn write_with_processor<R: Read + Seek, W: Write, F: FnMut(&[u8])>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
        mut processor: F,
    ) -> Result<()> {
        use crate::processing_writer::ProcessingWriter;

        let exclude_segments = &updates.processing.exclude_segments;
        let exclusion_mode = updates.processing.exclusion_mode;

        let mut pw = ProcessingWriter::new(writer, |data| processor(data));
        let should_exclude_jumbf = exclude_segments.contains(&SegmentKind::Jumbf);
        let data_only_mode = exclusion_mode == crate::ExclusionMode::DataOnly;

        // Calculate the destination structure first
        let dest_structure = self.calculate_updated_structure(structure, updates)?;

        source.seek(SeekFrom::Start(0))?;

        // Write SOI
        pw.write_u8(0xFF)?;
        pw.write_u8(SOI)?;

        // Iterate through destination structure and write each segment
        for dest_segment in &dest_structure.segments {
            match dest_segment {
                seg if seg.is_type(SegmentKind::Header) => {
                    continue;
                }

                seg if seg.is_xmp() => {
                    match &updates.xmp {
                        crate::MetadataUpdate::Set(new_xmp) => {
                            write_xmp_segment(&mut pw, new_xmp)?;
                        }
                        crate::MetadataUpdate::Keep => {
                            if let Some(source_seg) = structure.segments.iter().find(|s| s.is_xmp())
                            {
                                if let Some(meta) = &source_seg.metadata {
                                    if let Some((guid, chunk_offsets, _total_size)) =
                                        meta.as_jpeg_extended_xmp()
                                    {
                                        if !source_seg.ranges.is_empty() {
                                            pw.write_u8(0xFF)?;
                                            pw.write_u8(APP1)?;
                                            pw.write_u16::<BigEndian>(
                                                (source_seg.ranges[0].size
                                                    + XMP_SIGNATURE.len() as u64
                                                    + 2)
                                                    as u16,
                                            )?;
                                            pw.write_all(XMP_SIGNATURE)?;

                                            source.seek(SeekFrom::Start(
                                                source_seg.ranges[0].offset,
                                            ))?;
                                            let mut limited =
                                                source.take(source_seg.ranges[0].size);
                                            copy(&mut limited, &mut pw)?;
                                        }

                                        for (i, range) in source_seg.ranges[1..].iter().enumerate()
                                        {
                                            let chunk_offset =
                                                chunk_offsets.get(i).copied().unwrap_or(0);

                                            pw.write_u8(0xFF)?;
                                            pw.write_u8(APP1)?;

                                            let seg_size = XMP_EXTENDED_SIGNATURE.len()
                                                + 32
                                                + 4
                                                + 4
                                                + range.size as usize
                                                + 2;
                                            pw.write_u16::<BigEndian>(seg_size as u16)?;

                                            pw.write_all(XMP_EXTENDED_SIGNATURE)?;

                                            let guid_bytes = guid.as_bytes();
                                            pw.write_all(
                                                &guid_bytes[..guid_bytes.len().min(32)],
                                            )?;
                                            for _ in guid_bytes.len()..32 {
                                                pw.write_u8(0)?;
                                            }

                                            pw.write_u32::<BigEndian>(
                                                source_seg.ranges[1..]
                                                    .iter()
                                                    .map(|r| r.size as u32)
                                                    .sum(),
                                            )?;
                                            pw.write_u32::<BigEndian>(chunk_offset)?;

                                            source.seek(SeekFrom::Start(range.offset))?;
                                            let mut limited = source.take(range.size);
                                            copy(&mut limited, &mut pw)?;
                                        }
                                        continue;
                                    }
                                }

                                pw.write_u8(0xFF)?;
                                pw.write_u8(APP1)?;
                                pw.write_u16::<BigEndian>(
                                    (source_seg.ranges[0].size + XMP_SIGNATURE.len() as u64 + 2)
                                        as u16,
                                )?;
                                pw.write_all(XMP_SIGNATURE)?;

                                source.seek(SeekFrom::Start(source_seg.ranges[0].offset))?;
                                let mut limited = source.take(source_seg.ranges[0].size);
                                copy(&mut limited, &mut pw)?;
                            }
                        }
                        crate::MetadataUpdate::Remove => {}
                    }
                }

                seg if seg.is_jumbf() => {
                    // Handle JUMBF based on exclusion mode:
                    // - DataOnly: Include headers in hash, exclude only data (C2PA compliant)
                    // - EntireSegment: Exclude entire segment including headers
                    match &updates.jumbf {
                        crate::MetadataUpdate::Set(new_jumbf) => {
                            if data_only_mode {
                                write_jumbf_with_exclusion(&mut pw, new_jumbf, should_exclude_jumbf)?;
                            } else {
                                // EntireSegment mode: exclude everything
                                if should_exclude_jumbf {
                                    pw.set_exclude_mode(true);
                                }
                                write_jumbf_segments(&mut pw, new_jumbf)?;
                                if should_exclude_jumbf {
                                    pw.set_exclude_mode(false);
                                }
                            }
                        }
                        crate::MetadataUpdate::Keep => {
                            if let Some(source_seg) =
                                structure.segments.iter().find(|s| s.is_jumbf())
                            {
                                let total_size: u64 =
                                    source_seg.ranges.iter().map(|r| r.size).sum();
                                let mut jumbf_data = vec![0u8; total_size as usize];
                                let mut offset = 0;

                                for range in &source_seg.ranges {
                                    source.seek(SeekFrom::Start(range.offset))?;
                                    source.read_exact(
                                        &mut jumbf_data[offset..offset + range.size as usize],
                                    )?;
                                    offset += range.size as usize;
                                }

                                if data_only_mode {
                                    write_jumbf_with_exclusion(&mut pw, &jumbf_data, should_exclude_jumbf)?;
                                } else {
                                    // EntireSegment mode: exclude everything
                                    if should_exclude_jumbf {
                                        pw.set_exclude_mode(true);
                                    }
                                    write_jumbf_segments(&mut pw, &jumbf_data)?;
                                    if should_exclude_jumbf {
                                        pw.set_exclude_mode(false);
                                    }
                                }
                            }
                        }
                        crate::MetadataUpdate::Remove => {}
                    }
                }

                seg if seg.is_type(SegmentKind::ImageData) => {
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.is_type(SegmentKind::ImageData))
                    {
                        let location = source_seg.location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        let mut limited = source.take(location.size);
                        copy(&mut limited, &mut pw)?;
                    }
                }

                seg if seg.path.as_deref() == Some("EOI") => {
                    pw.write_u8(0xFF)?;
                    pw.write_u8(EOI)?;
                }

                _ => {
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.kind == dest_segment.kind && s.path == dest_segment.path)
                    {
                        let location = source_seg.location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        let mut limited = source.take(location.size);
                        copy(&mut limited, &mut pw)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn calculate_updated_structure(
        &self,
        source_structure: &Structure,
        updates: &Updates,
    ) -> Result<Structure> {
        // NOTE: This logic must stay in sync with write()
        // This calculates where segments will be WITHOUT actually writing
        // The segment iteration and decision logic mirrors write() exactly
        use crate::MetadataUpdate;

        let mut dest_structure = Structure::new(Container::Jpeg, source_structure.media_type);
        let mut current_offset = 2u64; // Start after SOI marker

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing XMP/JUMBF
        let has_xmp = source_structure.segments.iter().any(|s| s.is_xmp());
        let has_jumbf = source_structure.segments.iter().any(|s| s.is_jumbf());

        for segment in &source_structure.segments {
            match segment {
                segment if segment.is_type(SegmentKind::Header) => {
                    // SOI already accounted for in current_offset
                    continue;
                }

                segment if segment.is_xmp() => {
                    match &updates.xmp {
                        MetadataUpdate::Keep => {
                            // Keep existing XMP - add segment at current offset
                            // XMP stored in segment is just XMP data (after signature)
                            // But we write: marker(2) + length(2) + signature(29) + data
                            let location = segment.location();
                            let segment_size = 2 + 2 + XMP_SIGNATURE.len() as u64 + location.size;
                            dest_structure.add_segment(Segment::new(
                                current_offset + 4 + XMP_SIGNATURE.len() as u64,
                                location.size,
                                SegmentKind::Xmp,
                                segment.path.clone(),
                            ));
                            current_offset += segment_size;
                            xmp_written = true;
                        }
                        MetadataUpdate::Set(new_xmp) if !xmp_written => {
                            // Write new XMP - calculate APP1 segment size
                            let xmp_size = new_xmp.len() as u64;
                            let segment_size = 2 + 2 + XMP_SIGNATURE.len() as u64 + xmp_size; // FF E1 + length + signature + data
                            dest_structure.add_segment(Segment::new(
                                current_offset + 4 + XMP_SIGNATURE.len() as u64,
                                xmp_size,
                                SegmentKind::Xmp,
                                Some("APP1/XMP".to_string()),
                            ));
                            current_offset += segment_size;
                            xmp_written = true;
                        }
                        MetadataUpdate::Remove | MetadataUpdate::Set(_) => {
                            // Skip this segment
                        }
                    }
                }

                segment if segment.is_jumbf() => {
                    match &updates.jumbf {
                        MetadataUpdate::Keep => {
                            // Keep existing JUMBF - calculate all APP11 segments
                            // IMPORTANT: Use total size from ALL ranges, not just first one!
                            let jumbf_data_size: u64 = segment.ranges.iter().map(|r| r.size).sum();

                            // Calculate how many APP11 segments needed
                            let mut remaining = jumbf_data_size;
                            let mut ranges = Vec::new();

                            const JPEG_XT_FIRST_OVERHEAD: usize = 8;
                            const JPEG_XT_CONT_OVERHEAD: usize = 16;
                            const MAX_DATA: usize = MAX_MARKER_SIZE - JPEG_XT_CONT_OVERHEAD;

                            let mut seg_num = 0;
                            while remaining > 0 {
                                let data_in_segment = remaining.min(MAX_DATA as u64);
                                let overhead = if seg_num == 0 {
                                    JPEG_XT_FIRST_OVERHEAD
                                } else {
                                    JPEG_XT_CONT_OVERHEAD
                                };
                                let segment_size = 2 + 2 + overhead as u64 + data_in_segment; // FF EB + length + overhead + data

                                ranges.push(ByteRange::new(
                                    current_offset + 4 + overhead as u64,
                                    data_in_segment,
                                ));
                                current_offset += segment_size;
                                remaining -= data_in_segment;
                                seg_num += 1;
                            }

                            dest_structure.add_segment_with_ranges(
                                SegmentKind::Jumbf,
                                ranges,
                                segment.path.clone(),
                            );
                            jumbf_written = true;
                        }
                        MetadataUpdate::Set(new_jumbf) if !jumbf_written => {
                            // Write new JUMBF - calculate all APP11 segments
                            let jumbf_size = new_jumbf.len() as u64;
                            let mut remaining = jumbf_size;
                            let mut ranges = Vec::new();

                            const JPEG_XT_FIRST_OVERHEAD: usize = 8;
                            const JPEG_XT_CONT_OVERHEAD: usize = 16;
                            const MAX_DATA: usize = MAX_MARKER_SIZE - JPEG_XT_CONT_OVERHEAD;

                            let mut seg_num = 0;
                            while remaining > 0 {
                                let data_in_segment = remaining.min(MAX_DATA as u64);
                                let overhead = if seg_num == 0 {
                                    JPEG_XT_FIRST_OVERHEAD
                                } else {
                                    JPEG_XT_CONT_OVERHEAD
                                };
                                let segment_size = 2 + 2 + overhead as u64 + data_in_segment;

                                ranges.push(ByteRange::new(
                                    current_offset + 4 + overhead as u64,
                                    data_in_segment,
                                ));
                                current_offset += segment_size;
                                remaining -= data_in_segment;
                                seg_num += 1;
                            }

                            dest_structure.add_segment_with_ranges(
                                SegmentKind::Jumbf,
                                ranges,
                                Some("APP11/C2PA".to_string()),
                            );
                            jumbf_written = true;
                        }
                        MetadataUpdate::Remove | MetadataUpdate::Set(_) => {
                            // Skip this segment
                        }
                    }
                }

                segment if segment.is_type(SegmentKind::ImageData) => {
                    // ImageData - just copy it (new metadata already added earlier)
                    let location = segment.location();
                    let segment_size = location.size;
                    dest_structure.add_segment(Segment::new(
                        current_offset,
                        segment_size,
                        SegmentKind::ImageData,
                        segment.path.clone(),
                    ));
                    current_offset += segment_size;
                }

                segment
                    if segment.is_type(SegmentKind::Other)
                        && segment.path.as_deref() == Some("EOI") =>
                {
                    // EOI marker
                    dest_structure.add_segment(Segment::new(
                        current_offset,
                        2,
                        SegmentKind::Other,
                        Some("EOI".to_string()),
                    ));
                    current_offset += 2;
                }

                _ => {
                    // Before copying "Other" segments, check if this is the transition point
                    // from APP markers to frame markers (DQT/SOF/etc)
                    // If so, insert new XMP/JUMBF here
                    let is_frame_marker = segment
                        .path
                        .as_deref()
                        .map(|p| {
                            p.starts_with("DQT")
                                || p.starts_with("SOF")
                                || p.starts_with("DHT")
                                || p.starts_with("DRI")
                        })
                        .unwrap_or(false);

                    if is_frame_marker {
                        // This is a frame marker - insert any pending new metadata before it
                        if !xmp_written && !has_xmp {
                            if let MetadataUpdate::Set(new_xmp) = &updates.xmp {
                                let xmp_size = new_xmp.len() as u64;
                                let segment_size = 2 + 2 + XMP_SIGNATURE.len() as u64 + xmp_size;
                                dest_structure.add_segment(Segment::new(
                                    current_offset + 4 + XMP_SIGNATURE.len() as u64,
                                    xmp_size,
                                    SegmentKind::Xmp,
                                    Some("APP1/XMP".to_string()),
                                ));
                                current_offset += segment_size;
                                xmp_written = true;
                            }
                        }

                        if !jumbf_written && !has_jumbf {
                            if let MetadataUpdate::Set(new_jumbf) = &updates.jumbf {
                                // For c2pa-provided data (already in APP11 format), write directly
                                // Otherwise wrap in JPEG XT format
                                let is_already_app11 = new_jumbf.len() >= 2
                                    && new_jumbf[0] == 0xFF
                                    && new_jumbf[1] == APP11;

                                if is_already_app11 {
                                    // Data already has APP11 markers - treat as opaque blob
                                    // The write function will handle it correctly
                                    // For structure calculation, we just need ONE segment encompassing all the data
                                    dest_structure.add_segment(Segment::new(
                                        current_offset,
                                        new_jumbf.len() as u64,
                                        SegmentKind::Jumbf,
                                        Some("APP11/C2PA".to_string()),
                                    ));
                                    current_offset += new_jumbf.len() as u64;
                                } else {
                                    // Raw JUMBF - need to wrap in JPEG XT
                                    let jumbf_size = new_jumbf.len() as u64;
                                    let mut remaining = jumbf_size;
                                    let mut ranges = Vec::new();

                                    const JPEG_XT_FIRST_OVERHEAD: usize = 8;
                                    const JPEG_XT_CONT_OVERHEAD: usize = 16;
                                    const MAX_DATA: usize = MAX_MARKER_SIZE
                                        - JPEG_XT_FIRST_OVERHEAD
                                        - JPEG_XT_CONT_OVERHEAD;

                                    let mut seg_num = 0;
                                    while remaining > 0 {
                                        let data_in_segment = remaining.min(MAX_DATA as u64);
                                        let overhead = if seg_num == 0 {
                                            JPEG_XT_FIRST_OVERHEAD
                                        } else {
                                            JPEG_XT_CONT_OVERHEAD
                                        };
                                        let segment_size =
                                            2 + 2 + overhead as u64 + data_in_segment;

                                        ranges.push(ByteRange::new(
                                            current_offset + 4 + overhead as u64,
                                            data_in_segment,
                                        ));
                                        current_offset += segment_size;
                                        remaining -= data_in_segment;
                                        seg_num += 1;
                                    }

                                    dest_structure.add_segment_with_ranges(
                                        SegmentKind::Jumbf,
                                        ranges,
                                        Some("APP11/C2PA".to_string()),
                                    );
                                }
                                jumbf_written = true;
                            }
                        }
                    }

                    // Copy other segments as-is
                    let location = segment.location();
                    dest_structure.add_segment(Segment::new(
                        current_offset,
                        location.size,
                        segment.kind,
                        segment.path.clone(),
                    ));
                    current_offset += location.size;
                }
            }
        }

        dest_structure.total_size = current_offset;
        Ok(dest_structure)
    }

    #[cfg(feature = "exif")]
    fn extract_embedded_thumbnail_info<R: Read + Seek>(
        &self,
        structure: &Structure,
        _source: &mut R,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
        // Check if any EXIF segment has an embedded thumbnail
        for segment in structure.segments() {
            if segment.is_type(SegmentKind::Exif) {
                if let Some(info) = segment.thumbnail_info() {
                    return Ok(Some(info.clone()));
                }
            }
        }
        Ok(None)
    }

    #[cfg(feature = "exif")]
    fn extract_exif_info<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<crate::tiff::ExifInfo>> {
        use std::io::SeekFrom;

        // Find EXIF segment
        let exif_segment = structure
            .segments()
            .iter()
            .find(|s| s.is_type(SegmentKind::Exif));

        let segment = match exif_segment {
            Some(s) => s,
            None => return Ok(None),
        };

        // Read the EXIF data
        let location = segment.location();
        source.seek(SeekFrom::Start(location.offset))?;
        let mut data = vec![0u8; location.size as usize];
        source.read_exact(&mut data)?;

        // JPEG: segment includes marker(2) + length(2) + "Exif\0\0"(6) + TIFF data
        // Skip: FF E1 + length(2) + Exif\0\0(6) = 10 bytes
        let exif_data = if data.len() > 10 && &data[4..10] == b"Exif\0\0" {
            &data[10..]
        } else if data.len() > 4 {
            // Maybe just marker + length, data starts at offset 4
            &data[4..]
        } else {
            return Ok(None);
        };

        crate::tiff::parse_exif_info(exif_data)
    }
}

// Helper functions

/// Find End of Image marker (FFD9)
/// Properly handles byte stuffing in JPEG compressed data
fn find_eoi<R: Read + Seek>(source: &mut R) -> Result<u64> {
    const BUFFER_SIZE: usize = 8192;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut prev_was_ff = false;
    let start_pos = source.stream_position()?;
    let mut total_read = 0u64;

    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            return Err(Error::InvalidFormat("EOI marker not found".into()));
        }

        for (i, &byte) in buffer[..n].iter().enumerate() {
            if prev_was_ff {
                if byte == EOI {
                    // Found EOI! Return position of the FF
                    let eoi_pos = start_pos + total_read + i as u64 - 1;
                    return Ok(eoi_pos);
                } else if byte == 0x00 {
                    // Stuffed byte - the FF was part of data, not a marker
                    prev_was_ff = false;
                } else if byte == 0xFF {
                    // Multiple FF bytes (padding) - stay in FF state
                    prev_was_ff = true;
                } else {
                    // Some other marker - not EOI, keep scanning
                    prev_was_ff = false;
                }
            } else if byte == 0xFF {
                prev_was_ff = true;
            }
        }

        total_read += n as u64;
    }
}

/// Write XMP as APP1 segment(s), splitting if needed for large XMP
fn write_xmp_segment<W: Write>(writer: &mut W, xmp: &[u8]) -> Result<()> {
    const MAIN_XMP_MAX: usize = MAX_MARKER_SIZE - XMP_SIGNATURE.len() - 2;

    if xmp.len() <= MAIN_XMP_MAX {
        // Fits in single segment
        let total_size = XMP_SIGNATURE.len() + xmp.len() + 2;
        writer.write_u8(0xFF)?;
        writer.write_u8(APP1)?;
        writer.write_u16::<BigEndian>(total_size as u16)?;
        writer.write_all(XMP_SIGNATURE)?;
        writer.write_all(xmp)?;
        return Ok(());
    }

    // Need to split into main + extended
    // Generate GUID (MD5 of XMP content as hex string)
    let hash = md5::compute(xmp);
    let guid = format!("{:032x}", hash);

    // Create minimal main XMP with HasExtendedXMP property
    let main_xmp = format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:xmpNote="http://ns.adobe.com/xmp/note/">
      <xmpNote:HasExtendedXMP>{}</xmpNote:HasExtendedXMP>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        guid
    );

    // Write main XMP segment
    let main_size = XMP_SIGNATURE.len() + main_xmp.len() + 2;
    writer.write_u8(0xFF)?;
    writer.write_u8(APP1)?;
    writer.write_u16::<BigEndian>(main_size as u16)?;
    writer.write_all(XMP_SIGNATURE)?;
    writer.write_all(main_xmp.as_bytes())?;

    // Write extended XMP in chunks
    const EXTENDED_HEADER_SIZE: usize = XMP_EXTENDED_SIGNATURE.len() + 32 + 4 + 4;
    const CHUNK_SIZE: usize = MAX_MARKER_SIZE - EXTENDED_HEADER_SIZE - 2;

    let total_size = xmp.len() as u32;
    let mut offset = 0u32;

    while offset < total_size {
        let chunk_size = (total_size - offset).min(CHUNK_SIZE as u32);

        // Write segment header
        writer.write_u8(0xFF)?;
        writer.write_u8(APP1)?;

        let segment_size = (EXTENDED_HEADER_SIZE + chunk_size as usize + 2) as u16;
        writer.write_u16::<BigEndian>(segment_size)?;

        // Write extended XMP signature
        writer.write_all(XMP_EXTENDED_SIGNATURE)?;

        // Write GUID (32 bytes as ASCII)
        writer.write_all(guid.as_bytes())?;

        // Write total size (big-endian)
        writer.write_u32::<BigEndian>(total_size)?;

        // Write chunk offset (big-endian)
        writer.write_u32::<BigEndian>(offset)?;

        // Write chunk data
        let chunk_end = (offset + chunk_size) as usize;
        writer.write_all(&xmp[offset as usize..chunk_end])?;

        offset += chunk_size;
    }

    Ok(())
}

/// Write JUMBF data as one or more APP11 segments
fn write_jumbf_segments<W: Write>(writer: &mut W, jumbf: &[u8]) -> Result<()> {
    // Check if the JUMBF data is already in APP11 segment format (complete with FF EB marker)
    // (This happens when c2pa crate returns complete APP11 segments)
    let is_complete_app11 = jumbf.len() >= 2 && jumbf[0] == 0xFF && jumbf[1] == APP11;

    if is_complete_app11 {
        // Data is already in APP11 format with FF EB marker, write directly
        writer.write_all(jumbf)?;
        return Ok(());
    }

    // Check if the JUMBF data is already in JPEG XT format (starts with "JP")
    // (This can happen with some JUMBF sources, but note that when Keeping existing JUMBF,
    // we now copy the complete APP11 segments directly in the write() function instead of
    // going through this function, so this path is mainly for completeness)
    let is_already_jpeg_xt = jumbf.len() >= 2 && &jumbf[0..2] == b"JP";

    if is_already_jpeg_xt {
        // Data is in JPEG XT format payload, just needs APP11 marker + length wrapper
        const MAX_SEGMENT_PAYLOAD: usize = MAX_MARKER_SIZE - 2; // Minus the 2-byte length field

        for chunk in jumbf.chunks(MAX_SEGMENT_PAYLOAD) {
            writer.write_u8(0xFF)?;
            writer.write_u8(APP11)?;

            // Length includes the 2-byte length field itself plus the chunk
            let seg_size = 2 + chunk.len();
            writer.write_u16::<BigEndian>(seg_size as u16)?;

            // Write the JPEG XT formatted chunk as-is
            writer.write_all(chunk)?;
        }

        return Ok(());
    }

    // Otherwise, wrap raw JUMBF boxes in JPEG XT format
    // JPEG XT header for first segment: JP (2) + En (2) + Z (4) = 8 bytes
    // For continuation segments, we also repeat LBox (4) + TBox (4) = 8 more bytes
    const JPEG_XT_HEADER: usize = 8; // JP + En + Z
    const LBOX_TBOX_SIZE: usize = 8; // LBox + TBox repeated in continuations
    const MAX_DATA_PER_SEGMENT: usize = MAX_MARKER_SIZE - JPEG_XT_HEADER - LBOX_TBOX_SIZE;

    for (seg_num, chunk) in jumbf.chunks(MAX_DATA_PER_SEGMENT).enumerate() {
        writer.write_u8(0xFF)?;
        writer.write_u8(APP11)?;

        // For first segment: JPEG_XT_HEADER + chunk
        // For continuation: JPEG_XT_HEADER + LBOX_TBOX + chunk
        let continuation_overhead = if seg_num > 0 { LBOX_TBOX_SIZE } else { 0 };
        let seg_size = JPEG_XT_HEADER + continuation_overhead + chunk.len() + 2;
        writer.write_u16::<BigEndian>(seg_size as u16)?;

        // JPEG XT header
        writer.write_all(b"JP")?; // CI: JPEG extensions marker
        writer.write_u16::<BigEndian>(0x0211)?; // En: Box Instance Number
        writer.write_u32::<BigEndian>((seg_num + 1) as u32)?; // Z: Packet sequence

        // For continuation segments, repeat LBox and TBox
        if seg_num > 0 && jumbf.len() >= 8 {
            writer.write_all(&jumbf[0..8])?; // LBox + TBox
        }

        writer.write_all(chunk)?;
    }

    Ok(())
}

/// Write JUMBF data with proper C2PA exclusion handling for ProcessingWriter
///
/// Per C2PA spec, the APP11 headers must be included in the hash to prevent
/// insertion attacks. Only the manifest DATA is excluded from hashing.
///
/// This function:
/// 1. Writes headers (marker, length, JPEG XT fields) with processing ENABLED
/// 2. Enables exclude mode
/// 3. Writes the JUMBF data with processing DISABLED
/// 4. Disables exclude mode
fn write_jumbf_with_exclusion<W: Write, F: FnMut(&[u8])>(
    pw: &mut crate::processing_writer::ProcessingWriter<W, F>,
    jumbf: &[u8],
    should_exclude: bool,
) -> Result<()> {
    // Check if already in APP11 format
    let is_complete_app11 = jumbf.len() >= 2 && jumbf[0] == 0xFF && jumbf[1] == APP11;
    if is_complete_app11 {
        // For pre-formatted APP11 data, we can't easily separate headers from data
        // This path shouldn't be used for C2PA (format should be "application/c2pa")
        if should_exclude {
            pw.set_exclude_mode(true);
        }
        pw.write_all(jumbf)?;
        if should_exclude {
            pw.set_exclude_mode(false);
        }
        return Ok(());
    }

    // Check if in JPEG XT payload format (starts with "JP")
    let is_already_jpeg_xt = jumbf.len() >= 2 && &jumbf[0..2] == b"JP";
    if is_already_jpeg_xt {
        // Similar - can't easily separate
        const MAX_SEGMENT_PAYLOAD: usize = MAX_MARKER_SIZE - 2;
        for chunk in jumbf.chunks(MAX_SEGMENT_PAYLOAD) {
            // Write marker + length (included in hash)
            pw.write_u8(0xFF)?;
            pw.write_u8(APP11)?;
            let seg_size = 2 + chunk.len();
            pw.write_u16::<BigEndian>(seg_size as u16)?;

            // Exclude the JPEG XT payload
            if should_exclude {
                pw.set_exclude_mode(true);
            }
            pw.write_all(chunk)?;
            if should_exclude {
                pw.set_exclude_mode(false);
            }
        }
        return Ok(());
    }

    // Raw JUMBF - wrap in JPEG XT format with proper exclusion
    const JPEG_XT_HEADER: usize = 8; // JP + En + Z
    const LBOX_TBOX_SIZE: usize = 8; // LBox + TBox repeated in continuations
    const MAX_DATA_PER_SEGMENT: usize = MAX_MARKER_SIZE - JPEG_XT_HEADER - LBOX_TBOX_SIZE;

    for (seg_num, chunk) in jumbf.chunks(MAX_DATA_PER_SEGMENT).enumerate() {
        // Write marker (included in hash)
        pw.write_u8(0xFF)?;
        pw.write_u8(APP11)?;

        // Write length field (included in hash)
        let continuation_overhead = if seg_num > 0 { LBOX_TBOX_SIZE } else { 0 };
        let seg_size = JPEG_XT_HEADER + continuation_overhead + chunk.len() + 2;
        pw.write_u16::<BigEndian>(seg_size as u16)?;

        // Write JPEG XT header (included in hash per C2PA spec)
        pw.write_all(b"JP")?;
        pw.write_u16::<BigEndian>(0x0211)?;
        pw.write_u32::<BigEndian>((seg_num + 1) as u32)?;

        // For continuation segments, LBox+TBox header is also included in hash
        if seg_num > 0 && jumbf.len() >= 8 {
            pw.write_all(&jumbf[0..8])?;
        }

        // NOW enable exclusion for the actual JUMBF data
        if should_exclude {
            pw.set_exclude_mode(true);
        }

        pw.write_all(chunk)?;

        // Disable exclusion after data
        if should_exclude {
            pw.set_exclude_mode(false);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_jpeg_parse_minimal() {
        // Minimal JPEG: SOI + EOI
        let data = vec![0xFF, 0xD8, 0xFF, 0xD9];
        let mut source = Cursor::new(data);

        let handler = JpegIO::new();
        let structure = handler.parse(&mut source).unwrap();

        assert_eq!(structure.container, Container::Jpeg);
        assert_eq!(structure.total_size, 4);
        assert_eq!(structure.segments.len(), 2); // Header + EOI
    }
}
