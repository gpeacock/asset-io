//! PNG container I/O implementation

use super::{ContainerIO, ContainerKind};
use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind},
    structure::Structure,
    Updates,
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

// Size of the iTXt header for XMP chunks (before XMP data)
// = XMP_KEYWORD (18) + compression_flag (1) + compression_method (1) + language_tag_null (1) + translated_keyword_null (1)
const ITXT_XMP_HEADER_SIZE: u64 = 22;

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
    pub fn container_type() -> ContainerKind {
        ContainerKind::Png
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
    pub fn detect(header: &[u8]) -> Option<crate::ContainerKind> {
        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        if header.len() >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(ContainerKind::Png)
        } else {
            None
        }
    }

    /// Extract XMP data from PNG file (simple iTXt chunk, no extended XMP)
    pub fn read_xmp_impl<R: Read + Seek>(
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
    pub fn read_jumbf_impl<R: Read + Seek>(
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
                
                // Validate size to prevent memory exhaustion attacks
                if location.size > crate::segment::MAX_SEGMENT_SIZE {
                    return Err(Error::InvalidSegment {
                        offset: location.offset,
                        reason: format!(
                            "JUMBF segment too large: {} bytes (max {} MB)",
                            location.size,
                            crate::segment::MAX_SEGMENT_SIZE / (1024 * 1024)
                        ),
                    });
                }
                
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
        let mut structure = Structure::new(ContainerKind::Png, crate::MediaType::Png);

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

    /// Efficiently copy bytes from source to writer using chunked I/O
    /// This avoids large allocations for big segments
    fn copy_bytes<R: Read, W: Write>(source: &mut R, writer: &mut W, size: u64) -> Result<()> {
        const CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8MB chunks

        if size > CHUNK_SIZE as u64 {
            let mut buffer = vec![0u8; CHUNK_SIZE];
            let mut remaining = size;

            while remaining > 0 {
                let to_copy = remaining.min(CHUNK_SIZE as u64) as usize;
                source.read_exact(&mut buffer[..to_copy])?;
                writer.write_all(&buffer[..to_copy])?;
                remaining -= to_copy as u64;
            }
        } else {
            // Small data - single allocation is fine
            let mut buffer = vec![0u8; size as usize];
            source.read_exact(&mut buffer)?;
            writer.write_all(&buffer)?;
        }

        Ok(())
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

    /// Write a PNG chunk with proper C2PA exclusion handling for ProcessingWriter
    ///
    /// Per C2PA spec, only the manifest DATA (and CRC which depends on data)
    /// should be excluded from hashing. The length and type fields must be
    /// included in the hash to prevent insertion attacks.
    fn write_chunk_with_exclusion<W: Write, F: FnMut(&[u8])>(
        pw: &mut crate::processing_writer::ProcessingWriter<W, F>,
        chunk_type: &[u8],
        data: &[u8],
        should_exclude: bool,
    ) -> Result<()> {
        // Write length (included in hash)
        pw.write_u32::<BigEndian>(data.len() as u32)?;

        // Write type (included in hash)
        pw.write_all(chunk_type)?;

        // Enable exclusion for data + CRC
        if should_exclude {
            pw.set_exclude_mode(true);
        }

        // Write data (excluded from hash)
        pw.write_all(data)?;

        // Calculate and write CRC (excluded from hash - it depends on data)
        let crc = Self::calculate_crc(chunk_type, data);
        pw.write_u32::<BigEndian>(crc)?;

        // Disable exclusion
        if should_exclude {
            pw.set_exclude_mode(false);
        }

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
    fn container_type() -> ContainerKind {
        ContainerKind::Png
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

    fn detect(header: &[u8]) -> Option<crate::ContainerKind> {
        if header.len() >= 8 && &header[0..8] == b"\x89PNG\r\n\x1a\n" {
            Some(ContainerKind::Png)
        } else {
            None
        }
    }

    fn parse<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        self.parse_impl(source)
    }

    fn read_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::read_xmp_impl(structure, source)
    }

    fn read_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::read_jumbf_impl(structure, source)
    }

    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        // Calculate the destination structure first - this tells us exactly what to write
        // This is the SAME structure that will be returned for updates
        let dest_structure = self.calculate_updated_structure(structure, updates)?;

        source.seek(SeekFrom::Start(0))?;

        // Write PNG signature
        writer.write_all(PNG_SIGNATURE)?;

        // Collect source segments by type for ordered iteration
        let source_idats: Vec<_> = structure
            .segments
            .iter()
            .filter(|s| s.is_type(SegmentKind::ImageData))
            .collect();
        let mut idat_index = 0;

        // For "Other" segments, we need to track which ones we've used
        // since multiple chunks can have the same path (e.g., multiple tEXt)
        let source_others: Vec<_> = structure
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Other)
            .collect();
        let mut other_index = 0;

        // Iterate through destination structure and write each segment
        for dest_segment in &dest_structure.segments {
            match dest_segment {
                seg if seg.is_type(SegmentKind::Header) => {
                    // Already wrote signature
                    continue;
                }

                seg if seg.is_xmp() => {
                    // Write XMP based on updates
                    match &updates.xmp {
                        crate::MetadataUpdate::Set(new_xmp) => {
                            Self::write_xmp_chunk(writer, new_xmp)?;
                        }
                        crate::MetadataUpdate::Keep => {
                            // Find corresponding source segment and copy XMP data
                            if let Some(source_seg) = structure.segments.iter().find(|s| s.is_xmp())
                            {
                                let location = source_seg.location();
                                source.seek(SeekFrom::Start(location.offset))?;

                                let mut xmp_data = vec![0u8; location.size as usize];
                                source.read_exact(&mut xmp_data)?;

                                Self::write_xmp_chunk(writer, &xmp_data)?;
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
                            Self::write_chunk(writer, C2PA, new_jumbf)?;
                        }
                        crate::MetadataUpdate::Keep => {
                            // Find corresponding source segment and copy it
                            if let Some(source_seg) =
                                structure.segments.iter().find(|s| s.is_jumbf())
                            {
                                let location = source_seg.location();
                                source.seek(SeekFrom::Start(location.offset))?;

                                let mut jumbf_data = vec![0u8; location.size as usize];
                                source.read_exact(&mut jumbf_data)?;

                                Self::write_chunk(writer, C2PA, &jumbf_data)?;
                            }
                        }
                        crate::MetadataUpdate::Remove => {
                            // Skip - segment not in destination
                        }
                    }
                }

                _seg if _seg.is_type(SegmentKind::ImageData) => {
                    // Use ordered iteration through source IDAT segments
                    if idat_index < source_idats.len() {
                        let source_seg = source_idats[idat_index];
                        let location = source_seg.location();
                        let chunk_start = location.offset - 8; // Back to length field
                        let chunk_size = 8 + location.size + 4;

                        source.seek(SeekFrom::Start(chunk_start))?;
                        Self::copy_bytes(source, writer, chunk_size)?;
                        idat_index += 1;
                    }
                }

                _seg if _seg.is_type(SegmentKind::Exif) => {
                    // Find corresponding source segment (typically only one EXIF)
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.is_type(SegmentKind::Exif))
                    {
                        let location = source_seg.location();
                        let chunk_start = location.offset - 8; // Back to length field
                        let chunk_size = 8 + location.size + 4;

                        source.seek(SeekFrom::Start(chunk_start))?;
                        Self::copy_bytes(source, writer, chunk_size)?;
                    }
                }

                _ => {
                    // Copy other chunks from source in order
                    if other_index < source_others.len() {
                        let source_seg = source_others[other_index];
                        let location = source_seg.location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        Self::copy_bytes(source, writer, location.size)?;
                        other_index += 1;
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

        // Write PNG signature
        pw.write_all(PNG_SIGNATURE)?;

        // Collect source segments by type for ordered iteration
        let source_idats: Vec<_> = structure
            .segments
            .iter()
            .filter(|s| s.is_type(SegmentKind::ImageData))
            .collect();
        let mut idat_index = 0;

        let source_others: Vec<_> = structure
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Other)
            .collect();
        let mut other_index = 0;

        // Iterate through destination structure and write each segment
        for dest_segment in &dest_structure.segments {
            match dest_segment {
                seg if seg.is_type(SegmentKind::Header) => {
                    continue;
                }

                seg if seg.is_xmp() => {
                    match &updates.xmp {
                        crate::MetadataUpdate::Set(new_xmp) => {
                            Self::write_xmp_chunk(&mut pw, new_xmp)?;
                        }
                        crate::MetadataUpdate::Keep => {
                            if let Some(source_seg) = structure.segments.iter().find(|s| s.is_xmp())
                            {
                                let location = source_seg.location();
                                source.seek(SeekFrom::Start(location.offset))?;

                                let mut xmp_data = vec![0u8; location.size as usize];
                                source.read_exact(&mut xmp_data)?;

                                Self::write_xmp_chunk(&mut pw, &xmp_data)?;
                            }
                        }
                        crate::MetadataUpdate::Remove => {}
                    }
                }

                seg if seg.is_jumbf() => {
                    // Handle JUMBF based on exclusion mode:
                    // - DataOnly: Include length+type in hash, exclude data+CRC (C2PA compliant)
                    // - EntireSegment: Exclude entire chunk including headers
                    match &updates.jumbf {
                        crate::MetadataUpdate::Set(new_jumbf) => {
                            if data_only_mode {
                                Self::write_chunk_with_exclusion(
                                    &mut pw,
                                    C2PA,
                                    new_jumbf,
                                    should_exclude_jumbf,
                                )?;
                            } else {
                                // EntireSegment mode: exclude everything
                                if should_exclude_jumbf {
                                    pw.set_exclude_mode(true);
                                }
                                Self::write_chunk(&mut pw, C2PA, new_jumbf)?;
                                if should_exclude_jumbf {
                                    pw.set_exclude_mode(false);
                                }
                            }
                        }
                        crate::MetadataUpdate::Keep => {
                            if let Some(source_seg) =
                                structure.segments.iter().find(|s| s.is_jumbf())
                            {
                                let location = source_seg.location();
                                source.seek(SeekFrom::Start(location.offset))?;

                                let mut jumbf_data = vec![0u8; location.size as usize];
                                source.read_exact(&mut jumbf_data)?;

                                if data_only_mode {
                                    Self::write_chunk_with_exclusion(
                                        &mut pw,
                                        C2PA,
                                        &jumbf_data,
                                        should_exclude_jumbf,
                                    )?;
                                } else {
                                    // EntireSegment mode: exclude everything
                                    if should_exclude_jumbf {
                                        pw.set_exclude_mode(true);
                                    }
                                    Self::write_chunk(&mut pw, C2PA, &jumbf_data)?;
                                    if should_exclude_jumbf {
                                        pw.set_exclude_mode(false);
                                    }
                                }
                            }
                        }
                        crate::MetadataUpdate::Remove => {}
                    }
                }

                _seg if _seg.is_type(SegmentKind::ImageData) => {
                    if idat_index < source_idats.len() {
                        let source_seg = source_idats[idat_index];
                        let location = source_seg.location();
                        let chunk_start = location.offset - 8;
                        let chunk_size = 8 + location.size + 4;

                        source.seek(SeekFrom::Start(chunk_start))?;
                        Self::copy_bytes(source, &mut pw, chunk_size)?;
                        idat_index += 1;
                    }
                }

                _seg if _seg.is_type(SegmentKind::Exif) => {
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.is_type(SegmentKind::Exif))
                    {
                        let location = source_seg.location();
                        let chunk_start = location.offset - 8;
                        let chunk_size = 8 + location.size + 4;

                        source.seek(SeekFrom::Start(chunk_start))?;
                        Self::copy_bytes(source, &mut pw, chunk_size)?;
                    }
                }

                _ => {
                    if other_index < source_others.len() {
                        let source_seg = source_others[other_index];
                        let location = source_seg.location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        Self::copy_bytes(source, &mut pw, location.size)?;
                        other_index += 1;
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

        let mut dest_structure = Structure::new(ContainerKind::Png, source_structure.media_type);
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
                            // XMP data is wrapped in iTXt: keyword + flags + XMP data
                            let location = segment.location();
                            let chunk_data_size = ITXT_XMP_HEADER_SIZE + location.size;
                            let chunk_size = 8 + chunk_data_size + 4; // length + type + data + CRC
                            dest_structure.add_segment(Segment::new(
                                current_offset + 8 + ITXT_XMP_HEADER_SIZE, // After length + type + iTXt header
                                location.size,
                                SegmentKind::Xmp,
                                segment.path.clone(),
                            ));
                            current_offset += chunk_size;
                            xmp_written = true;
                        }
                        MetadataUpdate::Set(new_xmp) if !xmp_written => {
                            // New XMP chunk - iTXt header adds 22 bytes
                            let xmp_size = new_xmp.len() as u64;
                            let chunk_data_size = ITXT_XMP_HEADER_SIZE + xmp_size;
                            let chunk_size = 8 + chunk_data_size + 4;
                            dest_structure.add_segment(Segment::new(
                                current_offset + 8 + ITXT_XMP_HEADER_SIZE,
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
                                // New XMP chunk - iTXt header adds 22 bytes
                                let xmp_size = new_xmp.len() as u64;
                                let chunk_data_size = ITXT_XMP_HEADER_SIZE + xmp_size;
                                let chunk_size = 8 + chunk_data_size + 4;
                                dest_structure.add_segment(Segment::new(
                                    current_offset + 8 + ITXT_XMP_HEADER_SIZE,
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
                    // Note: "Other" chunks store the FULL chunk (offset at length field,
                    // size includes length + type + data + CRC), unlike data segments
                    // which only store the data portion
                    let location = segment.location();
                    let chunk_size = location.size; // Already includes full chunk
                    dest_structure.add_segment(Segment::new(
                        current_offset, // Full chunk starts here
                        location.size,
                        segment.kind,
                        segment.path.clone(),
                    ));
                    current_offset += chunk_size;
                }
            }
        }

        dest_structure.total_size = current_offset;
        Ok(dest_structure)
    }

    #[cfg(feature = "exif")]
    fn read_embedded_thumbnail_info<R: Read + Seek>(
        &self,
        structure: &Structure,
        _source: &mut R,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
        // PNG doesn't typically have embedded thumbnails, but check EXIF anyway
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
    fn read_exif_info<R: Read + Seek>(
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

        // PNG eXIf chunk: just raw TIFF data (no Exif\0\0 prefix)
        crate::tiff::parse_exif_info(&data)
    }
}

/// Update a PNG segment in-place with proper CRC recalculation
///
/// PNG chunks have a CRC that must be recalculated when data changes.
/// This function handles the PNG-specific update:
/// - For JUMBF (caBX): Updates data and recalculates CRC
/// - For XMP (iTXt): Updates XMP data portion and recalculates CRC
///
/// # Arguments
/// - `writer`: A seekable writer positioned at the file
/// - `structure`: The destination structure with segment positions
/// - `kind`: SegmentKind::Jumbf or SegmentKind::Xmp
/// - `data`: The new data (will be padded to fit existing capacity)
///
/// # Returns
/// Number of bytes written (the padded data size)
pub fn update_png_segment_in_stream<W: Write + Seek>(
    writer: &mut W,
    structure: &Structure,
    kind: SegmentKind,
    data: Vec<u8>,
) -> Result<usize> {
    // Find the segment
    let segment_idx = match kind {
        SegmentKind::Jumbf => structure.c2pa_jumbf_index(),
        SegmentKind::Xmp => structure.xmp_index(),
        _ => {
            return Err(Error::InvalidFormat(format!(
                "PNG in-place update not supported for {:?}",
                kind
            )))
        }
    }
    .ok_or_else(|| Error::InvalidFormat(format!("No {:?} segment found in PNG", kind)))?;

    let segment = &structure.segments[segment_idx];
    let data_offset = segment.location().offset;
    let data_capacity = segment.location().size;

    // Validate size
    if data.len() as u64 > data_capacity {
        return Err(Error::InvalidFormat(format!(
            "Data ({} bytes) exceeds PNG chunk capacity ({} bytes)",
            data.len(),
            data_capacity
        )));
    }

    // Pad data to exact capacity
    let mut padded_data = data;
    padded_data.resize(data_capacity as usize, 0);

    match kind {
        SegmentKind::Jumbf => {
            // caBX chunk layout:
            // [length:4][type:4][data:N][crc:4]
            // data_offset points to start of data (after type)
            
            // Write the data
            writer.seek(SeekFrom::Start(data_offset))?;
            writer.write_all(&padded_data)?;

            // Calculate CRC over chunk_type + data
            let crc = PngIO::calculate_crc(C2PA, &padded_data);

            // CRC is immediately after data
            // (we're already positioned there after writing data)
            writer.write_u32::<BigEndian>(crc)?;
        }

        SegmentKind::Xmp => {
            // iTXt XMP chunk layout:
            // [length:4][type:4][header:22][xmp_data:N][crc:4]
            // data_offset points to start of XMP data (after header)
            // header = keyword(18) + flags(4)

            // Write the XMP data
            writer.seek(SeekFrom::Start(data_offset))?;
            writer.write_all(&padded_data)?;

            // Build complete chunk data for CRC calculation
            // iTXt chunk data = keyword + flags + XMP data
            let mut chunk_data = Vec::with_capacity(ITXT_XMP_HEADER_SIZE as usize + padded_data.len());
            chunk_data.extend_from_slice(XMP_KEYWORD); // 18 bytes
            chunk_data.push(0); // compression flag
            chunk_data.push(0); // compression method
            chunk_data.push(0); // language tag null
            chunk_data.push(0); // translated keyword null
            chunk_data.extend_from_slice(&padded_data);

            // Calculate CRC over "iTXt" + chunk_data
            let crc = PngIO::calculate_crc(ITXT, &chunk_data);

            // CRC is immediately after XMP data
            writer.write_u32::<BigEndian>(crc)?;
        }

        _ => unreachable!(),
    }

    writer.flush()?;
    Ok(padded_data.len())
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

        assert_eq!(structure.container, ContainerKind::Png);
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
