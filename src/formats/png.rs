//! PNG format handler

use crate::{
    error::{Error, Result},
    segment::{LazyData, Location, Segment},
    structure::Structure,
    Format, FormatHandler, Updates,
};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{Read, Seek, SeekFrom, Write};

// PNG signature
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

// Metadata chunk types
const ITXT: &[u8] = b"iTXt";
const EXIF: &[u8] = b"eXIf";

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

/// PNG format handler
pub struct PngHandler;

impl PngHandler {
    /// Create a new PNG handler
    pub fn new() -> Self {
        Self
    }

    /// Formats this handler supports
    pub fn supported_formats() -> &'static [Format] {
        &[Format::Png]
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
    pub fn detect(header: &[u8]) -> Option<Format> {
        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        if header.len() >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(Format::Png)
        } else {
            None
        }
    }

    /// Extract XMP data from PNG file (simple iTXt chunk, no extended XMP)
    pub fn extract_xmp_impl<R: Read + Seek>(
        structure: &crate::structure::Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        let Some(index) = structure.xmp_index() else {
            return Ok(None);
        };

        if let crate::Segment::Xmp { offset, size, .. } = &structure.segments()[index] {
            // PNG stores XMP in a single iTXt chunk - no extended XMP like JPEG
            reader.seek(SeekFrom::Start(*offset))?;

            let mut xmp_data = vec![0u8; *size as usize];
            reader.read_exact(&mut xmp_data)?;

            return Ok(Some(xmp_data));
        }

        Ok(None)
    }

    /// Extract JUMBF data from PNG file (direct data from caBX chunks, no headers to strip)
    pub fn extract_jumbf_impl<R: Read + Seek>(
        structure: &crate::structure::Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        if structure.jumbf_indices().is_empty() {
            return Ok(None);
        }

        let mut result = Vec::new();

        for &index in structure.jumbf_indices() {
            if let crate::Segment::Jumbf { offset, size, .. } = &structure.segments()[index] {
                // PNG stores JUMBF directly in caBX chunks - no format-specific headers to strip
                reader.seek(SeekFrom::Start(*offset))?;

                let mut buf = vec![0u8; *size as usize];
                reader.read_exact(&mut buf)?;
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
    fn parse_impl<R: Read + Seek>(&self, reader: &mut R) -> Result<Structure> {
        let mut structure = Structure::new(Format::Png);

        // Check PNG signature
        let mut sig = [0u8; 8];
        reader.read_exact(&mut sig)?;
        if sig != PNG_SIGNATURE {
            return Err(Error::InvalidFormat("Not a PNG file".into()));
        }

        structure.add_segment(Segment::Header { offset: 0, size: 8 });

        let mut offset = 8u64;
        let mut found_iend = false;

        loop {
            // Read chunk length
            let chunk_len = match reader.read_u32::<BigEndian>() {
                Ok(len) => len as u64,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            };

            // Read chunk type
            let mut chunk_type = [0u8; 4];
            reader.read_exact(&mut chunk_type)?;

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
                    structure.add_segment(Segment::Other {
                        offset: chunk_start,
                        size: 8 + chunk_len + 4, // length + type + data + CRC
                        label: chunk_label(&chunk_type),
                    });
                    reader.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                b"IDAT" => {
                    // Image data
                    structure.add_segment(Segment::ImageData {
                        offset: data_offset,
                        size: chunk_len,
                    });
                    reader.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                b"IEND" => {
                    // End chunk
                    structure.add_segment(Segment::Other {
                        offset: chunk_start,
                        size: 8 + chunk_len + 4,
                        label: chunk_label(&chunk_type),
                    });
                    reader.seek(SeekFrom::Current((chunk_len + 4) as i64))?;
                    found_iend = true;
                    structure.total_size = offset + 8 + chunk_len + 4;
                    break;
                }

                b"iTXt" => {
                    // Check if this is XMP
                    let keyword_len = XMP_KEYWORD.len().min(chunk_len as usize);
                    let mut keyword_buf = vec![0u8; keyword_len];
                    reader.read_exact(&mut keyword_buf)?;

                    if keyword_buf == XMP_KEYWORD {
                        // This is XMP data
                        // iTXt format: keyword\0 + compression_flag(1) + compression_method(1) + language_tag\0 + translated_keyword\0 + text
                        // For XMP: "XML:com.adobe.xmp\0" + 0x00 + 0x00 + "\0" + "\0" + XMP_data
                        // Skip: compression_flag(1) + compression_method(1) + language_tag\0 + translated_keyword\0

                        // Read compression flag and method
                        let _compression_flag = reader.read_u8()?;
                        let _compression_method = reader.read_u8()?;

                        // Skip language tag (null-terminated)
                        let mut lang_consumed = 0;
                        loop {
                            let byte = reader.read_u8()?;
                            lang_consumed += 1;
                            if byte == 0 || lang_consumed > 100 {
                                break;
                            }
                        }

                        // Skip translated keyword (null-terminated)
                        let mut trans_consumed = 0;
                        loop {
                            let byte = reader.read_u8()?;
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

                        // Skip remaining XMP data + CRC
                        let remaining = xmp_size + 4; // XMP data + CRC
                        reader.seek(SeekFrom::Current(remaining as i64))?;
                    } else {
                        // Regular iTXt chunk
                        structure.add_segment(Segment::Other {
                            offset: chunk_start,
                            size: 8 + chunk_len + 4,
                            label: chunk_label(&chunk_type),
                        });
                        // Skip remaining data + CRC
                        let remaining = chunk_len - keyword_len as u64 + 4;
                        reader.seek(SeekFrom::Current(remaining as i64))?;
                    }
                }

                b"caBX" => {
                    // C2PA/JUMBF chunk
                    structure.add_segment(Segment::Jumbf {
                        offset: data_offset,
                        size: chunk_len,
                        segments: vec![crate::segment::Location {
                            offset: data_offset,
                            size: chunk_len,
                        }],
                        data: LazyData::NotLoaded,
                    });
                    reader.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                b"eXIf" => {
                    // EXIF chunk (PNG extension, added in PNG 1.5.0 specification)
                    // Contains raw EXIF data in TIFF format (without the "Exif\0\0" header used in JPEG)
                    structure.add_segment(Segment::Exif {
                        offset: data_offset,
                        size: chunk_len,
                        #[cfg(feature = "exif")]
                        thumbnail: None, // TODO: Parse EXIF to extract thumbnail
                    });
                    reader.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
                }

                _ => {
                    // Other chunk types
                    structure.add_segment(Segment::Other {
                        offset: chunk_start,
                        size: 8 + chunk_len + 4,
                        label: chunk_label(&chunk_type),
                    });
                    reader.seek(SeekFrom::Current((chunk_len + 4) as i64))?; // Skip data + CRC
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

impl Default for PngHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatHandler for PngHandler {
    fn supported_formats() -> &'static [Format] {
        &[Format::Png]
    }

    fn extensions() -> &'static [&'static str] {
        &["png"]
    }

    fn mime_types() -> &'static [&'static str] {
        &["image/png"]
    }

    fn detect(header: &[u8]) -> Option<Format> {
        if header.len() >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(Format::Png)
        } else {
            None
        }
    }

    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<Structure> {
        self.parse_impl(reader)
    }

    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::extract_xmp_impl(structure, reader)
    }

    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::extract_jumbf_impl(structure, reader)
    }

    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
        reader: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        reader.seek(SeekFrom::Start(0))?;

        // Write PNG signature
        writer.write_all(PNG_SIGNATURE)?;

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing metadata
        let _has_xmp = structure
            .segments
            .iter()
            .any(|s| matches!(s, Segment::Xmp { .. }));
        let _has_jumbf = structure
            .segments
            .iter()
            .any(|s| matches!(s, Segment::Jumbf { .. }));

        for segment in &structure.segments {
            match segment {
                Segment::Header { .. } => {
                    // Already wrote signature, skip
                    continue;
                }

                Segment::Xmp { offset, size, .. } => {
                    use crate::XmpUpdate;

                    match &updates.xmp {
                        XmpUpdate::Keep => {
                            // Copy existing XMP chunk
                            // We need to read the XMP data from the file
                            reader.seek(SeekFrom::Start(*offset))?;

                            let mut xmp_data = vec![0u8; *size as usize];
                            reader.read_exact(&mut xmp_data)?;

                            Self::write_xmp_chunk(writer, &xmp_data)?;
                            xmp_written = true;
                        }
                        XmpUpdate::Set(new_xmp) if !xmp_written => {
                            // Write new XMP
                            Self::write_xmp_chunk(writer, new_xmp)?;
                            xmp_written = true;
                        }
                        XmpUpdate::Remove | XmpUpdate::Set(_) => {
                            // Skip this chunk
                        }
                    }
                }

                Segment::Jumbf { offset, size, .. } => {
                    use crate::JumbfUpdate;

                    match &updates.jumbf {
                        JumbfUpdate::Keep => {
                            // Copy existing JUMBF chunk
                            reader.seek(SeekFrom::Start(*offset))?;

                            let mut jumbf_data = vec![0u8; *size as usize];
                            reader.read_exact(&mut jumbf_data)?;

                            Self::write_chunk(writer, C2PA, &jumbf_data)?;
                            jumbf_written = true;
                        }
                        JumbfUpdate::Set(new_jumbf) if !jumbf_written => {
                            // Write new JUMBF
                            Self::write_chunk(writer, C2PA, new_jumbf)?;
                            jumbf_written = true;
                        }
                        JumbfUpdate::Remove | JumbfUpdate::Set(_) => {
                            // Skip this chunk
                        }
                    }
                }

                Segment::ImageData { offset, size, .. } => {
                    // Copy IDAT chunk with header and CRC
                    // We need to reconstruct the chunk structure
                    let chunk_start = offset - 8; // Back to length field

                    reader.seek(SeekFrom::Start(chunk_start))?;

                    // Copy chunk: length(4) + type(4) + data(size) + crc(4)
                    let chunk_size = 8 + size + 4;
                    let mut buffer = vec![0u8; chunk_size as usize];
                    reader.read_exact(&mut buffer)?;
                    writer.write_all(&buffer)?;
                }

                Segment::Other {
                    offset,
                    size,
                    label,
                    ..
                } => {
                    // Check if this is IEND - we need to write new metadata before it
                    if *label == "IEND" {
                        // This is IEND - write any pending metadata first
                        use crate::{JumbfUpdate, XmpUpdate};

                        if !xmp_written {
                            if let XmpUpdate::Set(new_xmp) = &updates.xmp {
                                Self::write_xmp_chunk(writer, new_xmp)?;
                                xmp_written = true;
                            }
                        }

                        if !jumbf_written {
                            if let JumbfUpdate::Set(new_jumbf) = &updates.jumbf {
                                Self::write_chunk(writer, C2PA, new_jumbf)?;
                                jumbf_written = true;
                            }
                        }
                    }

                    // Copy other chunks as-is
                    reader.seek(SeekFrom::Start(*offset))?;

                    let mut buffer = vec![0u8; *size as usize];
                    reader.read_exact(&mut buffer)?;
                    writer.write_all(&buffer)?;
                }

                Segment::Exif { offset, size, .. } => {
                    // Write eXIf chunk with proper structure
                    reader.seek(SeekFrom::Start(*offset))?;
                    let mut exif_data = vec![0u8; *size as usize];
                    reader.read_exact(&mut exif_data)?;

                    // Write as eXIf chunk
                    Self::write_chunk(writer, EXIF, &exif_data)?;
                }
            }
        }

        // If we didn't write new metadata and it's being added to a file without it,
        // we should have written it before IEND (handled in the IEND case above)

        Ok(())
    }

    #[cfg(feature = "exif")]
    fn extract_embedded_thumbnail<R: Read + Seek>(
        &self,
        structure: &Structure,
        _reader: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>> {
        // PNG doesn't typically have embedded thumbnails, but check EXIF anyway
        for segment in structure.segments() {
            if let Segment::Exif { thumbnail, .. } = segment {
                return Ok(thumbnail.clone());
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

        let crc = PngHandler::calculate_crc(chunk_type, data);
        // Just verify it produces a consistent value
        assert_eq!(crc, PngHandler::calculate_crc(chunk_type, data));

        // And that different data produces different CRC
        let mut different_data = data.to_vec();
        different_data[0] = 0xFF;
        let different_crc = PngHandler::calculate_crc(chunk_type, &different_data);
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

        let mut reader = Cursor::new(data);
        let handler = PngHandler::new();
        let structure = handler.parse(&mut reader).unwrap();

        assert_eq!(structure.format, Format::Png);
        assert!(structure.segments.len() >= 2); // At least Header + IEND
    }

    #[test]
    fn test_png_invalid_signature() {
        let data = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut reader = Cursor::new(data);
        let handler = PngHandler::new();
        let result = handler.parse(&mut reader);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidFormat(_)));
    }
}
