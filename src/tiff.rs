//! Minimal TIFF/EXIF parser
//!
//! This module provides lightweight TIFF parsing for:
//! - Extracting embedded thumbnails from EXIF data
//! - Reading basic EXIF metadata (Make, Model, DateTime, etc.)
//!
//! For full TIFF container support, see `formats/tiff_io.rs` (future).
//!
//! TIFF Structure:
//! - Header: byte order (II/MM), magic (0x002A), IFD offset
//! - IFD (Image File Directory): tag count, tags (12 bytes each), next IFD offset
//! - Tags: tag ID (2), type (2), count (4), value/offset (4)

use crate::error::Result;
use std::io::{Cursor, Read, Seek, SeekFrom};

/// TIFF/EXIF tag IDs
#[allow(dead_code)]
mod tags {
    // IFD0 (main image) tags
    pub const IMAGE_WIDTH: u16 = 0x0100;
    pub const IMAGE_LENGTH: u16 = 0x0101;
    pub const MAKE: u16 = 0x010F;
    pub const MODEL: u16 = 0x0110;
    pub const ORIENTATION: u16 = 0x0112;
    pub const SOFTWARE: u16 = 0x0131;
    pub const DATE_TIME: u16 = 0x0132;
    pub const ARTIST: u16 = 0x013B;
    pub const COPYRIGHT: u16 = 0x8298;
    pub const EXIF_IFD_POINTER: u16 = 0x8769;

    // EXIF sub-IFD tags
    pub const EXPOSURE_TIME: u16 = 0x829A;
    pub const F_NUMBER: u16 = 0x829D;
    pub const ISO_SPEED: u16 = 0x8827;
    pub const DATE_TIME_ORIGINAL: u16 = 0x9003;
    pub const FOCAL_LENGTH: u16 = 0x920A;

    // IFD1 (thumbnail) tags
    pub const JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
    pub const JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;
}

/// TIFF data types
#[allow(dead_code)]
mod types {
    pub const BYTE: u16 = 1;
    pub const ASCII: u16 = 2;
    pub const SHORT: u16 = 3;
    pub const LONG: u16 = 4;
    pub const RATIONAL: u16 = 5;
}

/// Byte order for reading multi-byte values
#[derive(Debug, Clone, Copy)]
enum ByteOrder {
    LittleEndian,
    BigEndian,
}

impl ByteOrder {
    fn read_u16(&self, data: &[u8]) -> u16 {
        match self {
            ByteOrder::LittleEndian => u16::from_le_bytes([data[0], data[1]]),
            ByteOrder::BigEndian => u16::from_be_bytes([data[0], data[1]]),
        }
    }

    fn read_u32(&self, data: &[u8]) -> u32 {
        match self {
            ByteOrder::LittleEndian => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            ByteOrder::BigEndian => u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        }
    }
}


/// Information about an embedded thumbnail
#[derive(Debug)]
pub struct ThumbnailInfo {
    /// Offset of thumbnail JPEG data within the EXIF segment
    pub offset: u32,
    /// Size of thumbnail JPEG data in bytes
    pub size: u32,
    /// Width in pixels (if present)
    pub width: Option<u32>,
    /// Height in pixels (if present)
    pub height: Option<u32>,
}

/// Basic EXIF metadata extracted from IFD0
#[derive(Debug, Default)]
pub struct ExifInfo {
    /// Camera manufacturer (e.g., "Canon", "Nikon")
    pub make: Option<String>,
    /// Camera model (e.g., "EOS R5", "D850")
    pub model: Option<String>,
    /// Image orientation (1-8, where 1 is normal)
    pub orientation: Option<u16>,
    /// Software used to create/edit the image
    pub software: Option<String>,
    /// Date and time of image creation (format: "YYYY:MM:DD HH:MM:SS")
    pub date_time: Option<String>,
    /// Original capture date/time (from EXIF sub-IFD)
    pub date_time_original: Option<String>,
    /// Artist/photographer name
    pub artist: Option<String>,
    /// Copyright notice
    pub copyright: Option<String>,
}

impl std::fmt::Display for ExifInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if let Some(ref make) = self.make {
            parts.push(make.clone());
        }
        if let Some(ref model) = self.model {
            parts.push(model.clone());
        }
        if let Some(dt) = self.date_time_original.as_ref().or(self.date_time.as_ref()) {
            parts.push(dt.clone());
        }
        if parts.is_empty() {
            write!(f, "(no metadata)")
        } else {
            write!(f, "{}", parts.join(" | "))
        }
    }
}

/// Maximum number of tags in an IFD (prevents DOS attacks)
const MAX_IFD_TAGS: u16 = 1000;

/// Parse EXIF data to find embedded thumbnail location
///
/// This expects the EXIF data starting AFTER the "Exif\0\0" signature,
/// i.e., starting with the TIFF header.
pub fn parse_thumbnail_info(exif_data: &[u8]) -> Result<Option<ThumbnailInfo>> {
    if exif_data.len() < 8 {
        return Ok(None);
    }

    let mut cursor = Cursor::new(exif_data);

    // Parse TIFF header
    let mut header = [0u8; 8];
    cursor.read_exact(&mut header)?;

    // Byte order: "II" (0x4949) = little endian, "MM" (0x4D4D) = big endian
    let byte_order = match &header[0..2] {
        b"II" => ByteOrder::LittleEndian,
        b"MM" => ByteOrder::BigEndian,
        _ => return Ok(None), // Not valid TIFF
    };

    // Check magic number (0x002A)
    let magic = byte_order.read_u16(&header[2..4]);
    if magic != 0x002A {
        return Ok(None);
    }

    // Get IFD0 offset
    let ifd0_offset = byte_order.read_u32(&header[4..8]);

    // Validate IFD0 offset is within bounds
    if ifd0_offset as usize >= exif_data.len() {
        return Ok(None); // Invalid offset
    }

    // Parse IFD0 to find IFD1 offset
    let ifd1_offset = match parse_ifd(&mut cursor, ifd0_offset, byte_order, exif_data.len())? {
        Some(offset) => offset,
        None => return Ok(None), // No IFD1
    };

    // Validate IFD1 offset is within bounds
    if ifd1_offset as usize >= exif_data.len() {
        return Ok(None); // Invalid offset
    }

    // Parse IFD1 to find thumbnail tags
    parse_thumbnail_from_ifd(&mut cursor, ifd1_offset, byte_order, exif_data.len())
}

/// Parse EXIF data to extract basic metadata
///
/// This expects the EXIF data starting AFTER the "Exif\0\0" signature,
/// i.e., starting with the TIFF header.
pub fn parse_exif_info(exif_data: &[u8]) -> Result<Option<ExifInfo>> {
    if exif_data.len() < 8 {
        return Ok(None);
    }

    let mut cursor = Cursor::new(exif_data);

    // Parse TIFF header
    let mut header = [0u8; 8];
    cursor.read_exact(&mut header)?;

    let byte_order = match &header[0..2] {
        b"II" => ByteOrder::LittleEndian,
        b"MM" => ByteOrder::BigEndian,
        _ => return Ok(None),
    };

    let magic = byte_order.read_u16(&header[2..4]);
    if magic != 0x002A {
        return Ok(None);
    }

    let ifd0_offset = byte_order.read_u32(&header[4..8]);
    if ifd0_offset as usize >= exif_data.len() {
        return Ok(None);
    }

    // Parse IFD0 for basic EXIF info
    parse_exif_from_ifd(&cursor, ifd0_offset, byte_order, exif_data)
}

/// Parse IFD0 to extract basic EXIF metadata
fn parse_exif_from_ifd(
    cursor: &Cursor<&[u8]>,
    offset: u32,
    byte_order: ByteOrder,
    data: &[u8],
) -> Result<Option<ExifInfo>> {
    if offset as usize >= data.len() {
        return Ok(None);
    }

    let mut cursor = cursor.clone();
    cursor.seek(SeekFrom::Start(offset as u64))?;

    let mut count_bytes = [0u8; 2];
    if cursor.read_exact(&mut count_bytes).is_err() {
        return Ok(None);
    }
    let tag_count = byte_order.read_u16(&count_bytes);

    if tag_count > MAX_IFD_TAGS {
        return Ok(None);
    }

    let mut info = ExifInfo::default();
    let mut exif_ifd_offset: Option<u32> = None;

    for _ in 0..tag_count {
        let mut tag_bytes = [0u8; 12];
        if cursor.read_exact(&mut tag_bytes).is_err() {
            break;
        }

        let tag_id = byte_order.read_u16(&tag_bytes[0..2]);
        let tag_type = byte_order.read_u16(&tag_bytes[2..4]);
        let count = byte_order.read_u32(&tag_bytes[4..8]);
        let value_or_offset = byte_order.read_u32(&tag_bytes[8..12]);

        match tag_id {
            tags::MAKE => {
                info.make = read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
            }
            tags::MODEL => {
                info.model = read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
            }
            tags::ORIENTATION => {
                if tag_type == types::SHORT {
                    info.orientation = Some(value_or_offset as u16);
                }
            }
            tags::SOFTWARE => {
                info.software = read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
            }
            tags::DATE_TIME => {
                info.date_time = read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
            }
            tags::ARTIST => {
                info.artist = read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
            }
            tags::COPYRIGHT => {
                info.copyright = read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
            }
            tags::EXIF_IFD_POINTER => {
                if tag_type == types::LONG {
                    exif_ifd_offset = Some(value_or_offset);
                }
            }
            _ => {}
        }
    }

    // Parse EXIF sub-IFD for DateTimeOriginal if present
    if let Some(exif_offset) = exif_ifd_offset {
        if let Some(dt_orig) = parse_exif_subifd_datetime(data, exif_offset, byte_order) {
            info.date_time_original = Some(dt_orig);
        }
    }

    Ok(Some(info))
}

/// Read an ASCII string tag value
fn read_ascii_tag(
    data: &[u8],
    tag_type: u16,
    count: u32,
    value_or_offset: u32,
    byte_order: ByteOrder,
) -> Option<String> {
    if tag_type != types::ASCII || count == 0 {
        return None;
    }

    let bytes = if count <= 4 {
        // Value is inline in the tag
        let val_bytes = match byte_order {
            ByteOrder::LittleEndian => value_or_offset.to_le_bytes(),
            ByteOrder::BigEndian => value_or_offset.to_be_bytes(),
        };
        val_bytes[..count as usize].to_vec()
    } else {
        // Value is at offset
        let offset = value_or_offset as usize;
        let end = offset + count as usize;
        if end > data.len() {
            return None;
        }
        data[offset..end].to_vec()
    };

    // Convert to string, trimming null terminator
    String::from_utf8(bytes)
        .ok()
        .map(|s| s.trim_end_matches('\0').trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse EXIF sub-IFD to get DateTimeOriginal
fn parse_exif_subifd_datetime(data: &[u8], offset: u32, byte_order: ByteOrder) -> Option<String> {
    if offset as usize >= data.len() {
        return None;
    }

    let mut cursor = Cursor::new(data);
    cursor.seek(SeekFrom::Start(offset as u64)).ok()?;

    let mut count_bytes = [0u8; 2];
    cursor.read_exact(&mut count_bytes).ok()?;
    let tag_count = byte_order.read_u16(&count_bytes);

    if tag_count > MAX_IFD_TAGS {
        return None;
    }

    for _ in 0..tag_count {
        let mut tag_bytes = [0u8; 12];
        if cursor.read_exact(&mut tag_bytes).is_err() {
            break;
        }

        let tag_id = byte_order.read_u16(&tag_bytes[0..2]);
        let tag_type = byte_order.read_u16(&tag_bytes[2..4]);
        let count = byte_order.read_u32(&tag_bytes[4..8]);
        let value_or_offset = byte_order.read_u32(&tag_bytes[8..12]);

        if tag_id == tags::DATE_TIME_ORIGINAL {
            return read_ascii_tag(data, tag_type, count, value_or_offset, byte_order);
        }
    }

    None
}

/// Parse an IFD and return the offset to the next IFD (if any)
fn parse_ifd(
    cursor: &mut Cursor<&[u8]>,
    offset: u32,
    byte_order: ByteOrder,
    data_len: usize,
) -> Result<Option<u32>> {
    // Validate offset
    if offset as usize >= data_len {
        return Ok(None);
    }

    cursor.seek(SeekFrom::Start(offset as u64))?;

    // Read tag count
    let mut count_bytes = [0u8; 2];
    if cursor.read_exact(&mut count_bytes).is_err() {
        return Ok(None); // Can't read tag count
    }
    let tag_count = byte_order.read_u16(&count_bytes);

    // Validate tag count to prevent DOS attacks
    if tag_count > MAX_IFD_TAGS {
        return Ok(None); // Suspiciously large number of tags
    }

    // Skip all tags (12 bytes each)
    cursor.seek(SeekFrom::Current((tag_count as i64) * 12))?;

    // Read next IFD offset
    let mut next_bytes = [0u8; 4];
    if cursor.read_exact(&mut next_bytes).is_err() {
        return Ok(None); // Can't read next offset
    }
    let next_offset = byte_order.read_u32(&next_bytes);

    if next_offset == 0 || next_offset as usize >= data_len {
        Ok(None)
    } else {
        Ok(Some(next_offset))
    }
}

/// Parse IFD1 to extract thumbnail information
fn parse_thumbnail_from_ifd(
    cursor: &mut Cursor<&[u8]>,
    offset: u32,
    byte_order: ByteOrder,
    data_len: usize,
) -> Result<Option<ThumbnailInfo>> {
    // Validate offset
    if offset as usize >= data_len {
        return Ok(None);
    }

    cursor.seek(SeekFrom::Start(offset as u64))?;

    // Read tag count
    let mut count_bytes = [0u8; 2];
    if cursor.read_exact(&mut count_bytes).is_err() {
        return Ok(None);
    }
    let tag_count = byte_order.read_u16(&count_bytes);

    // Validate tag count
    if tag_count > MAX_IFD_TAGS {
        return Ok(None);
    }

    // Extract thumbnail information from tags
    let mut thumb_offset = None;
    let mut thumb_size = None;
    let mut width = None;
    let mut height = None;

    for _ in 0..tag_count {
        let mut tag_bytes = [0u8; 12];
        if cursor.read_exact(&mut tag_bytes).is_err() {
            break;
        }

        let tag_id = byte_order.read_u16(&tag_bytes[0..2]);
        let tag_type = byte_order.read_u16(&tag_bytes[2..4]);
        let value = byte_order.read_u32(&tag_bytes[8..12]);

        // We only care about SHORT and LONG types for thumbnail tags
        if tag_type == types::SHORT || tag_type == types::LONG {
            match tag_id {
                tags::JPEG_INTERCHANGE_FORMAT => thumb_offset = Some(value),
                tags::JPEG_INTERCHANGE_FORMAT_LENGTH => thumb_size = Some(value),
                tags::IMAGE_WIDTH => width = Some(value),
                tags::IMAGE_LENGTH => height = Some(value),
                _ => {}
            }
        }
    }

    // Both offset and size are required for a valid thumbnail
    match (thumb_offset, thumb_size) {
        (Some(offset), Some(size)) => Ok(Some(ThumbnailInfo {
            offset,
            size,
            width,
            height,
        })),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_order() {
        let be = ByteOrder::BigEndian;
        let le = ByteOrder::LittleEndian;

        assert_eq!(be.read_u16(&[0x12, 0x34]), 0x1234);
        assert_eq!(le.read_u16(&[0x34, 0x12]), 0x1234);

        assert_eq!(be.read_u32(&[0x12, 0x34, 0x56, 0x78]), 0x12345678);
        assert_eq!(le.read_u32(&[0x78, 0x56, 0x34, 0x12]), 0x12345678);
    }
}
