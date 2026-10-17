//! JPEG container I/O implementation

#[derive(Debug, Clone)]
struct JpegExtendedXmp {
    guid: String,
    chunk_offsets: Vec<u32>,
    total_size: u32,
}

impl JpegExtendedXmp {
    fn parts(&self) -> (&str, &[u32], u32) {
        (&self.guid, &self.chunk_offsets, self.total_size)
    }
}

use super::{ContainerIO, ContainerKind};
use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind},
    structure::Structure,
    Updates,
};
use std::io::SeekFrom;

use crate::sans_io::io::{PDst, Src};

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
    #[allow(dead_code)]
    pub fn container_type() -> ContainerKind {
        ContainerKind::Jpeg
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
    pub fn detect(header: &[u8]) -> Option<crate::ContainerKind> {
        // JPEG magic bytes: FF D8
        if header.len() >= 2 && header[0] == 0xFF && header[1] == 0xD8 {
            Some(ContainerKind::Jpeg)
        } else {
            None
        }
    }
}

impl JpegIO {
    pub(crate) async fn parse_async(&self, source: &mut Src) -> Result<Structure> {
        let mut structure = Structure::new(ContainerKind::Jpeg, crate::MediaType::Jpeg);
        if source.read_u8().await? != 0xFF || source.read_u8().await? != SOI {
            return Err(Error::InvalidFormat("Not a JPEG file".into()));
        }
        structure.add_segment(Segment::new(
            0,
            2,
            SegmentKind::Header,
            Some(marker_label(SOI).to_string()),
        ));
        let mut offset = 2u64;

        loop {
            let marker_prefix = source.read_u8().await?;
            let marker = source.read_u8().await?;
            if marker_prefix != 0xFF {
                return Err(Error::InvalidSegment {
                    offset,
                    reason: format!("Expected 0xFF, got 0x{:02X}", marker_prefix),
                });
            }
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
                    let size = source.read_u16_be().await? as u64;
                    let sos_start = offset;
                    source.seek(SeekFrom::Current(size.saturating_sub(2) as i64))?;
                    let image_end = find_eoi_async(source).await?;
                    structure.add_segment(Segment::new(
                        sos_start,
                        image_end - sos_start,
                        SegmentKind::ImageData,
                        Some(marker_label(SOS).to_string()),
                    ));
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
                    let size = source.read_u16_be().await? as u64;
                    let data_size = size.saturating_sub(2);
                    let segment_start = offset;
                    let sig_len = XMP_EXTENDED_SIGNATURE.len().max(XMP_SIGNATURE.len());
                    let bytes_to_read = sig_len.min(data_size as usize);
                    let mut sig_buf = vec![0u8; bytes_to_read];
                    source.read_exact(&mut sig_buf).await?;

                    if sig_buf.len() >= XMP_SIGNATURE.len()
                        && &sig_buf[..XMP_SIGNATURE.len()] == XMP_SIGNATURE
                    {
                        let xmp_offset = offset + 4 + XMP_SIGNATURE.len() as u64;
                        let xmp_size = data_size - XMP_SIGNATURE.len() as u64;
                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(xmp_offset, xmp_size)],
                            SegmentKind::Xmp,
                            Some(marker_label(APP1).to_string()),
                        )?);
                        source.seek(SeekFrom::Current(
                            (data_size as usize - sig_buf.len()) as i64,
                        ))?;
                    } else if sig_buf.len() >= XMP_EXTENDED_SIGNATURE.len()
                        && &sig_buf[..XMP_EXTENDED_SIGNATURE.len()] == XMP_EXTENDED_SIGNATURE
                    {
                        const HEADER_SIZE: u64 = 32 + 4 + 4;
                        if data_size < XMP_EXTENDED_SIGNATURE.len() as u64 + HEADER_SIZE {
                            source.seek(SeekFrom::Current(
                                (data_size as usize - sig_buf.len()) as i64,
                            ))?;
                            structure.add_segment(Segment::new(
                                segment_start,
                                size + 2,
                                SegmentKind::Other,
                                Some(marker_label(APP1).to_string()),
                            ));
                        } else {
                            let mut guid_bytes = [0u8; 32];
                            source.read_exact(&mut guid_bytes).await?;
                            let guid = String::from_utf8_lossy(&guid_bytes).to_string();
                            let total_size = source.read_u32_be().await?;
                            let chunk_offset = source.read_u32_be().await?;
                            let chunk_data_offset =
                                offset + 4 + XMP_EXTENDED_SIGNATURE.len() as u64 + HEADER_SIZE;
                            let chunk_data_size =
                                data_size - XMP_EXTENDED_SIGNATURE.len() as u64 - HEADER_SIZE;
                            if let Some(idx) = *structure.xmp_index_mut() {
                                let segment = &mut structure.segments[idx];
                                if segment.is_xmp() {
                                    segment
                                        .ranges
                                        .push(ByteRange::new(chunk_data_offset, chunk_data_size));
                                    match &mut segment.metadata {
                                        Some(metadata) => {
                                            let JpegExtendedXmp {
                                                guid: existing_guid,
                                                chunk_offsets,
                                                total_size: existing_total,
                                            } = metadata.get_mut::<JpegExtendedXmp>().ok_or_else(
                                                || {
                                                    Error::InvalidFormat(
                                                        "Unexpected metadata on JPEG XMP segment"
                                                            .into(),
                                                    )
                                                },
                                            )?;
                                            if existing_guid == &guid
                                                && *existing_total == total_size
                                            {
                                                chunk_offsets.push(chunk_offset);
                                            }
                                        }
                                        None => {
                                            segment.metadata = Some(crate::SegmentMetadata::new(
                                                JpegExtendedXmp {
                                                    guid,
                                                    chunk_offsets: vec![chunk_offset],
                                                    total_size,
                                                },
                                            ));
                                        }
                                    }
                                }
                            } else {
                                structure.add_segment(Segment::new(
                                    segment_start,
                                    size + 2,
                                    SegmentKind::Other,
                                    Some(marker_label(APP1).to_string()),
                                ));
                            }
                            source.seek(SeekFrom::Current(chunk_data_size as i64))?;
                        }
                    } else if sig_buf.len() >= EXIF_SIGNATURE.len()
                        && &sig_buf[..EXIF_SIGNATURE.len()] == EXIF_SIGNATURE
                    {
                        #[cfg(feature = "exif")]
                        {
                            let remaining_to_read = data_size as usize - sig_buf.len();
                            let mut remaining_data = vec![0u8; remaining_to_read];
                            source.read_exact(&mut remaining_data).await?;
                            let mut exif_data = Vec::new();
                            exif_data.extend_from_slice(&sig_buf[EXIF_SIGNATURE.len()..]);
                            exif_data.extend_from_slice(&remaining_data);
                            let thumbnail = match crate::tiff::parse_thumbnail_info(&exif_data) {
                                Ok(Some(info)) => {
                                    let thumb_offset = segment_start
                                        + 4
                                        + EXIF_SIGNATURE.len() as u64
                                        + info.offset as u64;
                                    Some(crate::thumbnail::EmbeddedThumbnailInfo::new(
                                        thumb_offset,
                                        info.size as u64,
                                        crate::thumbnail::ThumbnailKind::Jpeg,
                                        info.width,
                                        info.height,
                                    ))
                                }
                                Ok(None) | Err(_) => None,
                            };
                            let mut segment = Segment::new(
                                segment_start,
                                size + 2,
                                SegmentKind::Exif,
                                Some(marker_label(APP1).to_string()),
                            );
                            if let Some(info) = thumbnail {
                                segment = segment.with_metadata(crate::SegmentMetadata::new(info));
                            }
                            structure.add_segment(segment);
                        }
                        #[cfg(not(feature = "exif"))]
                        {
                            structure.add_segment(Segment::new(
                                segment_start,
                                size + 2,
                                SegmentKind::Exif,
                                Some(marker_label(APP1).to_string()),
                            ));
                            source.seek(SeekFrom::Current(
                                (data_size as usize - sig_buf.len()) as i64,
                            ))?;
                        }
                    } else {
                        source.seek(SeekFrom::Current(
                            (data_size as usize - sig_buf.len()) as i64,
                        ))?;
                        structure.add_segment(Segment::new(
                            segment_start,
                            size + 2,
                            SegmentKind::Other,
                            Some(marker_label(APP1).to_string()),
                        ));
                    }
                    if (sig_buf.len() < EXIF_SIGNATURE.len()
                        || &sig_buf[..EXIF_SIGNATURE.len()] != EXIF_SIGNATURE)
                        && !(sig_buf.len() >= XMP_SIGNATURE.len()
                            && &sig_buf[..XMP_SIGNATURE.len()] == XMP_SIGNATURE)
                        && !(sig_buf.len() >= XMP_EXTENDED_SIGNATURE.len()
                            && &sig_buf[..XMP_EXTENDED_SIGNATURE.len()] == XMP_EXTENDED_SIGNATURE)
                    {
                        source.seek(SeekFrom::Current(
                            (data_size as usize - sig_buf.len()) as i64,
                        ))?;
                        structure.add_segment(Segment::new(
                            segment_start,
                            size + 2,
                            SegmentKind::Other,
                            Some(marker_label(APP1).to_string()),
                        ));
                    }
                    offset += 2 + size;
                }
                APP11 => {
                    let size = source.read_u16_be().await? as u64;
                    let data_size = size.saturating_sub(2);
                    let data_start = offset + 4;
                    let segment_start = offset;
                    let mut header = [0u8; 16];
                    let bytes_to_read = header.len().min(data_size as usize);
                    source.read_exact(&mut header[..bytes_to_read]).await?;
                    let is_jpeg_xt = bytes_to_read >= 8 && &header[0..2] == b"JP";
                    let has_jumb_box_after_xt = bytes_to_read >= 16 && &header[12..16] == b"jumb";
                    let has_jumb_box_direct = bytes_to_read >= 8 && &header[4..8] == b"jumb";
                    let is_jumbf = (is_jpeg_xt && has_jumb_box_after_xt) || has_jumb_box_direct;
                    if is_jumbf {
                        let (jumbf_data_offset, jumbf_data_size) = if is_jpeg_xt {
                            let seq_num =
                                u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
                            let overhead = if seq_num > 1 { 16 } else { 8 };
                            (data_start + overhead, data_size - overhead)
                        } else {
                            (data_start, data_size)
                        };
                        let seq_num = if is_jpeg_xt {
                            u32::from_be_bytes([header[4], header[5], header[6], header[7]])
                        } else {
                            1
                        };
                        let mut is_continuation = false;
                        if seq_num > 1 {
                            if let Some(last_segment) = structure.segments.last_mut() {
                                if last_segment.is_jumbf() {
                                    last_segment
                                        .ranges
                                        .push(ByteRange::new(jumbf_data_offset, jumbf_data_size));
                                    is_continuation = true;
                                }
                            }
                        }
                        if !is_continuation {
                            structure.add_segment(Segment::with_ranges(
                                vec![ByteRange::new(jumbf_data_offset, jumbf_data_size)],
                                SegmentKind::Jumbf,
                                Some(marker_label(APP11).to_string()),
                            )?);
                        }
                        source
                            .seek(SeekFrom::Current((data_size - bytes_to_read as u64) as i64))?;
                    } else {
                        source
                            .seek(SeekFrom::Current((data_size - bytes_to_read as u64) as i64))?;
                        structure.add_segment(Segment::new(
                            segment_start,
                            size + 2,
                            SegmentKind::Other,
                            Some(marker_label(APP11).to_string()),
                        ));
                    }
                    offset += 2 + size;
                }
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
                    let size = source.read_u16_be().await? as u64;
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

    pub(crate) async fn read_xmp_async(
        &self,
        structure: &Structure,
        source: &mut Src,
    ) -> Result<Option<Vec<u8>>> {
        let Some(index) = structure.xmp_index() else {
            return Ok(None);
        };
        let segment = &structure.segments()[index];
        if !segment.is_xmp() {
            return Ok(None);
        }
        if let Some((guid, offsets, total_size)) = segment
            .metadata
            .as_ref()
            .and_then(|meta| meta.get::<JpegExtendedXmp>())
            .map(JpegExtendedXmp::parts)
        {
            return reassemble_extended_xmp_async(
                source,
                &segment.ranges,
                guid,
                offsets,
                total_size,
            )
            .await;
        }
        if segment.ranges.len() == 1 {
            let range = segment.ranges[0];
            source.seek(SeekFrom::Start(range.offset))?;
            return Ok(Some(source.read_vec(range.size as usize).await?));
        }
        let total_size = segment.ranges.iter().map(|r| r.size).sum::<u64>();
        let mut data = Vec::with_capacity(total_size as usize);
        for range in &segment.ranges {
            source.seek(SeekFrom::Start(range.offset))?;
            let mut chunk = vec![0u8; range.size as usize];
            source.read_exact(&mut chunk).await?;
            data.extend_from_slice(&chunk);
        }
        Ok(Some(data))
    }

    pub(crate) async fn read_jumbf_async(
        &self,
        structure: &Structure,
        source: &mut Src,
    ) -> Result<Option<Vec<u8>>> {
        if structure.jumbf_indices().is_empty() {
            return Ok(None);
        }
        let mut result = Vec::new();
        for &index in structure.jumbf_indices() {
            let segment = &structure.segments()[index];
            if !segment.is_jumbf() {
                continue;
            }
            for range in &segment.ranges {
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

    #[cfg(feature = "exif")]
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

    #[cfg(feature = "exif")]
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
        let data = source.read_vec(location.size as usize).await?;
        let exif_data = if data.len() > 10 && data[4..10] == [b'E', b'x', b'i', b'f', 0, 0] {
            &data[10..]
        } else if data.len() > 4 {
            &data[4..]
        } else {
            return Ok(None);
        };
        crate::tiff::parse_exif_info(exif_data)
    }

    pub(crate) async fn write_async(
        &self,
        structure: &Structure,
        source: &mut Src,
        pw: &mut PDst<'_>,
        updates: &Updates,
    ) -> Result<()> {
        let exclude_jumbf = updates
            .processing
            .exclude_segments
            .contains(&SegmentKind::Jumbf);
        let data_only = updates.processing.exclusion_mode == crate::ExclusionMode::DataOnly;
        let dest_structure = self.calculate_updated_structure(structure, updates)?;
        source.seek(SeekFrom::Start(0))?;
        for dest_segment in &dest_structure.segments {
            pw.begin_segment(dest_segment).await?;
            match dest_segment {
                seg if seg.is_type(SegmentKind::Header) => {
                    pw.write_u8(0xFF).await?;
                    pw.write_u8(SOI).await?;
                }
                seg if seg.is_xmp() => match &updates.xmp {
                    crate::MetadataUpdate::Set(xmp) => write_xmp_segment_async(pw, xmp).await?,
                    crate::MetadataUpdate::Keep => {
                        if let Some(source_seg) = structure.segments.iter().find(|s| s.is_xmp()) {
                            if let Some((guid, offsets, _)) = source_seg
                                .metadata
                                .as_ref()
                                .and_then(|meta| meta.get::<JpegExtendedXmp>())
                                .map(JpegExtendedXmp::parts)
                            {
                                if !source_seg.ranges.is_empty() {
                                    pw.write_u8(0xFF).await?;
                                    pw.write_u8(APP1).await?;
                                    pw.write_u16_be(
                                        (source_seg.ranges[0].size + XMP_SIGNATURE.len() as u64 + 2)
                                            as u16,
                                    )
                                    .await?;
                                    pw.write_all(XMP_SIGNATURE).await?;
                                    copy_range_async(
                                        source,
                                        pw,
                                        source_seg.ranges[0].offset,
                                        source_seg.ranges[0].size,
                                    )
                                    .await?;
                                }
                                for (i, range) in source_seg.ranges[1..].iter().enumerate() {
                                    let chunk_offset = offsets.get(i).copied().unwrap_or(0);
                                    pw.write_u8(0xFF).await?;
                                    pw.write_u8(APP1).await?;
                                    let seg_size = XMP_EXTENDED_SIGNATURE.len()
                                        + 32
                                        + 4
                                        + 4
                                        + range.size as usize
                                        + 2;
                                    pw.write_u16_be(seg_size as u16).await?;
                                    pw.write_all(XMP_EXTENDED_SIGNATURE).await?;
                                    let guid_bytes = guid.as_bytes();
                                    pw.write_all(&guid_bytes[..guid_bytes.len().min(32)])
                                        .await?;
                                    for _ in guid_bytes.len()..32 {
                                        pw.write_u8(0).await?;
                                    }
                                    pw.write_u32_be(
                                        source_seg.ranges[1..].iter().map(|r| r.size as u32).sum(),
                                    )
                                    .await?;
                                    pw.write_u32_be(chunk_offset).await?;
                                    copy_range_async(source, pw, range.offset, range.size).await?;
                                }
                                continue;
                            }
                            pw.write_u8(0xFF).await?;
                            pw.write_u8(APP1).await?;
                            pw.write_u16_be(
                                (source_seg.ranges[0].size + XMP_SIGNATURE.len() as u64 + 2) as u16,
                            )
                            .await?;
                            pw.write_all(XMP_SIGNATURE).await?;
                            copy_range_async(
                                source,
                                pw,
                                source_seg.ranges[0].offset,
                                source_seg.ranges[0].size,
                            )
                            .await?;
                        }
                    }
                    crate::MetadataUpdate::Remove => {}
                },
                seg if seg.is_jumbf() => match &updates.jumbf {
                    crate::MetadataUpdate::Set(jumbf) => {
                        if data_only {
                            write_jumbf_with_exclusion_async(pw, jumbf, exclude_jumbf).await?;
                        } else {
                            pw.set_exclude_mode(exclude_jumbf);
                            write_jumbf_segments_async(pw, jumbf).await?;
                            pw.set_exclude_mode(false);
                        }
                    }
                    crate::MetadataUpdate::Keep => {
                        if let Some(source_seg) = structure.segments.iter().find(|s| s.is_jumbf()) {
                            let total_size = source_seg.ranges.iter().map(|r| r.size).sum::<u64>();
                            let mut jumbf = vec![0u8; total_size as usize];
                            let mut offset = 0usize;
                            for range in &source_seg.ranges {
                                source.seek(SeekFrom::Start(range.offset))?;
                                source
                                    .read_exact(&mut jumbf[offset..offset + range.size as usize])
                                    .await?;
                                offset += range.size as usize;
                            }
                            if data_only {
                                write_jumbf_with_exclusion_async(pw, &jumbf, exclude_jumbf).await?;
                            } else {
                                pw.set_exclude_mode(exclude_jumbf);
                                write_jumbf_segments_async(pw, &jumbf).await?;
                                pw.set_exclude_mode(false);
                            }
                        }
                    }
                    crate::MetadataUpdate::Remove => {}
                },
                seg if seg.is_type(SegmentKind::ImageData) => {
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.is_type(SegmentKind::ImageData))
                    {
                        let loc = source_seg.location();
                        copy_range_async(source, pw, loc.offset, loc.size).await?;
                    }
                }
                seg if seg.path.as_deref() == Some("EOI") => {
                    pw.write_u8(0xFF).await?;
                    pw.write_u8(EOI).await?;
                }
                _ => {
                    if let Some(source_seg) = structure
                        .segments
                        .iter()
                        .find(|s| s.kind == dest_segment.kind && s.path == dest_segment.path)
                    {
                        let loc = source_seg.location();
                        copy_range_async(source, pw, loc.offset, loc.size).await?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for JpegIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerIO for JpegIO {
    fn container_type() -> ContainerKind {
        ContainerKind::Jpeg
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

    fn detect(header: &[u8]) -> Option<crate::ContainerKind> {
        if header.len() >= 2 && header[0] == 0xFF && header[1] == 0xD8 {
            Some(ContainerKind::Jpeg)
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

        let mut dest_structure = Structure::new(ContainerKind::Jpeg, source_structure.media_type);
        let mut current_offset = 2u64; // Start after SOI marker

        // Header segment is always first — the write loop writes SOI bytes for it.
        dest_structure.add_segment(Segment::new(
            0,
            2,
            SegmentKind::Header,
            Some(marker_label(SOI).to_string()),
        ));

        let mut xmp_written = false;
        let mut jumbf_written = false;

        // Track if file has existing XMP/JUMBF
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
                            )?;
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
                                Some(marker_label(APP11).to_string()),
                            )?;
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
                        Some(marker_label(EOI).to_string()),
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
                                // If data is already in APP11 format, write directly;
                                // otherwise wrap in JPEG XT format
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
                                        Some(marker_label(APP11).to_string()),
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
                                        Some(marker_label(APP11).to_string()),
                                    )?;
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

    fn exclusion_range_for_segment(structure: &Structure, kind: SegmentKind) -> Option<(u64, u64)> {
        let segment = match kind {
            SegmentKind::Jumbf => structure
                .c2pa_jumbf_index()
                .map(|i| &structure.segments()[i]),
            SegmentKind::Xmp => structure.xmp_index().map(|i| &structure.segments()[i]),
            _ => None,
        }?;

        let location = segment.location();
        // JPEG: Only exclude the JUMBF/XMP data portion
        // Headers (marker, length, JPEG XT header) are included in hash
        // Segment offset already points to data start
        Some((location.offset, location.size))
    }
}

async fn find_eoi_async(source: &mut Src) -> Result<u64> {
    const BUFFER_SIZE: usize = 64 * 1024;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut prev_was_ff = false;
    let start_pos = source.stream_position()?;
    let mut total_read = 0u64;
    loop {
        let n = source.read(&mut buffer).await?;
        if n == 0 {
            return Err(Error::InvalidFormat("EOI marker not found".into()));
        }
        if let Some(i) = scan_for_eoi(&buffer[..n], &mut prev_was_ff) {
            return Ok(start_pos + total_read + i as u64 - 1);
        }
        total_read += n as u64;
    }
}

/// Scans `buf` for an EOI marker, returning the index of the EOI byte.
///
/// Kept out of the async fn so the hot loop state lives in registers rather than in the
/// suspended future (which LLVM cannot prove doesn't alias the byte buffer).
#[inline(never)]
fn scan_for_eoi(buf: &[u8], prev_was_ff_state: &mut bool) -> Option<usize> {
    let mut prev_was_ff = *prev_was_ff_state;
    for (i, &byte) in buf.iter().enumerate() {
        if prev_was_ff {
            if byte == EOI {
                return Some(i);
            } else if byte == 0x00 {
                prev_was_ff = false;
            } else {
                prev_was_ff = byte == 0xFF;
            }
        } else if byte == 0xFF {
            prev_was_ff = true;
        }
    }
    *prev_was_ff_state = prev_was_ff;
    None
}

async fn copy_range_async(
    source: &mut Src,
    dest: &mut PDst<'_>,
    offset: u64,
    size: u64,
) -> Result<()> {
    const EVENT_SIZE: usize = 8192;
    source.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; size.min(crate::segment::DEFAULT_CHUNK_SIZE as u64) as usize];
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(buffer.len() as u64) as usize;
        source.read_exact_direct(&mut buffer[..n]).await?;
        dest.write_all_split(&buffer[..n], EVENT_SIZE).await?;
        remaining -= n as u64;
    }
    Ok(())
}

async fn reassemble_extended_xmp_async(
    source: &mut Src,
    ranges: &[ByteRange],
    _guid: &str,
    chunk_offsets: &[u32],
    total_size: u32,
) -> Result<Option<Vec<u8>>> {
    const MAX_XMP_SIZE: u32 = 100 * 1024 * 1024;
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
    if ranges.len() < 2 || chunk_offsets.len() != ranges.len() - 1 {
        if let Some(range) = ranges.first() {
            source.seek(SeekFrom::Start(range.offset))?;
            return Ok(Some(source.read_vec(range.size as usize).await?));
        }
        return Ok(None);
    }
    let mut extended_xmp = vec![0u8; total_size as usize];
    for (range, &chunk_offset) in ranges[1..].iter().zip(chunk_offsets) {
        source.seek(SeekFrom::Start(range.offset))?;
        let start = chunk_offset as usize;
        let end = (start + range.size as usize).min(extended_xmp.len());
        if start >= extended_xmp.len() {
            continue;
        }
        source.read_exact(&mut extended_xmp[start..end]).await?;
    }
    Ok(Some(extended_xmp))
}

async fn write_xmp_segment_async(pw: &mut PDst<'_>, xmp: &[u8]) -> Result<()> {
    const MAIN_XMP_MAX: usize = MAX_MARKER_SIZE - XMP_SIGNATURE.len() - 2;
    if xmp.len() <= MAIN_XMP_MAX {
        let total_size = XMP_SIGNATURE.len() + xmp.len() + 2;
        pw.write_u8(0xFF).await?;
        pw.write_u8(APP1).await?;
        pw.write_u16_be(total_size as u16).await?;
        pw.write_all(XMP_SIGNATURE).await?;
        pw.write_all(xmp).await?;
        return Ok(());
    }
    let guid = format!("{:032x}", md5::compute(xmp));
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
    let main_size = XMP_SIGNATURE.len() + main_xmp.len() + 2;
    pw.write_u8(0xFF).await?;
    pw.write_u8(APP1).await?;
    pw.write_u16_be(main_size as u16).await?;
    pw.write_all(XMP_SIGNATURE).await?;
    pw.write_all(main_xmp.as_bytes()).await?;
    const EXTENDED_HEADER_SIZE: usize = XMP_EXTENDED_SIGNATURE.len() + 32 + 4 + 4;
    const CHUNK_SIZE: usize = MAX_MARKER_SIZE - EXTENDED_HEADER_SIZE - 2;
    let total_size = xmp.len() as u32;
    let mut offset = 0u32;
    while offset < total_size {
        let chunk_size = (total_size - offset).min(CHUNK_SIZE as u32);
        pw.write_u8(0xFF).await?;
        pw.write_u8(APP1).await?;
        pw.write_u16_be((EXTENDED_HEADER_SIZE + chunk_size as usize + 2) as u16)
            .await?;
        pw.write_all(XMP_EXTENDED_SIGNATURE).await?;
        pw.write_all(guid.as_bytes()).await?;
        pw.write_u32_be(total_size).await?;
        pw.write_u32_be(offset).await?;
        let chunk_end = (offset + chunk_size) as usize;
        pw.write_all(&xmp[offset as usize..chunk_end]).await?;
        offset += chunk_size;
    }
    Ok(())
}

async fn write_jumbf_segments_async(pw: &mut PDst<'_>, jumbf: &[u8]) -> Result<()> {
    let complete_app11 = jumbf.len() >= 2 && jumbf[0] == 0xFF && jumbf[1] == APP11;
    if complete_app11 {
        pw.write_all(jumbf).await?;
        return Ok(());
    }
    let already_jpeg_xt = jumbf.len() >= 2 && &jumbf[..2] == b"JP";
    if already_jpeg_xt {
        const MAX_SEGMENT_PAYLOAD: usize = MAX_MARKER_SIZE - 2;
        for chunk in jumbf.chunks(MAX_SEGMENT_PAYLOAD) {
            pw.write_u8(0xFF).await?;
            pw.write_u8(APP11).await?;
            pw.write_u16_be((2 + chunk.len()) as u16).await?;
            pw.write_all(chunk).await?;
        }
        return Ok(());
    }
    const JPEG_XT_HEADER: usize = 8;
    const LBOX_TBOX_SIZE: usize = 8;
    const MAX_DATA_PER_SEGMENT: usize = MAX_MARKER_SIZE - JPEG_XT_HEADER - LBOX_TBOX_SIZE;
    for (seg_num, chunk) in jumbf.chunks(MAX_DATA_PER_SEGMENT).enumerate() {
        pw.write_u8(0xFF).await?;
        pw.write_u8(APP11).await?;
        let continuation_overhead = if seg_num > 0 { LBOX_TBOX_SIZE } else { 0 };
        let seg_size = JPEG_XT_HEADER + continuation_overhead + chunk.len() + 2;
        pw.write_u16_be(seg_size as u16).await?;
        pw.write_all(b"JP").await?;
        pw.write_u16_be(0x0211).await?;
        pw.write_u32_be((seg_num + 1) as u32).await?;
        if seg_num > 0 && jumbf.len() >= 8 {
            pw.write_all(&jumbf[..8]).await?;
        }
        pw.write_all(chunk).await?;
    }
    Ok(())
}

async fn write_jumbf_with_exclusion_async(
    pw: &mut PDst<'_>,
    jumbf: &[u8],
    should_exclude: bool,
) -> Result<()> {
    let complete_app11 = jumbf.len() >= 2 && jumbf[0] == 0xFF && jumbf[1] == APP11;
    if complete_app11 {
        if should_exclude {
            pw.set_exclude_mode(true);
        }
        pw.write_all(jumbf).await?;
        if should_exclude {
            pw.set_exclude_mode(false);
        }
        return Ok(());
    }
    let already_jpeg_xt = jumbf.len() >= 2 && &jumbf[..2] == b"JP";
    if already_jpeg_xt {
        const MAX_SEGMENT_PAYLOAD: usize = MAX_MARKER_SIZE - 2;
        for chunk in jumbf.chunks(MAX_SEGMENT_PAYLOAD) {
            pw.write_u8(0xFF).await?;
            pw.write_u8(APP11).await?;
            pw.write_u16_be((2 + chunk.len()) as u16).await?;
            if should_exclude {
                pw.set_exclude_mode(true);
            }
            pw.write_all(chunk).await?;
            if should_exclude {
                pw.set_exclude_mode(false);
            }
        }
        return Ok(());
    }
    const JPEG_XT_HEADER: usize = 8;
    const LBOX_TBOX_SIZE: usize = 8;
    const MAX_DATA_PER_SEGMENT: usize = MAX_MARKER_SIZE - JPEG_XT_HEADER - LBOX_TBOX_SIZE;
    for (seg_num, chunk) in jumbf.chunks(MAX_DATA_PER_SEGMENT).enumerate() {
        pw.write_u8(0xFF).await?;
        pw.write_u8(APP11).await?;
        let continuation_overhead = if seg_num > 0 { LBOX_TBOX_SIZE } else { 0 };
        let seg_size = JPEG_XT_HEADER + continuation_overhead + chunk.len() + 2;
        pw.write_u16_be(seg_size as u16).await?;
        pw.write_all(b"JP").await?;
        pw.write_u16_be(0x0211).await?;
        pw.write_u32_be((seg_num + 1) as u32).await?;
        if seg_num > 0 && jumbf.len() >= 8 {
            pw.write_all(&jumbf[..8]).await?;
        }
        if should_exclude {
            pw.set_exclude_mode(true);
        }
        pw.write_all(chunk).await?;
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

        assert_eq!(structure.container, ContainerKind::Jpeg);
        assert_eq!(structure.total_size, 4);
        assert_eq!(structure.segments.len(), 2); // Header + EOI
    }
}
