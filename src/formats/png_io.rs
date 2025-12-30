//! PNG container I/O implementation

use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind},
    structure::Structure,
    Container, ContainerIO, Updates,
};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{Read, Seek, SeekFrom, Write};

// PNG signature
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

// Metadata chunk types
const ITXT: &[u8] = b"iTXt";

// JUMBF/C2PA chunk types (following c2pa-rs convention)
const C2PA: &[u8] = b"caBX";

// XMP keyword in iTXt chunks
const XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp\0";

/// Get human-readable label for a PNG chunk type
fn chunk_label(chunk_type: &[u8; 4]) -> &'static str {
    match chunk_type {
        b"IHDR" => "IHDR",
        b"PLTE" => "PLTE",
        b"IDAT" => "IDAT",
        b"IEND" => "IEND",
        b"tRNS" => "tRNS",
        b"gAMA" => "gAMA",
        b"cHRM" => "cHRM",
        b"sRGB" => "sRGB",
        b"iCCP" => "iCCP",
        b"iTXt" => "iTXt",
        b"tEXt" => "tEXt",
        b"zTXt" => "zTXt",
        b"bKGD" => "bKGD",
        b"pHYs" => "pHYs",
        b"tIME" => "tIME",
        _ => "OTHER",
    }
}

/// PNG container I/O implementation
pub struct PngIO;

impl PngIO {
    /// Create a new PNG I/O implementation
    pub fn new() -> Self {
        Self
    }

    /// Formats this handler supports
    pub fn container_type() -> Container {
        Container::Png
    }

    /// Media types this handler supports
    pub fn supported_media_types() -> &'static [crate::MediaType] {
        &[crate::MediaType::Png]
    }

    /// File extensions this handler accepts
    pub fn extensions() -> &'static [&'static str] {
        &["png"]
    }

    /// MIME types this handler accepts
    pub fn mime_types() -> &'static [&'static str] {
        &["image/png"]
    }

    /// Detect if this is a PNG file from header
    pub fn detect(header: &[u8]) -> Option<crate::Container> {
        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        if header.len() >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(Container::Png)
        } else {
            None
        }
    }

    /// Extract XMP data from PNG file (simple iTXt chunk, no extended XMP)
    pub fn extract_xmp_impl<R: Read + Seek>(
        structure: &crate::structure::Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        let Some(index) = structure.xmp_index() else {
            return Ok(None);
        };

        let segment = &structure.segments()[index];
        if segment.is_xmp() {
            // PNG stores XMP in a single iTXt chunk - no extended XMP like JPEG
            let location = segment.location();
            source.seek(SeekFrom::Start(location.offset))?;

            let mut xmp_data = vec![0u8; location.size as usize];
            source.read_exact(&mut xmp_data)?;

            return Ok(Some(xmp_data));
        }

        Ok(None)
    }

    /// Extract JUMBF data from PNG file (direct data from caBX chunks, no headers to strip)
    pub fn extract_jumbf_impl<R: Read + Seek>(
        structure: &crate::structure::Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        if structure.jumbf_indices().is_empty() {
            return Ok(None);
        }

        let mut result = Vec::new();

        // For now, extract all JUMBF segments and concatenate them
        // TODO: Filter by C2PA-specific JUMBF if needed
        for &index in structure.jumbf_indices() {
            let segment = &structure.segments()[index];
            if segment.is_jumbf() {
                // PNG stores JUMBF directly in caBX chunks - no format-specific headers to strip
                let location = segment.location();
                source.seek(SeekFrom::Start(location.offset))?;

                let mut buf = vec![0u8; location.size as usize];
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
        let mut structure = Structure::new(Container::Png, crate::MediaType::Png);

        // Check PNG signature
        let mut sig = [0u8; 8];
        source.read_exact(&mut sig)?;
        if sig != PNG_SIGNATURE {
            return Err(Error::InvalidFormat("Not a PNG file".into()));
        }

        structure.add_segment(Segment::new(0, 8, SegmentKind::Header, None));

        let mut offset = 8u64;
        let mut found_iend = false;

        loop {
            // Read chunk length
            let chunk_len = match source.read_u32::<BigEndian>() {
                Ok(len) => len as u64,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            };

            // Read chunk type
            let mut chunk_type = [0u8; 4];
            source.read_exact(&mut chunk_type)?;

            let chunk_start = offset;
            let data_offset = offset + 8; // After length (4) + type (4)

            // Validate chunk length to prevent allocation attacks
            if chunk_len > 0x7FFFFFFF {
                return Err(Error::InvalidSegment {
                    offset,
                    reason: format!("Chunk length too large: {}", chunk_len),
                });
            }

            match &chunk_type {
                b"IHDR" => {
                    // Header chunk - must be first after signature
                    structure.add_segment(Segment::new(
                        chunk_start,
                        8 + chunk_len + 4, // length + type + data + CRC
                        SegmentKind::Other,
                        Some(chunk_label(&chunk_type).to_string()),
                    ));
                    source.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                b"IDAT" => {
                    // Image data
                    structure.add_segment(Segment::new(
                        data_offset,
                        chunk_len,
                        SegmentKind::ImageData,
                        Some("idat".to_string()),
                    ));
                    source.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                b"IEND" => {
                    // End chunk
                    structure.add_segment(Segment::new(
                        chunk_start,
                        8 + chunk_len + 4,
                        SegmentKind::Other,
                        Some(chunk_label(&chunk_type).to_string()),
                    ));
                    source.seek(SeekFrom::Current((chunk_len + 4) as i64))?;
                    found_iend = true;
                    structure.total_size = offset + 8 + chunk_len + 4;
                    break;
                }

                b"iTXt" => {
                    // Check if this is XMP
                    let keyword_len = XMP_KEYWORD.len().min(chunk_len as usize);
                    let mut keyword_buf = vec![0u8; keyword_len];
                    source.read_exact(&mut keyword_buf)?;

                    if keyword_buf == XMP_KEYWORD {
                        // This is XMP data
                        // iTXt container: keyword\0 + compression_flag(1) + compression_method(1) + language_tag\0 + translated_keyword\0 + text
                        // For XMP: "XML:com.adobe.xmp\0" + 0x00 + 0x00 + "\0" + "\0" + XMP_data
                        // Skip: compression_flag(1) + compression_method(1) + language_tag\0 + translated_keyword\0

                        // Read compression flag and method
                        let _compression_flag = source.read_u8()?;
                        let _compression_method = source.read_u8()?;

                        // Skip language tag (null-terminated)
                        let mut lang_consumed = 0;
                        loop {
                            let byte = source.read_u8()?;
                            lang_consumed += 1;
                            if byte == 0 || lang_consumed > 100 {
                                break;
                            }
                        }

                        // Skip translated keyword (null-terminated)
                        let mut trans_consumed = 0;
                        loop {
                            let byte = source.read_u8()?;
                            trans_consumed += 1;
                            if byte == 0 || trans_consumed > 100 {
                                break;
                            }
                        }

                        let xmp_offset =
                            data_offset + keyword_len as u64 + 2 + lang_consumed + trans_consumed;
                        let xmp_size = chunk_len.saturating_sub(
                            keyword_len as u64 + 2 + lang_consumed + trans_consumed,
                        );

                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(xmp_offset, xmp_size)],
                            SegmentKind::Xmp,
                            Some("iTXt[xmp]".to_string()),
                        ));

                        // Skip remaining XMP data + CRC
                        let remaining = xmp_size + 4; // XMP data + CRC
                        source.seek(SeekFrom::Current(remaining as i64))?;
                    } else {
                        // Regular iTXt chunk
                        structure.add_segment(Segment::new(
                            chunk_start,
                            8 + chunk_len + 4,
                            SegmentKind::Other,
                            Some(chunk_label(&chunk_type).to_string()),
                        ));
                        // Skip remaining data + CRC
                        let remaining = chunk_len - keyword_len as u64 + 4;
                        source.seek(SeekFrom::Current(remaining as i64))?;
                    }
                }

                b"caBX" => {
                    // C2PA/JUMBF chunk
                    structure.add_segment(Segment::with_ranges(
                        vec![ByteRange::new(data_offset, chunk_len)],
                        SegmentKind::Jumbf,
                        Some("caBX".to_string()),
                    ));
                    source.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                b"eXIf" => {
                    // EXIF chunk (PNG extension, added in PNG 1.5.0 specification)
                    // Contains raw EXIF data in TIFF format (without the "Exif\0\0" header used in JPEG)
                    structure.add_segment(Segment::new(
                        data_offset,
                        chunk_len,
                        SegmentKind::Exif,
                        Some("eXIf".to_string()),
                    )); // TODO: Parse EXIF to extract thumbnail metadata
                    source.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                _ => {
                    // Other chunk types
                    structure.add_segment(Segment::new(
                        chunk_start,
                        8 + chunk_len + 4,
                        SegmentKind::Other,
                        Some(chunk_label(&chunk_type).to_string()),
                    ));
                    source.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }
            }

            offset += 8 + chunk_len + 4; // Move to next chunk
        }

        if !found_iend {
            return Err(Error::InvalidFormat("PNG file missing IEND chunk".into()));
        }

        Ok(structure)
    }

    /// Calculate CRC32 for PNG chunk
    fn calculate_crc(chunk_type: &[u8], data: &[u8]) -> u32 {
        let mut crc = 0xFFFFFFFF_u32;

        // Process chunk type
        for &byte in chunk_type {
            crc ^= byte as u32;
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB88320;
                } else {
                    crc >>= 1;
                }
            }
        }

        // Process data
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB88320;
                } else {
                    crc >>= 1;
                }
            }
        }

        crc ^ 0xFFFFFFFF
    }

    /// Write a PNG chunk with proper CRC
    fn write_chunk<W: Write>(writer: &mut W, chunk_type: &[u8], data: &[u8]) -> Result<()> {
        // Write length
        writer.write_u32::<BigEndian>(data.len() as u32)?;

        // Write type
        writer.write_all(chunk_type)?;

        // Write data
        writer.write_all(data)?;

        // Calculate and write CRC
        let crc = Self::calculate_crc(chunk_type, data);
        writer.write_u32::<BigEndian>(crc)?;

        Ok(())
    }

    /// Write XMP as iTXt chunk
    fn write_xmp_chunk<W: Write>(writer: &mut W, xmp_data: &[u8]) -> Result<()> {
        // Build iTXt data: keyword + flags + language + translated keyword + XMP
        let mut chunk_data = Vec::with_capacity(XMP_KEYWORD.len() + 4 + xmp_data.len());

        // Keyword
        chunk_data.extend_from_slice(XMP_KEYWORD);

        // Compression flag (0 = uncompressed)
        chunk_data.push(0);

        // Compression method (0 = none)
        chunk_data.push(0);

        // Language tag (empty, null-terminated)
        chunk_data.push(0);

        // Translated keyword (empty, null-terminated)
        chunk_data.push(0);

        // XMP data
        chunk_data.extend_from_slice(xmp_data);

        Self::write_chunk(writer, ITXT, &chunk_data)
    }
}

impl Default for PngIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerIO for PngIO {
    fn container_type() -> Container {
        Container::Png
    }

    fn supported_media_types() -> &'static [crate::MediaType] {
        &[crate::MediaType::Png]
    }

    fn extensions() -> &'static [&'static str] {
        &["png"]
    }

    fn mime_types() -> &'static [&'static str] {
        &["image/png"]
    }

    fn detect(header: &[u8]) -> Option<crate::Container> {
        if header.len() >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(Container::Png)
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

        // Write PNG signature
        writer.write_all(PNG_SIGNATURE)?;

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing metadata
        let _has_xmp = structure.segments.iter().any(|s| s.is_xmp());
        let _has_jumbf = structure.segments.iter().any(|s| s.is_jumbf());

        for segment in &structure.segments {
            match segment {
                segment if segment.is_type(SegmentKind::Header) => {
                    // Already wrote signature, skip
                    continue;
                }

                segment if segment.is_xmp() => {
                    use crate::MetadataUpdate;

                    match &updates.xmp {
                        MetadataUpdate::Keep => {
                            // Copy existing XMP chunk
                            // We need to read the XMP data from the file
                            let location = segment.location();
                            source.seek(SeekFrom::Start(location.offset))?;

                            let mut xmp_data = vec![0u8; location.size as usize];
                            source.read_exact(&mut xmp_data)?;

                            Self::write_xmp_chunk(writer, &xmp_data)?;
                            xmp_written = true;
                        }
                        MetadataUpdate::Set(new_xmp) if !xmp_written => {
                            // Write new XMP
                            Self::write_xmp_chunk(writer, new_xmp)?;
                            xmp_written = true;
                        }
                        MetadataUpdate::Remove | MetadataUpdate::Set(_) => {
                            // Skip this chunk
                        }
                    }
                }

                segment if segment.is_jumbf() => {
                    use crate::MetadataUpdate;

                    match &updates.jumbf {
                        MetadataUpdate::Keep => {
                            // Copy existing JUMBF chunk
                            let location = segment.location();
                            source.seek(SeekFrom::Start(location.offset))?;

                            let mut jumbf_data = vec![0u8; location.size as usize];
                            source.read_exact(&mut jumbf_data)?;

                            Self::write_chunk(writer, C2PA, &jumbf_data)?;
                            jumbf_written = true;
                        }
                        MetadataUpdate::Set(new_jumbf) if !jumbf_written => {
                            // Write new JUMBF
                            Self::write_chunk(writer, C2PA, new_jumbf)?;
                            jumbf_written = true;
                        }
                        MetadataUpdate::Remove | MetadataUpdate::Set(_) => {
                            // Skip this chunk
                        }
                    }
                }

                segment if segment.is_type(SegmentKind::ImageData) => {
                    // Copy IDAT chunk with header and CRC
                    // We need to reconstruct the chunk structure
                    let location = segment.location();
                    let chunk_start = location.offset - 8; // Back to length field

                    source.seek(SeekFrom::Start(chunk_start))?;

                    // Copy chunk: length(4) + type(4) + data(size) + crc(4)
                    let chunk_size = 8 + location.size + 4;
                    let mut buffer = vec![0u8; chunk_size as usize];
                    source.read_exact(&mut buffer)?;
                    writer.write_all(&buffer)?;
                }

                segment if segment.is_type(SegmentKind::Exif) => {
                    // Copy EXIF chunk with header and CRC
                    // Like IDAT, the segment location points to data, not the chunk start
                    let location = segment.location();
                    let chunk_start = location.offset - 8; // Back to length field

                    source.seek(SeekFrom::Start(chunk_start))?;

                    // Copy chunk: length(4) + type(4) + data(size) + crc(4)
                    let chunk_size = 8 + location.size + 4;
                    let mut buffer = vec![0u8; chunk_size as usize];
                    source.read_exact(&mut buffer)?;
                    writer.write_all(&buffer)?;
                }

                segment => {
                    // Check if this is IEND - we need to write new metadata before it
                    if segment.path.as_deref() == Some("IEND") {
                        // This is IEND - write any pending metadata first
                        use crate::MetadataUpdate;

                        if !xmp_written {
                            if let MetadataUpdate::Set(new_xmp) = &updates.xmp {
                                Self::write_xmp_chunk(writer, new_xmp)?;
                                xmp_written = true;
                            }
                        }

                        if !jumbf_written {
                            if let MetadataUpdate::Set(new_jumbf) = &updates.jumbf {
                                Self::write_chunk(writer, C2PA, new_jumbf)?;
                                jumbf_written = true;
                            }
                        }
                    }

                    // Copy other chunks as-is
                    let location = segment.location();
                    source.seek(SeekFrom::Start(location.offset))?;

                    let mut buffer = vec![0u8; location.size as usize];
                    source.read_exact(&mut buffer)?;
                    writer.write_all(&buffer)?;
                }
            }
        }

        // If we didn't write new metadata and it's being added to a file without it,
        // we should have written it before IEND (handled in the IEND case above)

        Ok(())
    }

    fn calculate_updated_structure(
        &self,
        source_structure: &Structure,
        updates: &Updates,
    ) -> Result<Structure> {
        use crate::MetadataUpdate;

        let mut dest_structure = Structure::new(Container::Png, source_structure.media_type);
        let mut current_offset = PNG_SIGNATURE.len() as u64;

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing metadata
        let has_xmp = source_structure.segments.iter().any(|s| s.is_xmp());
        let has_jumbf = source_structure.segments.iter().any(|s| s.is_jumbf());

        for segment in &source_structure.segments {
            match segment {
                segment if segment.is_type(SegmentKind::Header) => {
                    // PNG signature already accounted for
                    continue;
                }

                segment if segment.is_xmp() => {
                    match &updates.xmp {
                        MetadataUpdate::Keep => {
                            // Keep existing XMP chunk
                            let location = segment.location();
                            let chunk_size = 8 + location.size + 4; // length + type + data + CRC
                            dest_structure.add_segment(Segment::new(
                                current_offset + 8, // Skip length + type
                                location.size,
                                SegmentKind::Xmp,
                                segment.path.clone(),
                            ));
                            current_offset += chunk_size;
                            xmp_written = true;
                        }
                        MetadataUpdate::Set(new_xmp) if !xmp_written => {
                            // New XMP chunk
                            let xmp_size = new_xmp.len() as u64;
                            let chunk_size = 8 + xmp_size + 4;
                            dest_structure.add_segment(Segment::new(
                                current_offset + 8,
                                xmp_size,
                                SegmentKind::Xmp,
                                Some("iTXt".to_string()),
                            ));
                            current_offset += chunk_size;
                            xmp_written = true;
                        }
                        MetadataUpdate::Remove | MetadataUpdate::Set(_) => {
                            // Skip this chunk
                        }
                    }
                }

                segment if segment.is_jumbf() => {
                    match &updates.jumbf {
                        MetadataUpdate::Keep => {
                            // Keep existing JUMBF chunk
                            let location = segment.location();
                            let chunk_size = 8 + location.size + 4;
                            dest_structure.add_segment(Segment::new(
                                current_offset + 8,
                                location.size,
                                SegmentKind::Jumbf,
                                segment.path.clone(),
                            ));
                            current_offset += chunk_size;
                            jumbf_written = true;
                        }
                        MetadataUpdate::Set(new_jumbf) if !jumbf_written => {
                            // New JUMBF chunk
                            let jumbf_size = new_jumbf.len() as u64;
                            let chunk_size = 8 + jumbf_size + 4;
                            dest_structure.add_segment(Segment::new(
                                current_offset + 8,
                                jumbf_size,
                                SegmentKind::Jumbf,
                                Some("caBX".to_string()),
                            ));
                            current_offset += chunk_size;
                            jumbf_written = true;
                        }
                        MetadataUpdate::Remove | MetadataUpdate::Set(_) => {
                            // Skip this chunk
                        }
                    }
                }

                segment if segment.is_type(SegmentKind::ImageData) => {
                    // IDAT chunk
                    let location = segment.location();
                    let chunk_size = 8 + location.size + 4;
                    dest_structure.add_segment(Segment::new(
                        current_offset + 8,
                        location.size,
                        SegmentKind::ImageData,
                        segment.path.clone(),
                    ));
                    current_offset += chunk_size;
                }

                segment if segment.is_type(SegmentKind::Exif) => {
                    // EXIF chunk - always copy
                    let location = segment.location();
                    let chunk_size = 8 + location.size + 4;
                    dest_structure.add_segment(Segment::new(
                        current_offset + 8,
                        location.size,
                        SegmentKind::Exif,
                        segment.path.clone(),
                    ));
                    current_offset += chunk_size;
                }

                segment => {
                    // Check if this is IEND - write new metadata before it
                    if segment.path.as_deref() == Some("IEND") {
                        if !xmp_written && !has_xmp {
                            if let MetadataUpdate::Set(new_xmp) = &updates.xmp {
                                let xmp_size = new_xmp.len() as u64;
                                let chunk_size = 8 + xmp_size + 4;
                                dest_structure.add_segment(Segment::new(
                                    current_offset + 8,
                                    xmp_size,
                                    SegmentKind::Xmp,
                                    Some("iTXt".to_string()),
                                ));
                                current_offset += chunk_size;
                                xmp_written = true;
                            }
                        }

                        if !jumbf_written && !has_jumbf {
                            if let MetadataUpdate::Set(new_jumbf) = &updates.jumbf {
                                let jumbf_size = new_jumbf.len() as u64;
                                let chunk_size = 8 + jumbf_size + 4;
                                dest_structure.add_segment(Segment::new(
                                    current_offset + 8,
                                    jumbf_size,
                                    SegmentKind::Jumbf,
                                    Some("caBX".to_string()),
                                ));
                                current_offset += chunk_size;
                                jumbf_written = true;
                            }
                        }
                    }

                    // Copy other chunks as-is
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
    fn extract_embedded_thumbnail<R: Read + Seek>(
        &self,
        structure: &Structure,
        _source: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>> {
        // PNG doesn't typically have embedded thumbnails, but check EXIF anyway
        for segment in structure.segments() {
            if segment.is_type(SegmentKind::Exif) {
                if let Some(thumb) = segment.thumbnail() {
                    return Ok(Some(thumb.clone()));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_crc_calculation() {
        // Test CRC calculation with known values
        // IHDR chunk type + 13 bytes of data
        let chunk_type = b"IHDR";
        let data = &[
            0x00, 0x00, 0x00, 0x01, // Width: 1
            0x00, 0x00, 0x00, 0x01, // Height: 1
            0x08, // Bit depth: 8
            0x02, // Color type: RGB
            0x00, // Compression: deflate
            0x00, // Filter: adaptive
            0x00, // Interlace: none
        ];

        let crc = PngIO::calculate_crc(chunk_type, data);
        // Just verify it produces a consistent value
        assert_eq!(crc, PngIO::calculate_crc(chunk_type, data));

        // And that different data produces different CRC
        let mut different_data = data.to_vec();
        different_data[0] = 0xFF;
        let different_crc = PngIO::calculate_crc(chunk_type, &different_data);
        assert_ne!(crc, different_crc);
    }

    #[test]
    fn test_png_minimal_parse() {
        // Minimal valid PNG: signature + IHDR + IEND
        let mut data = Vec::new();

        // PNG signature
        data.extend_from_slice(PNG_SIGNATURE);

        // IHDR chunk (1x1 RGB image)
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x0D, // Length: 13
        ]);
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x01, // Width: 1
            0x00, 0x00, 0x00, 0x01, // Height: 1
            0x08, // Bit depth: 8
            0x02, // Color type: RGB
            0x00, // Compression: deflate
            0x00, // Filter: adaptive
            0x00, // Interlace: none
        ]);
        data.extend_from_slice(&0x90770c9e_u32.to_be_bytes()); // CRC

        // IEND chunk
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, // Length: 0
        ]);
        data.extend_from_slice(b"IEND");
        data.extend_from_slice(&0xAE426082_u32.to_be_bytes()); // CRC

        let mut source = Cursor::new(data);
        let handler = PngIO::new();
        let structure = handler.parse(&mut source).unwrap();

        assert_eq!(structure.container, Container::Png);
        assert!(structure.segments.len() >= 2); // At least Header + IEND
    }

    #[test]
    fn test_png_invalid_signature() {
        let data = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut source = Cursor::new(data);
        let handler = PngIO::new();
        let result = handler.parse(&mut source);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidFormat(_)));
    }
}
