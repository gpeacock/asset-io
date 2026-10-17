//! PNG container I/O implementation

use super::{ContainerIO, ContainerKind};
use crate::sans_io::io::{PDst, Src};
use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind, DEFAULT_CHUNK_SIZE},
    structure::Structure,
    Updates,
};
use std::io::SeekFrom;

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
    #[allow(dead_code)]
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
    pub(crate) async fn read_xmp_async(
        &self,
        structure: &Structure,
        source: &mut Src,
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
            source.read_exact(&mut xmp_data).await?;

            return Ok(Some(xmp_data));
        }

        Ok(None)
    }

    /// Extract JUMBF data from PNG file (direct data from caBX chunks, no headers to strip)
    pub(crate) async fn read_jumbf_async(
        &self,
        structure: &Structure,
        source: &mut Src,
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
                source.read_exact(&mut buf).await?;
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
    pub(crate) async fn parse_async(&self, source: &mut Src) -> Result<Structure> {
        let mut structure = Structure::new(ContainerKind::Png, crate::MediaType::Png);

        // Check PNG signature
        let mut sig = [0u8; 8];
        source.read_exact(&mut sig).await?;
        if sig != PNG_SIGNATURE {
            return Err(Error::InvalidFormat("Not a PNG file".into()));
        }

        structure.add_segment(Segment::new(
            0,
            8,
            SegmentKind::Header,
            Some("PNGh".to_string()),
        ));

        let mut offset = 8u64;
        let mut found_iend = false;

        loop {
            // Read chunk length
            let chunk_len = match source.read_u32_be().await {
                Ok(len) => len as u64,
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };

            // Read chunk type
            let mut chunk_type = [0u8; 4];
            source.read_exact(&mut chunk_type).await?;

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
                        Some("IDAT".to_string()),
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
                    source.read_exact(&mut keyword_buf).await?;

                    if keyword_buf == XMP_KEYWORD {
                        // This is XMP data
                        // iTXt container: keyword\0 + compression_flag(1) + compression_method(1) + language_tag\0 + translated_keyword\0 + text
                        // For XMP: "XML:com.adobe.xmp\0" + 0x00 + 0x00 + "\0" + "\0" + XMP_data
                        // Skip: compression_flag(1) + compression_method(1) + language_tag\0 + translated_keyword\0

                        // Read compression flag and method
                        let _compression_flag = source.read_u8().await?;
                        let _compression_method = source.read_u8().await?;

                        // Skip language tag (null-terminated)
                        let mut lang_consumed = 0;
                        loop {
                            let byte = source.read_u8().await?;
                            lang_consumed += 1;
                            if byte == 0 || lang_consumed > 100 {
                                break;
                            }
                        }

                        // Skip translated keyword (null-terminated)
                        let mut trans_consumed = 0;
                        loop {
                            let byte = source.read_u8().await?;
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
                        )?);

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
                    )?);
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

    /// Efficiently copy bytes from source to writer using chunked I/O.
    async fn copy_bytes_async(source: &mut Src, writer: &mut PDst<'_>, size: u64) -> Result<()> {
        let mut buffer = vec![0u8; size.min(DEFAULT_CHUNK_SIZE as u64) as usize];
        let mut remaining = size;
        while remaining > 0 {
            let to_copy = remaining.min(buffer.len() as u64) as usize;
            source.read_exact_direct(&mut buffer[..to_copy]).await?;
            writer.write_all(&buffer[..to_copy]).await?;
            remaining -= to_copy as u64;
        }
        Ok(())
    }

    /// Write a PNG chunk with proper CRC.
    async fn write_chunk_async(
        writer: &mut PDst<'_>,
        chunk_type: &[u8],
        data: &[u8],
    ) -> Result<()> {
        writer.write_u32_be(data.len() as u32).await?;
        writer.write_all(chunk_type).await?;
        writer.write_all(data).await?;
        let crc = Self::calculate_crc(chunk_type, data);
        writer.write_u32_be(crc).await?;
        Ok(())
    }

    /// Write a PNG chunk, optionally excluding the payload from processing.
    async fn write_chunk_with_exclusion_async(
        pw: &mut PDst<'_>,
        chunk_type: &[u8],
        data: &[u8],
        should_exclude: bool,
    ) -> Result<()> {
        pw.write_u32_be(data.len() as u32).await?;
        pw.write_all(chunk_type).await?;
        if should_exclude {
            pw.set_exclude_mode(true);
        }
        pw.write_all(data).await?;
        let crc = Self::calculate_crc(chunk_type, data);
        pw.write_u32_be(crc).await?;
        if should_exclude {
            pw.set_exclude_mode(false);
        }
        Ok(())
    }

    /// Write XMP as an iTXt chunk.
    async fn write_xmp_chunk_async(writer: &mut PDst<'_>, xmp_data: &[u8]) -> Result<()> {
        let mut chunk_data = Vec::with_capacity(XMP_KEYWORD.len() + 4 + xmp_data.len());
        chunk_data.extend_from_slice(XMP_KEYWORD);
        chunk_data.push(0);
        chunk_data.push(0);
        chunk_data.push(0);
        chunk_data.push(0);
        chunk_data.extend_from_slice(xmp_data);
        Self::write_chunk_async(writer, ITXT, &chunk_data).await
    }

    pub(crate) async fn write_async(
        &self,
        structure: &Structure,
        source: &mut Src,
        pw: &mut PDst<'_>,
        updates: &Updates,
    ) -> Result<()> {
        let dest_structure = self.calculate_updated_structure(structure, updates)?;
        let should_exclude_jumbf = updates
            .processing
            .exclude_segments
            .contains(&SegmentKind::Jumbf);
        let data_only_mode = updates.processing.exclusion_mode == crate::ExclusionMode::DataOnly;
        source.seek(SeekFrom::Start(0))?;

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

        for dest_segment in &dest_structure.segments {
            pw.begin_segment(dest_segment).await?;
            match dest_segment {
                seg if seg.is_type(SegmentKind::Header) => {
                    pw.write_all(PNG_SIGNATURE).await?;
                }
                seg if seg.is_xmp() => match &updates.xmp {
                    crate::MetadataUpdate::Set(new_xmp) => {
                        Self::write_xmp_chunk_async(pw, new_xmp).await?;
                    }
                    crate::MetadataUpdate::Keep => {
                        if let Some(source_seg) = structure.segments.iter().find(|s| s.is_xmp()) {
                            let location = source_seg.location();
                            source.seek(SeekFrom::Start(location.offset))?;
                            let mut xmp_data = vec![0u8; location.size as usize];
                            source.read_exact(&mut xmp_data).await?;
                            Self::write_xmp_chunk_async(pw, &xmp_data).await?;
                        }
                    }
                    crate::MetadataUpdate::Remove => {}
                },
                seg if seg.is_jumbf() => match &updates.jumbf {
                    crate::MetadataUpdate::Set(new_jumbf) => {
                        if data_only_mode {
                            Self::write_chunk_with_exclusion_async(
                                pw,
                                C2PA,
                                new_jumbf,
                                should_exclude_jumbf,
                            )
                            .await?;
                        } else {
                            if should_exclude_jumbf {
                                pw.set_exclude_mode(true);
                            }
                            Self::write_chunk_async(pw, C2PA, new_jumbf).await?;
                            if should_exclude_jumbf {
                                pw.set_exclude_mode(false);
                            }
                        }
                    }
                    crate::MetadataUpdate::Keep => {
                        if let Some(source_seg) = structure.segments.iter().find(|s| s.is_jumbf()) {
                            let location = source_seg.location();
                            source.seek(SeekFrom::Start(location.offset))?;
                            let mut jumbf_data = vec![0u8; location.size as usize];
                            source.read_exact(&mut jumbf_data).await?;
                            if data_only_mode {
                                Self::write_chunk_with_exclusion_async(
                                    pw,
                                    C2PA,
                                    &jumbf_data,
                                    should_exclude_jumbf,
                                )
                                .await?;
                            } else {
                                if should_exclude_jumbf {
                                    pw.set_exclude_mode(true);
                                }
                                Self::write_chunk_async(pw, C2PA, &jumbf_data).await?;
                                if should_exclude_jumbf {
                                    pw.set_exclude_mode(false);
                                }
                            }
                        }
                    }
                    crate::MetadataUpdate::Remove => {}
                },
                seg if seg.is_type(SegmentKind::ImageData) => {
                    if idat_index < source_idats.len() {
                        let location = source_idats[idat_index].location();
                        let chunk_start = location.offset - 8;
                        let chunk_size = 8 + location.size + 4;
                        source.seek(SeekFrom::Start(chunk_start))?;
                        Self::copy_bytes_async(source, pw, chunk_size).await?;
                        idat_index += 1;
                    }
                }
                seg if seg.is_type(SegmentKind::Exif) => {
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.is_type(SegmentKind::Exif))
                    {
                        let location = source_seg.location();
                        let chunk_start = location.offset - 8;
                        let chunk_size = 8 + location.size + 4;
                        source.seek(SeekFrom::Start(chunk_start))?;
                        Self::copy_bytes_async(source, pw, chunk_size).await?;
                    }
                }
                _ => {
                    if other_index < source_others.len() {
                        let location = source_others[other_index].location();
                        source.seek(SeekFrom::Start(location.offset))?;
                        Self::copy_bytes_async(source, pw, location.size).await?;
                        other_index += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "exif")]
impl PngIO {
    pub(crate) async fn read_embedded_thumbnail_info_async(
        &self,
        structure: &Structure,
        _source: &mut Src,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
        for segment in structure.segments() {
            if segment.is_type(SegmentKind::Exif) {
                if let Some(info) = segment.thumbnail_info() {
                    return Ok(Some(info.clone()));
                }
            }
        }
        Ok(None)
    }

    pub(crate) async fn read_exif_info_async(
        &self,
        structure: &Structure,
        source: &mut Src,
    ) -> Result<Option<crate::tiff::ExifInfo>> {
        let Some(segment) = structure
            .segments()
            .iter()
            .find(|s| s.is_type(SegmentKind::Exif))
        else {
            return Ok(None);
        };
        let location = segment.location();
        source.seek(SeekFrom::Start(location.offset))?;
        let mut data = vec![0u8; location.size as usize];
        source.read_exact(&mut data).await?;
        crate::tiff::parse_exif_info(&data)
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

        // Header segment is always first — the write loop writes PNG_SIGNATURE for it.
        dest_structure.add_segment(Segment::new(
            0,
            PNG_SIGNATURE.len() as u64,
            SegmentKind::Header,
            Some("PNGh".to_string()),
        ));

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing metadata
        let has_xmp = source_structure.segments.iter().any(|s| s.is_xmp());
        let has_jumbf = source_structure.segments.iter().any(|s| s.is_jumbf());

        for segment in &source_structure.segments {
            match segment {
                segment if segment.is_type(SegmentKind::Header) => {
                    continue; // already added above
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

    fn exclusion_range_for_segment(structure: &Structure, kind: SegmentKind) -> Option<(u64, u64)> {
        let segment = match kind {
            SegmentKind::Jumbf => structure
                .c2pa_jumbf_index()
                .map(|i| &structure.segments()[i]),
            SegmentKind::Xmp => structure.xmp_index().map(|i| &structure.segments()[i]),
            _ => None,
        }?;

        let location = segment.location();
        // PNG chunks: length(4) + type(4) + data(N) + CRC(4)
        // Segment stores data offset/size. CRC immediately follows data.
        // Exclude data + CRC (CRC depends on data, so it changes too)
        Some((location.offset, location.size + 4))
    }
}

/// Plan an in-place PNG segment update with CRC recalculation.
///
/// PNG chunks have a CRC that must be recalculated when data changes:
/// - For JUMBF (caBX): Updates data and recalculates CRC
/// - For XMP (iTXt): Updates XMP data portion and recalculates CRC
///
/// Returns the positioned writes and the number of payload bytes (padded to capacity).
pub(crate) fn png_segment_update_plan(
    structure: &Structure,
    kind: SegmentKind,
    data: Vec<u8>,
) -> Result<crate::structure::SegmentUpdatePlan> {
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
    let written = padded_data.len();

    // The chunk CRC immediately follows the data, so data + CRC is one contiguous write.
    let crc = match kind {
        // caBX chunk: [length:4][type:4][data:N][crc:4]; data_offset points at data.
        SegmentKind::Jumbf => PngIO::calculate_crc(C2PA, &padded_data),
        // iTXt XMP chunk: [length:4][type:4][header:22][xmp:N][crc:4]; data_offset points
        // at the XMP data. CRC covers "iTXt" + keyword(18) + 4 flag/null bytes + XMP.
        SegmentKind::Xmp => {
            let mut chunk_data =
                Vec::with_capacity(ITXT_XMP_HEADER_SIZE as usize + padded_data.len());
            chunk_data.extend_from_slice(XMP_KEYWORD);
            chunk_data.extend_from_slice(&[0, 0, 0, 0]);
            chunk_data.extend_from_slice(&padded_data);
            PngIO::calculate_crc(ITXT, &chunk_data)
        }
        _ => unreachable!(),
    };
    padded_data.extend_from_slice(&crc.to_be_bytes());

    Ok((vec![(data_offset, padded_data)], written))
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
