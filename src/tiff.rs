//! Minimal TIFF/EXIF parser
//!
//! This module provides just enough TIFF parsing to extract embedded thumbnails from EXIF data.
//! It will eventually be expanded to support full TIFF format parsing.
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
    pub const IMAGE_WIDTH: u16 = 0x0100;
    pub const IMAGE_LENGTH: u16 = 0x0101;
    pub const JPEG_INTERCHANGE_FORMAT: u16 = 0x0201; // Thumbnail offset
    pub const JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202; // Thumbnail size
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

/// A parsed TIFF tag
#[derive(Debug)]
struct TiffTag {
    tag: u16,
    value: u32,
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

    // Read all tags
    let mut tags = Vec::new();
    for _ in 0..tag_count {
        let mut tag_bytes = [0u8; 12];
        if cursor.read_exact(&mut tag_bytes).is_err() {
            break; // Can't read tag, stop parsing
        }

        let tag_id = byte_order.read_u16(&tag_bytes[0..2]);
        let tag_type = byte_order.read_u16(&tag_bytes[2..4]);
        let _count = byte_order.read_u32(&tag_bytes[4..8]);
        let value = byte_order.read_u32(&tag_bytes[8..12]);

        // We only care about SHORT and LONG types for thumbnail tags
        if tag_type == types::SHORT || tag_type == types::LONG {
            tags.push(TiffTag { tag: tag_id, value });
        }
    }

    // Extract thumbnail information from tags
    let mut thumb_offset = None;
    let mut thumb_size = None;
    let mut width = None;
    let mut height = None;

    for tag in tags {
        match tag.tag {
            tags::JPEG_INTERCHANGE_FORMAT => thumb_offset = Some(tag.value),
            tags::JPEG_INTERCHANGE_FORMAT_LENGTH => thumb_size = Some(tag.value),
            tags::IMAGE_WIDTH => width = Some(tag.value),
            tags::IMAGE_LENGTH => height = Some(tag.value),
            _ => {}
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
