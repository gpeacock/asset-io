//! JPEG container I/O implementation

use crate::{
    error::{Error, Result},
    segment::{LazyData, Location, Segment},
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

        if let crate::Segment::Xmp {
            segments, metadata, ..
        } = &structure.segments()[index]
        {
            // Check if this has extended XMP metadata
            if let Some(meta) = metadata {
                if let Some((guid, chunk_offsets, total_size)) = meta.as_jpeg_extended_xmp() {
                    // JPEG Extended XMP - reassemble from parts
                    return Self::reassemble_extended_xmp(
                        source,
                        segments,
                        guid,
                        chunk_offsets,
                        total_size,
                    );
                }
            }

            // Simple case: single segment or concatenated segments
            if segments.len() == 1 {
                // Single XMP segment
                source.seek(SeekFrom::Start(segments[0].offset))?;
                let mut xmp_data = vec![0u8; segments[0].size as usize];
                source.read_exact(&mut xmp_data)?;
                return Ok(Some(xmp_data));
            } else {
                // Multiple segments - concatenate them
                let total_size: u64 = segments.iter().map(|s| s.size).sum();
                let mut xmp_data = Vec::with_capacity(total_size as usize);
                for segment in segments {
                    source.seek(SeekFrom::Start(segment.offset))?;
                    let mut chunk = vec![0u8; segment.size as usize];
                    source.read_exact(&mut chunk)?;
                    xmp_data.extend_from_slice(&chunk);
                }
                return Ok(Some(xmp_data));
            }
        }

        Ok(None)
    }

    /// Reassemble JPEG Extended XMP from multiple parts using chunk offsets
    fn reassemble_extended_xmp<R: Read + Seek>(
        source: &mut R,
        segments: &[Location],
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

        // Skip first segment (it's the main XMP with pointer)
        if segments.len() < 2 || chunk_offsets.len() != segments.len() - 1 {
            // Malformed - should have main segment + extended segments
            // Fall back to reading just the first segment
            if !segments.is_empty() {
                source.seek(SeekFrom::Start(segments[0].offset))?;
                let mut xmp_data = vec![0u8; segments[0].size as usize];
                source.read_exact(&mut xmp_data)?;
                return Ok(Some(xmp_data));
            }
            return Ok(None);
        }

        // Allocate buffer for complete extended XMP
        let mut extended_xmp = vec![0u8; total_size as usize];

        // Read each extended chunk into the correct position
        // Note: segments[0] is main XMP, segments[1..] are extended parts
        for (segment, &chunk_offset) in segments[1..].iter().zip(chunk_offsets) {
            source.seek(SeekFrom::Start(segment.offset))?;
            let end_pos = (chunk_offset as usize + segment.size as usize).min(extended_xmp.len());
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
        const JPEG_XT_HEADER_SIZE: usize = 8;

        for &index in structure.jumbf_indices() {
            if let crate::Segment::Jumbf {
                offset,
                size,
                segments,
                ..
            } = &structure.segments()[index]
            {
                if segments.len() > 1 {
                    // Multi-segment JUMBF: strip JPEG XT headers
                    for (i, loc) in segments.iter().enumerate() {
                        source.seek(SeekFrom::Start(loc.offset))?;

                        // First segment: skip JPEG XT header (8 bytes)
                        // Continuation segments: skip JPEG XT header + repeated LBox/TBox (16 bytes total)
                        let skip_bytes = if i == 0 {
                            JPEG_XT_HEADER_SIZE
                        } else {
                            JPEG_XT_HEADER_SIZE + 8 // Skip JPEG XT header + repeated LBox/TBox
                        };

                        let data_size = loc.size.saturating_sub(skip_bytes as u64);
                        if data_size > 0 {
                            let mut skip_buf = vec![0u8; skip_bytes];
                            source.read_exact(&mut skip_buf)?; // Skip the header

                            let mut buf = vec![0u8; data_size as usize];
                            source.read_exact(&mut buf)?;
                            result.extend_from_slice(&buf);
                        }
                    }
                } else {
                    // Single segment: skip JPEG XT header
                    source.seek(SeekFrom::Start(*offset))?;

                    let mut skip_buf = [0u8; JPEG_XT_HEADER_SIZE];
                    source.read_exact(&mut skip_buf)?;

                    let data_size = size.saturating_sub(JPEG_XT_HEADER_SIZE as u64);
                    if data_size > 0 {
                        let mut buf = vec![0u8; data_size as usize];
                        source.read_exact(&mut buf)?;
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

    /// Fast single-pass parser
    fn parse_impl<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        let mut structure = Structure::new(Container::Jpeg, crate::MediaType::Jpeg);

        // Check SOI marker
        if source.read_u8()? != 0xFF || source.read_u8()? != SOI {
            return Err(Error::InvalidFormat("Not a JPEG file".into()));
        }

        structure.add_segment(Segment::Header { offset: 0, size: 2 });

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
                    structure.add_segment(Segment::Other {
                        offset,
                        size: 2,
                        label: marker_label(EOI),
                    });
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
                    structure.add_segment(Segment::ImageData {
                        offset: sos_start,
                        size: image_end - sos_start,
                    });

                    // Add EOI segment
                    structure.add_segment(Segment::Other {
                        offset: image_end,
                        size: 2,
                        label: marker_label(EOI),
                    });

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
                        structure.add_segment(Segment::Xmp {
                            offset: xmp_offset,
                            size: xmp_size,
                            segments: vec![Location {
                                offset: xmp_offset,
                                size: xmp_size,
                            }],
                            data: LazyData::NotLoaded,
                            metadata: None,
                        });
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
                            structure.add_segment(Segment::Other {
                                offset: segment_start,
                                size: size + 2,
                                label: marker_label(APP1),
                            });
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
                                if let Segment::Xmp {
                                    segments, metadata, ..
                                } = &mut structure.segments[idx]
                                {
                                    // Add location to segments
                                    segments.push(Location {
                                        offset: chunk_data_offset,
                                        size: chunk_data_size,
                                    });

                                    // Update or create metadata
                                    match metadata {
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
                                            *metadata =
                                                Some(crate::SegmentMetadata::JpegExtendedXmp {
                                                    guid,
                                                    chunk_offsets: vec![chunk_offset],
                                                    total_size,
                                                });
                                        }
                                    }
                                }
                            } else {
                                // Extended XMP without main XMP - malformed but handle gracefully
                                structure.add_segment(Segment::Other {
                                    offset: segment_start,
                                    size: size + 2,
                                    label: marker_label(APP1),
                                });
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
                                        // Create EmbeddedThumbnail with location relative to EXIF segment start
                                        let thumb_offset = segment_start
                                            + 4
                                            + EXIF_SIGNATURE.len() as u64
                                            + thumb_info.offset as u64;
                                        Some(crate::thumbnail::EmbeddedThumbnail::new(
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

                                structure.add_segment(Segment::Exif {
                                    offset: segment_start,
                                    size: size + 2,
                                    thumbnail,
                                });
                            }

                            #[cfg(not(feature = "exif"))]
                            {
                                // Just record the EXIF segment without parsing thumbnails
                                structure.add_segment(Segment::Exif {
                                    offset: segment_start,
                                    size: size + 2,
                                });

                                // Skip remaining EXIF data
                                let remaining = (data_size as usize) - sig_buf.len();
                                source.seek(SeekFrom::Current(remaining as i64))?;
                            }
                        } else {
                            // Other APP1 segment
                            let remaining = (data_size as usize) - sig_buf.len();
                            source.seek(SeekFrom::Current(remaining as i64))?;
                            structure.add_segment(Segment::Other {
                                offset: segment_start,
                                size: size + 2,
                                label: marker_label(APP1),
                            });
                        }

                        // If not XMP or EXIF, treat as Other APP1 segment
                        if sig_buf.len() < EXIF_SIGNATURE.len()
                            || &sig_buf[..EXIF_SIGNATURE.len()] != EXIF_SIGNATURE
                        {
                            // Other APP1 segment (probably EXIF)
                            let remaining = (data_size as usize) - sig_buf.len();
                            source.seek(SeekFrom::Current(remaining as i64))?;
                            structure.add_segment(Segment::Other {
                                offset: segment_start,
                                size: size + 2,
                                label: marker_label(APP1),
                            });
                        }
                    }

                    offset += 2 + size;
                }

                APP11 => {
                    let size = source.read_u16::<BigEndian>()? as u64;
                    let data_size = size - 2;
                    let data_start = offset + 4;
                    let segment_start = offset;

                    // Check for JPEG XT + JUMBF structure
                    // JPEG XT: CI(2) + En(2) + Z(4) = 8 bytes
                    // JUMBF superbox: LBox(4) + TBox(4 "jumb")
                    let mut header = [0u8; 32];
                    let bytes_to_read = header.len().min(data_size as usize);
                    source.read_exact(&mut header[..bytes_to_read])?;

                    // Check if this is JPEG XT with JUMBF
                    let is_jpeg_xt = bytes_to_read >= 8 && &header[0..2] == b"JP";
                    let has_jumb_box = bytes_to_read >= 16 && &header[12..16] == b"jumb";
                    let has_c2pa = bytes_to_read >= 32 && &header[28..32] == C2PA_MARKER;

                    let is_jumbf = is_jpeg_xt && (has_jumb_box || has_c2pa);

                    if is_jumbf {
                        // Extract sequence number from JPEG XT header
                        let seq_num = if bytes_to_read >= 8 {
                            u32::from_be_bytes([header[4], header[5], header[6], header[7]])
                        } else {
                            1
                        };

                        // Check if this is a continuation of the previous JUMBF segment
                        let mut is_continuation = false;
                        if seq_num > 1 {
                            if let Some(Segment::Jumbf {
                                segments,
                                size: total_size,
                                ..
                            }) = structure.segments.last_mut()
                            {
                                // Add this segment to the existing JUMBF
                                segments.push(Location {
                                    offset: data_start,
                                    size: data_size,
                                });
                                *total_size += data_size;
                                is_continuation = true;
                            }
                        }

                        if !is_continuation {
                            // New JUMBF segment
                            structure.add_segment(Segment::Jumbf {
                                offset: data_start,
                                size: data_size,
                                segments: vec![Location {
                                    offset: data_start,
                                    size: data_size,
                                }],
                                data: LazyData::NotLoaded,
                            });
                        }

                        // Skip remaining JUMBF data
                        let remaining = data_size - bytes_to_read as u64;
                        source.seek(SeekFrom::Current(remaining as i64))?;
                    } else {
                        // Other APP11 segment - skip remaining data
                        let remaining = data_size - bytes_to_read as u64;
                        source.seek(SeekFrom::Current(remaining as i64))?;
                        structure.add_segment(Segment::Other {
                            offset: segment_start,
                            size: size + 2,
                            label: marker_label(APP11),
                        });
                    }

                    offset += 2 + size;
                }

                // RST markers have no length
                RST0..=RST7 => {
                    structure.add_segment(Segment::Other {
                        offset,
                        size: 2,
                        label: marker_label(marker),
                    });
                    offset += 2;
                }

                _ => {
                    // Standard marker with length
                    let size = source.read_u16::<BigEndian>()? as u64;
                    structure.add_segment(Segment::Other {
                        offset,
                        size: size + 2,
                        label: marker_label(marker),
                    });
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
        source.seek(SeekFrom::Start(0))?;
        let mut current_read_pos = 0u64;

        // Write SOI
        writer.write_u8(0xFF)?;
        writer.write_u8(SOI)?;

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing XMP/JUMBF
        let has_xmp = structure
            .segments
            .iter()
            .any(|s| matches!(s, Segment::Xmp { .. }));
        let has_jumbf = structure
            .segments
            .iter()
            .any(|s| matches!(s, Segment::Jumbf { .. }));

        for segment in &structure.segments {
            match segment {
                Segment::Header { .. } => {
                    // Already wrote SOI
                    continue;
                }

                Segment::Xmp {
                    segments, metadata, ..
                } => {
                    match &updates.xmp {
                        crate::XmpUpdate::Remove => {
                            // Skip existing XMP - effectively removing it
                            xmp_written = true;
                        }
                        crate::XmpUpdate::Set(new_xmp) => {
                            // Replace with new XMP
                            if !xmp_written {
                                write_xmp_segment(writer, new_xmp)?;
                                xmp_written = true;
                            }
                            // Skip existing XMP
                        }
                        crate::XmpUpdate::Keep => {
                            // Check if this is JPEG Extended XMP
                            if let Some(meta) = metadata {
                                if let Some((guid, chunk_offsets, total_size)) =
                                    meta.as_jpeg_extended_xmp()
                                {
                                    // Write main XMP segment (first in segments)
                                    if !segments.is_empty() {
                                        writer.write_u8(0xFF)?;
                                        writer.write_u8(APP1)?;
                                        writer.write_u16::<BigEndian>(
                                            (segments[0].size + XMP_SIGNATURE.len() as u64 + 2)
                                                as u16,
                                        )?;
                                        writer.write_all(XMP_SIGNATURE)?;

                                        if current_read_pos != segments[0].offset {
                                            source.seek(SeekFrom::Start(segments[0].offset))?;
                                            current_read_pos = segments[0].offset;
                                        }

                                        let mut limited = source.take(segments[0].size);
                                        copy(&mut limited, writer)?;
                                        current_read_pos += segments[0].size;
                                    }

                                    // Write extended XMP segments (segments[1..])
                                    for (i, segment) in segments[1..].iter().enumerate() {
                                        let chunk_offset =
                                            chunk_offsets.get(i).copied().unwrap_or(0);

                                        // Write APP1 marker
                                        writer.write_u8(0xFF)?;
                                        writer.write_u8(APP1)?;

                                        // Calculate segment size: signature + GUID + total_size + offset + data
                                        let seg_size = XMP_EXTENDED_SIGNATURE.len()
                                            + 32
                                            + 4
                                            + 4
                                            + segment.size as usize
                                            + 2;
                                        writer.write_u16::<BigEndian>(seg_size as u16)?;

                                        // Write extended XMP signature
                                        writer.write_all(XMP_EXTENDED_SIGNATURE)?;

                                        // Write GUID (pad to 32 bytes if needed)
                                        let guid_bytes = guid.as_bytes();
                                        writer
                                            .write_all(&guid_bytes[..guid_bytes.len().min(32)])?;
                                        // Pad if GUID is shorter than 32 bytes
                                        for _ in guid_bytes.len()..32 {
                                            writer.write_u8(0)?;
                                        }

                                        // Write total size
                                        writer.write_u32::<BigEndian>(total_size)?;

                                        // Write chunk offset
                                        writer.write_u32::<BigEndian>(chunk_offset)?;

                                        // Copy the data
                                        if current_read_pos != segment.offset {
                                            source.seek(SeekFrom::Start(segment.offset))?;
                                            current_read_pos = segment.offset;
                                        }

                                        let mut limited = source.take(segment.size);
                                        copy(&mut limited, writer)?;
                                        current_read_pos += segment.size;
                                    }

                                    xmp_written = true;
                                    continue;
                                }
                            }

                            // Simple XMP (single or concatenated segments, no extended metadata)
                            for segment in segments {
                                writer.write_u8(0xFF)?;
                                writer.write_u8(APP1)?;
                                writer.write_u16::<BigEndian>(
                                    (segment.size + XMP_SIGNATURE.len() as u64 + 2) as u16,
                                )?;
                                writer.write_all(XMP_SIGNATURE)?;

                                if current_read_pos != segment.offset {
                                    source.seek(SeekFrom::Start(segment.offset))?;
                                    current_read_pos = segment.offset;
                                }

                                let mut limited = source.take(segment.size);
                                copy(&mut limited, writer)?;
                                current_read_pos += segment.size;
                            }

                            xmp_written = true;
                        }
                    }
                }

                Segment::Jumbf { segments, .. } => {
                    match &updates.jumbf {
                        crate::JumbfUpdate::Remove => {
                            // Skip existing JUMBF - effectively removing it
                            jumbf_written = true;
                        }
                        crate::JumbfUpdate::Set(new_jumbf) => {
                            // Replace with new JUMBF
                            if !jumbf_written {
                                write_jumbf_segments(writer, new_jumbf)?;
                                jumbf_written = true;
                            }
                            // Skip existing JUMBF
                        }
                        crate::JumbfUpdate::Keep => {
                            // Copy existing JUMBF segments
                            for loc in segments.iter() {
                                writer.write_u8(0xFF)?;
                                writer.write_u8(APP11)?;

                                let seg_size = loc.size + 2;
                                writer.write_u16::<BigEndian>(seg_size as u16)?;

                                // Optimized seek
                                if current_read_pos != loc.offset {
                                    source.seek(SeekFrom::Start(loc.offset))?;
                                    current_read_pos = loc.offset;
                                }

                                let mut limited = source.take(loc.size);
                                copy(&mut limited, writer)?;
                                current_read_pos += loc.size;
                            }
                            jumbf_written = true;
                        }
                    }
                }

                Segment::Other { label, .. } if *label == "APP1" && !xmp_written => {
                    // First APP1 segment - good place to insert XMP if we're adding it
                    if let crate::XmpUpdate::Set(new_xmp) = &updates.xmp {
                        if !has_xmp {
                            // Insert new XMP before this segment
                            write_xmp_segment(writer, new_xmp)?;
                            xmp_written = true;
                        }
                    }

                    // Copy the Other segment
                    copy_other_segment(segment, source, writer, &mut current_read_pos)?;
                }

                Segment::Other { label, .. } if *label == "APP11" && !jumbf_written => {
                    // First APP11 segment - good place to insert JUMBF if we're adding it
                    if let crate::JumbfUpdate::Set(new_jumbf) = &updates.jumbf {
                        if !has_jumbf {
                            // Insert new JUMBF before this segment
                            write_jumbf_segments(writer, new_jumbf)?;
                            jumbf_written = true;
                        }
                    }

                    // Copy the Other segment
                    copy_other_segment(segment, source, writer, &mut current_read_pos)?;
                }

                Segment::ImageData { .. } => {
                    // Before writing image data, insert any pending new metadata
                    if !xmp_written {
                        if let crate::XmpUpdate::Set(new_xmp) = &updates.xmp {
                            if !has_xmp {
                                write_xmp_segment(writer, new_xmp)?;
                                xmp_written = true;
                            }
                        }
                    }

                    if !jumbf_written {
                        if let crate::JumbfUpdate::Set(new_jumbf) = &updates.jumbf {
                            if !has_jumbf {
                                write_jumbf_segments(writer, new_jumbf)?;
                                jumbf_written = true;
                            }
                        }
                    }

                    // Copy image data
                    copy_other_segment(segment, source, writer, &mut current_read_pos)?;
                }

                Segment::Exif { .. } => {
                    // Copy EXIF segment as-is (thumbnails are embedded in it)
                    copy_other_segment(segment, source, writer, &mut current_read_pos)?;
                }

                Segment::Other { .. } => {
                    copy_other_segment(segment, source, writer, &mut current_read_pos)?;
                }
            }
        }

        Ok(())
    }

    #[cfg(feature = "exif")]
    fn extract_embedded_thumbnail<R: Read + Seek>(
        &self,
        structure: &Structure,
        _source: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>> {
        // Check if any EXIF segment has an embedded thumbnail
        for segment in structure.segments() {
            if let Segment::Exif { thumbnail, .. } = segment {
                return Ok(thumbnail.clone());
            }
        }
        Ok(None)
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

/// Helper to copy a segment (ImageData or Other)
fn copy_other_segment<R: Read + Seek, W: Write>(
    segment: &Segment,
    source: &mut R,
    writer: &mut W,
    current_read_pos: &mut u64,
) -> Result<()> {
    let (offset, size) = match segment {
        Segment::ImageData { offset, size, .. } => (*offset, *size),
        Segment::Other { offset, size, .. } => (*offset, *size),
        _ => return Ok(()), // Shouldn't happen, but handle gracefully
    };

    // Optimized seek
    if *current_read_pos != offset {
        source.seek(SeekFrom::Start(offset))?;
        *current_read_pos = offset;
    }

    let mut limited = source.take(size);
    copy(&mut limited, writer)?;
    *current_read_pos += size;

    Ok(())
}

/// Write JUMBF data as one or more APP11 segments
fn write_jumbf_segments<W: Write>(writer: &mut W, jumbf: &[u8]) -> Result<()> {
    // JPEG XT header: CI (2) + En (2) + Z (4) + LBox (4) + TBox (4) = 16 bytes
    const JPEG_XT_OVERHEAD: usize = 16;
    const MAX_DATA_PER_SEGMENT: usize = MAX_MARKER_SIZE - JPEG_XT_OVERHEAD;

    for (seg_num, chunk) in jumbf.chunks(MAX_DATA_PER_SEGMENT).enumerate() {
        writer.write_u8(0xFF)?;
        writer.write_u8(APP11)?;

        let seg_size = chunk.len() + JPEG_XT_OVERHEAD + 2;
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
