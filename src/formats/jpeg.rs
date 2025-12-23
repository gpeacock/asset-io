//! JPEG format handler

use crate::{
    error::{Error, Result},
    segment::{LazyData, Location, Segment},
    structure::FileStructure,
    Format, FormatHandler, Updates,
};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{copy, Read, Seek, SeekFrom, Take, Write};

// JPEG markers
const SOI: u8 = 0xD8; // Start of Image
const EOI: u8 = 0xD9; // End of Image
const APP1: u8 = 0xE1; // XMP
const APP11: u8 = 0xEB; // JUMBF
const SOS: u8 = 0xDA; // Start of Scan (image data follows)

// Special markers without length
const RST0: u8 = 0xD0;
const RST7: u8 = 0xD7;
const SOF0: u8 = 0xC0;
const SOF15: u8 = 0xCF;

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const C2PA_MARKER: &[u8] = b"c2pa";
const MAX_MARKER_SIZE: usize = 65533; // Max size for JPEG marker segment

/// JPEG format handler
pub struct JpegHandler;

impl JpegHandler {
    /// Create a new JPEG handler
    pub fn new() -> Self {
        Self
    }
    
    /// Fast single-pass parser
    fn parse_impl<R: Read + Seek>(&self, reader: &mut R) -> Result<FileStructure> {
        let mut structure = FileStructure::new(Format::Jpeg);
        
        // Check SOI marker
        if reader.read_u8()? != 0xFF || reader.read_u8()? != SOI {
            return Err(Error::InvalidFormat("Not a JPEG file".into()));
        }
        
        structure.add_segment(Segment::Header {
            offset: 0,
            size: 2,
        });
        
        let mut offset = 2u64;
        let mut in_scan = false;
        
        loop {
            // Read marker
            let marker_prefix = reader.read_u8()?;
            let marker = reader.read_u8()?;
            
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
                        marker: EOI,
                    });
                    structure.total_size = offset + 2;
                    break;
                }
                
                SOS => {
                    // Start of scan - image data follows
                    let size = reader.read_u16::<BigEndian>()? as u64;
                    
                    // Skip SOS header
                    reader.seek(SeekFrom::Current((size - 2) as i64))?;
                    let image_start = offset + 2 + size;
                    
                    // Find end of image data (scan for FFD9)
                    let image_end = find_eoi(reader)?;
                    
                    structure.add_segment(Segment::ImageData {
                        offset: image_start,
                        size: image_end - image_start,
                        hashable: true,
                    });
                    
                    offset = image_end;
                    reader.seek(SeekFrom::Start(offset))?;
                    in_scan = true;
                }
                
                APP1 => {
                    let size = reader.read_u16::<BigEndian>()? as u64;
                    let data_size = size - 2;
                    
                    // Check for XMP signature
                    let mut sig_buf = vec![0u8; XMP_SIGNATURE.len().min(data_size as usize)];
                    reader.read_exact(&mut sig_buf)?;
                    
                    if sig_buf == XMP_SIGNATURE {
                        // This is an XMP segment
                        structure.add_segment(Segment::Xmp {
                            offset: offset + 4 + XMP_SIGNATURE.len() as u64,
                            size: data_size - XMP_SIGNATURE.len() as u64,
                            data: LazyData::NotLoaded,
                        });
                    } else {
                        // Other APP1 segment
                        structure.add_segment(Segment::Other {
                            offset,
                            size: size + 2,
                            marker: APP1,
                        });
                    }
                    
                    offset += 2 + size;
                    reader.seek(SeekFrom::Start(offset))?;
                }
                
                APP11 => {
                    let size = reader.read_u16::<BigEndian>()? as u64;
                    let data_start = offset + 4;
                    
                    // Check for C2PA marker (JPEG XT box header + c2pa UUID)
                    // Skip JPEG XT header (CI + En + Z = 2 + 2 + 4 = 8 bytes)
                    let mut header = [0u8; 16];
                    reader.read_exact(&mut header)?;
                    
                    if &header[12..16] == C2PA_MARKER {
                        // This is a JUMBF segment
                        structure.add_segment(Segment::Jumbf {
                            offset: data_start,
                            size: size - 2,
                            segments: vec![Location {
                                offset: data_start,
                                size: size - 2,
                            }],
                            data: LazyData::NotLoaded,
                        });
                    } else {
                        structure.add_segment(Segment::Other {
                            offset,
                            size: size + 2,
                            marker: APP11,
                        });
                    }
                    
                    offset += 2 + size;
                    reader.seek(SeekFrom::Start(offset))?;
                }
                
                // RST markers have no length
                RST0..=RST7 => {
                    structure.add_segment(Segment::Other {
                        offset,
                        size: 2,
                        marker,
                    });
                    offset += 2;
                }
                
                _ => {
                    // Standard marker with length
                    let size = reader.read_u16::<BigEndian>()? as u64;
                    structure.add_segment(Segment::Other {
                        offset,
                        size: size + 2,
                        marker,
                    });
                    offset += 2 + size;
                    reader.seek(SeekFrom::Start(offset))?;
                }
            }
        }
        
        Ok(structure)
    }
}

impl Default for JpegHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatHandler for JpegHandler {
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<FileStructure> {
        self.parse_impl(reader)
    }
    
    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        reader.seek(SeekFrom::Start(0))?;
        
        // Write SOI
        writer.write_u8(0xFF)?;
        writer.write_u8(SOI)?;
        
        let mut xmp_written = updates.new_xmp.is_some();
        let mut jumbf_written = updates.new_jumbf.is_some();
        
        for segment in &structure.segments {
            match segment {
                Segment::Header { .. } => {
                    // Already wrote SOI
                    continue;
                }
                
                Segment::Xmp { offset, size, .. } => {
                    if let Some(ref new_xmp) = updates.new_xmp {
                        if !xmp_written {
                            write_xmp_segment(writer, new_xmp)?;
                            xmp_written = true;
                        }
                    } else {
                        // Copy existing XMP
                        copy_segment(reader, writer, *offset, *size, 0xFF, APP1)?;
                    }
                }
                
                Segment::Jumbf { offset, size, .. } => {
                    if updates.remove_existing_jumbf {
                        continue;
                    }
                    
                    if let Some(ref new_jumbf) = updates.new_jumbf {
                        if !jumbf_written {
                            write_jumbf_segments(writer, new_jumbf)?;
                            jumbf_written = true;
                        }
                    } else {
                        // Copy existing JUMBF
                        copy_segment(reader, writer, *offset, *size, 0xFF, APP11)?;
                    }
                }
                
                Segment::ImageData { offset, size, .. } => {
                    // Stream copy image data
                    reader.seek(SeekFrom::Start(*offset))?;
                    let mut limited = reader.take(*size);
                    copy(&mut limited, writer)?;
                }
                
                Segment::Other {
                    offset, size, marker, ..
                } => {
                    // Copy other segments as-is
                    reader.seek(SeekFrom::Start(*offset))?;
                    let mut limited = reader.take(*size);
                    copy(&mut limited, writer)?;
                }
            }
        }
        
        Ok(())
    }
    
    #[cfg(feature = "thumbnails")]
    fn generate_thumbnail<R: Read + Seek>(
        &self,
        _structure: &FileStructure,
        _reader: &mut R,
        _request: &crate::ThumbnailRequest,
    ) -> Result<Option<Vec<u8>>> {
        // TODO: Implement thumbnail generation using image crate
        Ok(None)
    }
}

// Helper functions

/// Find End of Image marker (FFD9)
fn find_eoi<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    let mut prev = 0u8;
    let start_pos = reader.stream_position()?;
    
    loop {
        let byte = reader.read_u8()?;
        
        if prev == 0xFF && byte == EOI {
            // Found EOI, return position of FF
            return Ok(reader.stream_position()? - 1);
        }
        
        prev = byte;
    }
}

/// Copy a segment with marker prefix
fn copy_segment<R: Read + Seek, W: Write>(
    reader: &mut R,
    writer: &mut W,
    offset: u64,
    size: u64,
    marker_prefix: u8,
    marker: u8,
) -> Result<()> {
    writer.write_u8(marker_prefix)?;
    writer.write_u8(marker)?;
    writer.write_u16::<BigEndian>((size + 2) as u16)?;
    
    reader.seek(SeekFrom::Start(offset))?;
    let mut limited = reader.take(size);
    copy(&mut limited, writer)?;
    
    Ok(())
}

/// Write XMP as APP1 segment
fn write_xmp_segment<W: Write>(writer: &mut W, xmp: &[u8]) -> Result<()> {
    let total_size = XMP_SIGNATURE.len() + xmp.len() + 2;
    
    if total_size > MAX_MARKER_SIZE {
        return Err(Error::DataTooLarge {
            size: total_size,
            max: MAX_MARKER_SIZE,
        });
    }
    
    writer.write_u8(0xFF)?;
    writer.write_u8(APP1)?;
    writer.write_u16::<BigEndian>(total_size as u16)?;
    writer.write_all(XMP_SIGNATURE)?;
    writer.write_all(xmp)?;
    
    Ok(())
}

/// Write JUMBF data as one or more APP11 segments
fn write_jumbf_segments<W: Write>(writer: &mut W, jumbf: &[u8]) -> Result<()> {
    // JPEG XT header: CI (2) + En (2) + Z (4) + LBox (4) + TBox (4) = 16 bytes
    const JPEG_XT_OVERHEAD: usize = 16;
    const MAX_DATA_PER_SEGMENT: usize = MAX_MARKER_SIZE - JPEG_XT_OVERHEAD;
    
    let num_segments = (jumbf.len() + MAX_DATA_PER_SEGMENT - 1) / MAX_DATA_PER_SEGMENT;
    
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
        let mut reader = Cursor::new(data);
        
        let handler = JpegHandler::new();
        let structure = handler.parse(&mut reader).unwrap();
        
        assert_eq!(structure.format, Format::Jpeg);
        assert_eq!(structure.total_size, 4);
        assert_eq!(structure.segments.len(), 2); // Header + EOI
    }
}

