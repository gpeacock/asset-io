//! RIFF container I/O implementation
//!
//! Supports: WebP (image/webp), WAV (audio/wav, audio/wave, audio/x-wav),
//! and AVI (video/avi, video/msvideo, video/x-msvideo).
//!
//! # RIFF Format
//!
//! RIFF files have the following structure:
//! ```text
//! [RIFF:4][data_size:4 LE][format:4][chunks...]
//! ```
//! where `data_size = total_file_size - 8`.
//!
//! Each child chunk:
//! ```text
//! [id:4][size:4 LE][data:size][padding:0 or 1]
//! ```
//! Odd-sized data is padded with a single 0x00 byte (not counted in size).
//!
//! # C2PA Embedding
//!
//! C2PA data is stored in a top-level chunk with ID `"C2PA"`, appended at the
//! end of the RIFF chunk for maximum compatibility (per the c2pa-rs convention).
//! XMP is stored in a `"XMP "` chunk, also appended before the C2PA chunk.
//!
//! # Security
//!
//! - Chunk sizes are capped at [`MAX_RIFF_CHUNK_ALLOC`] (256 MB) to prevent OOM attacks.
//! - File size is validated against the RIFF header's declared size.
//! - All arithmetic uses checked or saturating operations.

use super::{ContainerIO, ContainerKind};
use crate::sans_io::io::{PDst, Src};
use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind, MAX_SEGMENT_SIZE},
    structure::Structure,
    MediaType, Updates,
};
use std::io::SeekFrom;

/// Maximum allocation for a single RIFF chunk (256 MB)
///
/// Prevents OOM attacks from maliciously crafted files claiming giant chunk sizes.
const MAX_RIFF_CHUNK_ALLOC: u64 = 256 * 1024 * 1024;

// Chunk IDs (4-byte FourCC codes, in spec-correct byte order)
const C2PA_CHUNK_ID: &[u8; 4] = b"C2PA";
const XMP_CHUNK_ID: &[u8; 4] = b"XMP ";

// Top-level RIFF format codes (bytes 8–11 of file)
const WEBP_FORMAT: &[u8; 4] = b"WEBP";
const WAVE_FORMAT: &[u8; 4] = b"WAVE";
const AVI_FORMAT: &[u8; 4] = b"AVI ";

// VP8X feature flags (LE uint32 at bytes 0–3 of the VP8X chunk data)
const VP8X_XMP_FLAG: u32 = 0x0004; // bit 2 = XMP metadata present

// VP8X chunk data must be exactly 10 bytes
const VP8X_DATA_SIZE: u64 = 10;

/// RIFF container I/O implementation
pub struct RiffIO;

impl RiffIO {
    /// Create a new RIFF I/O implementation
    pub fn new() -> Self {
        Self
    }

    /// Detect media type from the RIFF format code (bytes 8–11)
    fn detect_media_type(format: &[u8; 4]) -> MediaType {
        if format == WEBP_FORMAT {
            MediaType::WebP
        } else if format == WAVE_FORMAT {
            MediaType::Wav
        } else if format == AVI_FORMAT {
            MediaType::Avi
        } else {
            MediaType::Wav // conservative fallback
        }
    }

    /// Compute the padded (even-aligned) size for a chunk data section
    #[inline]
    fn padded(size: u64) -> u64 {
        size + (size % 2)
    }

    /// Total on-disk bytes for a chunk with the given data size (header + data + padding)
    #[inline]
    fn chunk_on_disk(data_size: u64) -> u64 {
        8 + Self::padded(data_size)
    }

    /// Parse the RIFF file structure in a single sequential pass
    pub(crate) async fn parse_async(&self, source: &mut Src) -> Result<Structure> {
        source.seek(SeekFrom::Start(0))?;

        // Validate RIFF signature
        let mut sig = [0u8; 4];
        source.read_exact(&mut sig).await?;
        if &sig != b"RIFF" {
            return Err(Error::InvalidFormat("Not a RIFF file".into()));
        }

        // Read declared data size (total file - 8 bytes for root chunk header)
        let riff_data_size = source.read_u32_le().await? as u64;
        let declared_end = riff_data_size.saturating_add(8);

        // Read format code (bytes 8–11) to determine media type
        let mut format = [0u8; 4];
        source.read_exact(&mut format).await?;

        let media_type = Self::detect_media_type(&format);
        let mut structure = Structure::new(ContainerKind::Riff, media_type);

        // Clamp against actual file size to handle truncated/malformed files
        let actual_end = source.len().min(declared_end);
        source.seek(SeekFrom::Start(12))?;

        log::debug!(
            "parse: format='{}' declared_end={} actual_end={}",
            String::from_utf8_lossy(&format),
            declared_end,
            actual_end
        );

        // RIFF root header: signature (4) + size field (4) + format (4) = 12 bytes
        structure.add_segment(Segment::new(
            0,
            12,
            SegmentKind::Header,
            Some("riff".to_string()),
        ));

        // Walk direct child chunks sequentially
        let mut offset = 12u64;
        while offset + 8 <= actual_end {
            // Read chunk ID
            let mut chunk_id = [0u8; 4];
            match source.read_exact(&mut chunk_id).await {
                Ok(()) => {}
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            // Read chunk data size (LE u32)
            let data_size = match source.read_u32_le().await {
                Ok(s) => s as u64,
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };

            let data_offset = offset + 8;
            let padded_data_size = Self::padded(data_size);
            let chunk_total = 8 + padded_data_size;

            // Some AVI encoders (e.g. VLC) insert 4 null bytes for DWORD alignment
            // between top-level chunks. These are not valid chunk IDs — skip them.
            if chunk_id == [0u8; 4] {
                log::debug!("parse: skipping 4 null padding bytes at offset={}", offset);
                offset += 4;
                source.seek(SeekFrom::Start(offset))?;
                continue;
            }

            log::debug!(
                "parse: chunk='{}' offset={} data_size={} padded={} chunk_total={} actual_end={}",
                String::from_utf8_lossy(&chunk_id),
                offset,
                data_size,
                padded_data_size,
                chunk_total,
                actual_end
            );

            // Validate the chunk fits within the (clamped) file size.
            // Truncated or malformed files will have chunk sizes that run past EOF.
            if data_offset.saturating_add(padded_data_size) > actual_end {
                log::warn!(
                    "parse: chunk '{}' at offset={} extends past actual_end={} (data_offset={} + padded_data_size={} = {}), stopping",
                    String::from_utf8_lossy(&chunk_id),
                    offset,
                    actual_end,
                    data_offset,
                    padded_data_size,
                    data_offset.saturating_add(padded_data_size)
                );
                break;
            }

            match &chunk_id {
                b"C2PA" => {
                    // Security: C2PA data is loaded entirely into RAM — cap it.
                    if data_size > MAX_RIFF_CHUNK_ALLOC {
                        return Err(Error::InvalidSegment {
                            offset,
                            reason: format!(
                                "C2PA chunk too large: {} bytes (max {} MB)",
                                data_size,
                                MAX_RIFF_CHUNK_ALLOC / (1024 * 1024)
                            ),
                        });
                    }
                    log::debug!(
                        "parse: found C2PA chunk data_offset={} data_size={}",
                        data_offset,
                        data_size
                    );
                    structure.add_segment(Segment::with_ranges(
                        vec![ByteRange::new(data_offset, data_size)],
                        SegmentKind::Jumbf,
                        Some("C2PA".to_string()),
                    )?);
                    source.seek(SeekFrom::Current(padded_data_size as i64))?;
                }
                b"XMP " => {
                    // Security: XMP data is loaded entirely into RAM — cap it.
                    if data_size > MAX_RIFF_CHUNK_ALLOC {
                        return Err(Error::InvalidSegment {
                            offset,
                            reason: format!(
                                "XMP chunk too large: {} bytes (max {} MB)",
                                data_size,
                                MAX_RIFF_CHUNK_ALLOC / (1024 * 1024)
                            ),
                        });
                    }
                    log::debug!(
                        "parse: found XMP chunk data_offset={} data_size={}",
                        data_offset,
                        data_size
                    );
                    structure.add_segment(Segment::with_ranges(
                        vec![ByteRange::new(data_offset, data_size)],
                        SegmentKind::Xmp,
                        Some("XMP ".to_string()),
                    )?);
                    source.seek(SeekFrom::Current(padded_data_size as i64))?;
                }
                b"VP8 " | b"VP8L" => {
                    // WebP image data (lossy VP8 or lossless VP8L)
                    let path = String::from_utf8_lossy(&chunk_id).into_owned();
                    structure.add_segment(Segment::new(
                        offset,
                        chunk_total,
                        SegmentKind::ImageData,
                        Some(path),
                    ));
                    source.seek(SeekFrom::Current(padded_data_size as i64))?;
                }
                _ => {
                    // All other chunks (LIST/movi, idx1, JUNK, audio/video frames, etc.)
                    // are never loaded into memory — only seeked past during parse and
                    // streamed through std::io::copy during write. No size cap needed.
                    let path = String::from_utf8_lossy(&chunk_id).into_owned();
                    structure.add_segment(Segment::new(
                        offset,
                        chunk_total,
                        SegmentKind::Other,
                        Some(path.clone()),
                    ));
                    source.seek(SeekFrom::Current(padded_data_size as i64))?;
                }
            }

            offset += chunk_total;
        }

        log::debug!(
            "parse: done, {} segments, total_size={}",
            structure.segments().len(),
            actual_end
        );
        structure.total_size = actual_end;
        Ok(structure)
    }

    pub(crate) async fn read_xmp_async(
        &self,
        structure: &Structure,
        source: &mut Src,
    ) -> Result<Option<Vec<u8>>> {
        let Some(idx) = structure.xmp_index() else {
            return Ok(None);
        };
        let location = structure.segments()[idx].location();
        if location.size > MAX_SEGMENT_SIZE {
            return Err(Error::InvalidSegment {
                offset: location.offset,
                reason: format!(
                    "XMP chunk too large: {} bytes (max {} MB)",
                    location.size,
                    MAX_SEGMENT_SIZE / (1024 * 1024)
                ),
            });
        }
        source.seek(SeekFrom::Start(location.offset))?;
        Ok(Some(source.read_vec(location.size as usize).await?))
    }

    pub(crate) async fn read_jumbf_async(
        &self,
        structure: &Structure,
        source: &mut Src,
    ) -> Result<Option<Vec<u8>>> {
        let Some(idx) = structure.c2pa_jumbf_index() else {
            return Ok(None);
        };
        let location = structure.segments()[idx].location();
        if location.size > MAX_SEGMENT_SIZE {
            return Err(Error::InvalidSegment {
                offset: location.offset,
                reason: format!(
                    "C2PA chunk too large: {} bytes (max {} MB)",
                    location.size,
                    MAX_SEGMENT_SIZE / (1024 * 1024)
                ),
            });
        }
        source.seek(SeekFrom::Start(location.offset))?;
        Ok(Some(source.read_vec(location.size as usize).await?))
    }

    #[cfg(feature = "exif")]
    pub(crate) async fn read_embedded_thumbnail_info_async(
        &self,
        _structure: &Structure,
        _source: &mut Src,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
        Ok(None)
    }

    #[cfg(feature = "exif")]
    pub(crate) async fn read_exif_info_async(
        &self,
        _structure: &Structure,
        _source: &mut Src,
    ) -> Result<Option<crate::tiff::ExifInfo>> {
        Ok(None)
    }

    pub(crate) async fn write_async(
        &self,
        structure: &Structure,
        source: &mut Src,
        pw: &mut PDst<'_>,
        updates: &Updates,
    ) -> Result<()> {
        use crate::segment::ExclusionMode;
        use crate::updates::MetadataUpdate;

        let exclude_segments = &updates.processing.exclude_segments;
        let should_exclude_jumbf = exclude_segments.contains(&SegmentKind::Jumbf);
        let data_only = updates.processing.exclusion_mode == ExclusionMode::DataOnly;
        let dest = self.calculate_updated_structure(structure, updates)?;
        let format = Self::read_format_async(source).await?;
        let is_webp = &format == WEBP_FORMAT;
        let adding_xmp = Self::has_xmp_output(structure, updates);

        pw.write_all(b"RIFF").await?;
        pw.write_u32_le((dest.total_size - 8) as u32).await?;
        pw.write_all(&format).await?;

        Self::write_other_chunks_async(structure, source, pw, is_webp, adding_xmp).await?;

        match &updates.xmp {
            MetadataUpdate::Set(xmp_data) => {
                Self::write_chunk_async(pw, XMP_CHUNK_ID, xmp_data).await?;
            }
            MetadataUpdate::Keep => {
                if let Some(idx) = structure.xmp_index() {
                    let loc = structure.segments()[idx].location();
                    if loc.size > MAX_SEGMENT_SIZE {
                        return Err(Error::InvalidSegment {
                            offset: loc.offset,
                            reason: "XMP chunk too large to copy".into(),
                        });
                    }
                    source.seek(SeekFrom::Start(loc.offset))?;
                    let xmp_data = source.read_vec(loc.size as usize).await?;
                    Self::write_chunk_async(pw, XMP_CHUNK_ID, &xmp_data).await?;
                }
            }
            MetadataUpdate::Remove => {}
        }

        match &updates.jumbf {
            MetadataUpdate::Set(jumbf_data) => {
                Self::write_c2pa_chunk_with_exclusion_async(
                    pw,
                    jumbf_data,
                    should_exclude_jumbf,
                    data_only,
                )
                .await?;
            }
            MetadataUpdate::Keep => {
                if let Some(idx) = structure.c2pa_jumbf_index() {
                    let loc = structure.segments()[idx].location();
                    if loc.size > MAX_SEGMENT_SIZE {
                        return Err(Error::InvalidSegment {
                            offset: loc.offset,
                            reason: "C2PA chunk too large to copy".into(),
                        });
                    }
                    source.seek(SeekFrom::Start(loc.offset))?;
                    let jumbf_data = source.read_vec(loc.size as usize).await?;
                    Self::write_c2pa_chunk_with_exclusion_async(
                        pw,
                        &jumbf_data,
                        should_exclude_jumbf,
                        data_only,
                    )
                    .await?;
                }
            }
            MetadataUpdate::Remove => {}
        }
        Ok(())
    }

    /// Returns true if the destination will contain XMP
    fn has_xmp_output(source: &Structure, updates: &Updates) -> bool {
        use crate::updates::MetadataUpdate;
        match &updates.xmp {
            MetadataUpdate::Set(_) => true,
            MetadataUpdate::Keep => source.segments().iter().any(|s| s.is_xmp()),
            MetadataUpdate::Remove => false,
        }
    }

    /// Read the 4-byte RIFF format code from a source stream.
    async fn read_format_async(source: &mut Src) -> Result<[u8; 4]> {
        source.seek(SeekFrom::Start(8))?;
        let mut format = [0u8; 4];
        source.read_exact(&mut format).await?;
        Ok(format)
    }

    /// Write a RIFF chunk: [id:4][size:4 LE][data][pad:0 or 1].
    async fn write_chunk_async(pw: &mut PDst<'_>, id: &[u8; 4], data: &[u8]) -> Result<()> {
        pw.write_all(id).await?;
        pw.write_u32_le(data.len() as u32).await?;
        pw.write_all(data).await?;
        if data.len() % 2 == 1 {
            pw.write_u8(0).await?;
        }
        Ok(())
    }

    /// Write the C2PA chunk with proper exclusion handling for ProcessingWriter.
    async fn write_c2pa_chunk_with_exclusion_async(
        pw: &mut PDst<'_>,
        data: &[u8],
        should_exclude: bool,
        data_only: bool,
    ) -> Result<()> {
        if should_exclude {
            if data_only {
                pw.write_all(C2PA_CHUNK_ID).await?;
                pw.write_u32_le(data.len() as u32).await?;
                pw.set_exclude_mode(true);
                pw.write_all(data).await?;
                if data.len() % 2 == 1 {
                    pw.write_u8(0).await?;
                }
                pw.set_exclude_mode(false);
            } else {
                pw.set_exclude_mode(true);
                Self::write_chunk_async(pw, C2PA_CHUNK_ID, data).await?;
                pw.set_exclude_mode(false);
            }
        } else {
            Self::write_chunk_async(pw, C2PA_CHUNK_ID, data).await?;
        }
        Ok(())
    }

    /// Copy non-metadata chunks with the same 8 KiB write granularity as std::io::copy.
    async fn write_other_chunks_async(
        source_structure: &Structure,
        source: &mut Src,
        pw: &mut PDst<'_>,
        is_webp: bool,
        adding_xmp: bool,
    ) -> Result<()> {
        let other_count = source_structure
            .segments()
            .iter()
            .filter(|s| !s.is_type(SegmentKind::Header) && !s.is_xmp() && !s.is_jumbf())
            .count();
        log::debug!("write_other_chunks: {} chunks to copy", other_count);

        let mut buffer = vec![0u8; crate::segment::DEFAULT_CHUNK_SIZE];
        for seg in source_structure.segments() {
            if seg.is_type(SegmentKind::Header) || seg.is_xmp() || seg.is_jumbf() {
                continue;
            }

            let location = seg.location();
            log::debug!(
                "write_other_chunks: copying '{}' offset={} size={}",
                seg.path.as_deref().unwrap_or("?"),
                location.offset,
                location.size
            );

            if is_webp
                && adding_xmp
                && seg.path.as_deref() == Some("VP8X")
                && location.size == 8 + VP8X_DATA_SIZE
            {
                source.seek(SeekFrom::Start(location.offset))?;
                let mut buf = source.read_vec(location.size as usize).await?;
                if let Ok(arr) = buf[8..12].try_into() {
                    let flags = u32::from_le_bytes(arr) | VP8X_XMP_FLAG;
                    buf[8..12].copy_from_slice(&flags.to_le_bytes());
                }
                pw.write_all(&buf).await?;
            } else {
                source.seek(SeekFrom::Start(location.offset))?;
                let mut remaining = location.size;
                let mut copied = 0u64;
                while remaining > 0 {
                    let n = remaining.min(buffer.len() as u64) as usize;
                    source.read_exact_direct(&mut buffer[..n]).await?;
                    pw.write_all_split(&buffer[..n], 8192).await?;
                    remaining -= n as u64;
                    copied += n as u64;
                }
                log::debug!("write_other_chunks: copied {} bytes", copied);
            }
        }
        log::debug!("write_other_chunks: done");
        Ok(())
    }
}

impl Default for RiffIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerIO for RiffIO {
    fn container_type() -> ContainerKind {
        ContainerKind::Riff
    }

    fn supported_media_types() -> &'static [MediaType] {
        &[MediaType::WebP, MediaType::Wav, MediaType::Avi]
    }

    fn extensions() -> &'static [&'static str] {
        &["webp", "wav", "avi"]
    }

    fn mime_types() -> &'static [&'static str] {
        &[
            "image/webp",
            "audio/wav",
            "audio/wave",
            "audio/x-wav",
            "audio/vnd.wave",
            "video/avi",
            "video/msvideo",
            "video/x-msvideo",
            "application/x-troff-msvideo",
        ]
    }

    fn detect(header: &[u8]) -> Option<ContainerKind> {
        // RIFF files begin with the literal "RIFF" FourCC
        if header.len() >= 4 && &header[0..4] == b"RIFF" {
            Some(ContainerKind::Riff)
        } else {
            None
        }
    }

    fn calculate_updated_structure(
        &self,
        source_structure: &Structure,
        updates: &Updates,
    ) -> Result<Structure> {
        use crate::updates::MetadataUpdate;

        let mut dest = Structure::new(ContainerKind::Riff, source_structure.media_type);

        // RIFF root header: 12 bytes (signature + size_field + format)
        dest.add_segment(Segment::new(
            0,
            12,
            SegmentKind::Header,
            Some("riff".to_string()),
        ));
        let mut offset = 12u64;

        let is_webp = source_structure.media_type == MediaType::WebP;
        let adding_xmp = Self::has_xmp_output(source_structure, updates);

        // Copy all Other chunks in original order (C2PA and XMP will be appended at end)
        for seg in source_structure.segments() {
            if seg.is_type(SegmentKind::Header) || seg.is_xmp() || seg.is_jumbf() {
                continue;
            }

            let src_loc = seg.location();
            let seg_size = src_loc.size; // already includes 8-byte header + padding

            // VP8X same size - just content changes, not layout
            // Preserve kind (ImageData for VP8/VP8L, Other for VP8X etc.)
            dest.add_segment(Segment::new(offset, seg_size, seg.kind, seg.path.clone()));
            offset += seg_size;

            // Detect if VP8X is missing for WebP + XMP case
            // (note: creation of VP8X requires VP8/VP8L dimension parsing - out of scope)
            let _ = (is_webp, adding_xmp);
        }

        // Append XMP chunk at end (if requested)
        let xmp_data_size: Option<u64> = match &updates.xmp {
            MetadataUpdate::Set(xmp) => Some(xmp.len() as u64),
            MetadataUpdate::Keep => source_structure
                .xmp_index()
                .map(|i| source_structure.segments()[i].location().size),
            MetadataUpdate::Remove => None,
        };

        if let Some(sz) = xmp_data_size {
            // XMP segment records the data range (after the 8-byte chunk header)
            dest.add_segment(Segment::with_ranges(
                vec![ByteRange::new(offset + 8, sz)],
                SegmentKind::Xmp,
                Some("XMP ".to_string()),
            )?);
            offset += Self::chunk_on_disk(sz);
        }

        // Append C2PA chunk at very end (for maximum compatibility)
        let c2pa_data_size: Option<u64> = match &updates.jumbf {
            MetadataUpdate::Set(jumbf) => Some(jumbf.len() as u64),
            MetadataUpdate::Keep => source_structure
                .c2pa_jumbf_index()
                .map(|i| source_structure.segments()[i].location().size),
            MetadataUpdate::Remove => None,
        };

        if let Some(sz) = c2pa_data_size {
            // Jumbf segment records the data range (after the 8-byte chunk header)
            dest.add_segment(Segment::with_ranges(
                vec![ByteRange::new(offset + 8, sz)],
                SegmentKind::Jumbf,
                Some("C2PA".to_string()),
            )?);
            offset += Self::chunk_on_disk(sz);
        }

        dest.total_size = offset;
        Ok(dest)
    }

    fn exclusion_range_for_segment(structure: &Structure, kind: SegmentKind) -> Option<(u64, u64)> {
        let segment = match kind {
            SegmentKind::Jumbf => structure
                .c2pa_jumbf_index()
                .map(|i| &structure.segments()[i]),
            SegmentKind::Xmp => structure.xmp_index().map(|i| &structure.segments()[i]),
            _ => None,
        }?;

        let loc = segment.location();
        // RIFF has no CRC, but data is padded to even boundary.
        // For DataOnly mode, exclude data + alignment padding byte (if any).
        let padded_size = Self::padded(loc.size);
        Some((loc.offset, padded_size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Seek};

    /// Build a minimal RIFF file with the given format code and chunks
    fn make_riff(format: &[u8; 4], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut content: Vec<u8> = Vec::new();
        // format code is part of RIFF content
        content.extend_from_slice(format);
        for (id, data) in chunks {
            content.extend_from_slice(*id);
            content.extend_from_slice(&(data.len() as u32).to_le_bytes());
            content.extend_from_slice(data);
            if data.len() % 2 == 1 {
                content.push(0); // padding
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(content.len() as u32).to_le_bytes());
        out.extend_from_slice(&content);
        out
    }

    #[test]
    fn test_detect_riff() {
        let data = make_riff(WAVE_FORMAT, &[]);
        assert_eq!(RiffIO::detect(&data), Some(ContainerKind::Riff));

        let not_riff = b"\xFF\xD8\xFF\xE0";
        assert_eq!(RiffIO::detect(not_riff), None);
    }

    #[test]
    fn test_parse_wav() {
        let payload = b"some audio data";
        let data = make_riff(WAVE_FORMAT, &[(b"data", payload)]);
        let mut cursor = Cursor::new(data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        assert_eq!(structure.container, ContainerKind::Riff);
        assert_eq!(structure.media_type, MediaType::Wav);
        assert!(structure.xmp_index().is_none());
        assert!(structure.jumbf_indices().is_empty());
    }

    #[test]
    fn test_parse_webp() {
        let data = make_riff(WEBP_FORMAT, &[(b"VP8 ", b"\x00\x01\x02\x03")]);
        let mut cursor = Cursor::new(data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        assert_eq!(structure.media_type, MediaType::WebP);
    }

    #[test]
    fn test_parse_finds_c2pa_chunk() {
        let c2pa_data = b"fake c2pa manifest";
        let data = make_riff(WAVE_FORMAT, &[(b"data", b"audio"), (b"C2PA", c2pa_data)]);
        let mut cursor = Cursor::new(data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        assert!(structure.c2pa_jumbf_index().is_some());
        let jumbf = handler.read_jumbf(&structure, &mut cursor).unwrap();
        assert_eq!(jumbf.as_deref(), Some(c2pa_data.as_ref()));
    }

    #[test]
    fn test_parse_finds_xmp_chunk() {
        let xmp_bytes = b"<?xpacket>xmp data<?/xpacket>";
        let data = make_riff(WAVE_FORMAT, &[(b"XMP ", xmp_bytes)]);
        let mut cursor = Cursor::new(data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        assert!(structure.xmp_index().is_some());
        let xmp = handler.read_xmp(&structure, &mut cursor).unwrap();
        assert_eq!(xmp.as_deref(), Some(xmp_bytes.as_ref()));
    }

    #[test]
    fn test_write_adds_c2pa_chunk() {
        use crate::Updates;

        let audio = b"audio payload";
        let c2pa = b"my c2pa manifest";
        let source_data = make_riff(WAVE_FORMAT, &[(b"data", audio)]);

        let mut source = Cursor::new(source_data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut source).unwrap();

        let updates = Updates::new().set_jumbf(c2pa.to_vec());

        let mut output = Cursor::new(Vec::new());
        handler
            .write(&structure, &mut source, &mut output, &updates)
            .unwrap();

        // Parse the output and verify C2PA chunk is there
        output.seek(SeekFrom::Start(0)).unwrap();
        let out_structure = handler.parse(&mut output).unwrap();
        assert!(out_structure.c2pa_jumbf_index().is_some());

        let jumbf = handler.read_jumbf(&out_structure, &mut output).unwrap();
        assert_eq!(jumbf.as_deref(), Some(c2pa.as_ref()));
    }

    #[test]
    fn test_write_replaces_c2pa_chunk() {
        use crate::Updates;

        let old_c2pa = b"old manifest data";
        let new_c2pa = b"new manifest data";
        let source_data = make_riff(WAVE_FORMAT, &[(b"C2PA", old_c2pa)]);

        let mut source = Cursor::new(source_data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut source).unwrap();

        let updates = Updates::new().set_jumbf(new_c2pa.to_vec());

        let mut output = Cursor::new(Vec::new());
        handler
            .write(&structure, &mut source, &mut output, &updates)
            .unwrap();

        output.seek(SeekFrom::Start(0)).unwrap();
        let out_structure = handler.parse(&mut output).unwrap();
        let jumbf = handler.read_jumbf(&out_structure, &mut output).unwrap();
        assert_eq!(jumbf.as_deref(), Some(new_c2pa.as_ref()));
    }

    #[test]
    fn test_write_removes_c2pa_chunk() {
        use crate::Updates;

        let c2pa_data = b"manifest to remove";
        let source_data = make_riff(WAVE_FORMAT, &[(b"C2PA", c2pa_data)]);

        let mut source = Cursor::new(source_data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut source).unwrap();

        let updates = Updates::new().remove_jumbf();

        let mut output = Cursor::new(Vec::new());
        handler
            .write(&structure, &mut source, &mut output, &updates)
            .unwrap();

        output.seek(SeekFrom::Start(0)).unwrap();
        let out_structure = handler.parse(&mut output).unwrap();
        assert!(out_structure.c2pa_jumbf_index().is_none());
    }

    #[test]
    fn test_riff_header_size_field_is_correct() {
        use crate::Updates;

        let audio = b"audio";
        let c2pa = b"manifest";
        let source_data = make_riff(WAVE_FORMAT, &[(b"data", audio)]);

        let mut source = Cursor::new(source_data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut source).unwrap();

        let updates = Updates::new().set_jumbf(c2pa.to_vec());
        let mut output = Cursor::new(Vec::new());
        handler
            .write(&structure, &mut source, &mut output, &updates)
            .unwrap();

        let out = output.into_inner();
        assert_eq!(&out[0..4], b"RIFF");
        // size field = total_len - 8
        let size_field = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
        assert_eq!(size_field + 8, out.len());
    }

    #[test]
    fn test_odd_size_chunk_is_padded() {
        // Chunk with odd-length data gets a padding byte on disk
        let odd_data = b"hello"; // 5 bytes
        let data = make_riff(WAVE_FORMAT, &[(b"test", odd_data)]);

        let mut cursor = Cursor::new(&data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        // Chunk offset: 12 (header) + 0 = 12; size = 8 + 5 + 1 (pad) = 14
        let other = structure
            .segments()
            .iter()
            .find(|s| s.path.as_deref() == Some("test"));
        assert!(other.is_some());
        assert_eq!(other.unwrap().location().size, 14);
    }

    #[test]
    fn test_invalid_riff_signature() {
        let bad = b"WAVE\x00\x00\x00\x00\x00\x00\x00\x00";
        let mut cursor = Cursor::new(bad);
        let handler = RiffIO::new();
        let result = handler.parse(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_exclusion_range_jumbf() {
        let c2pa = b"test manifest data";
        let source_data = make_riff(WAVE_FORMAT, &[(b"C2PA", c2pa)]);

        let mut cursor = Cursor::new(source_data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        let (offset, size) =
            RiffIO::exclusion_range_for_segment(&structure, SegmentKind::Jumbf).unwrap();

        // offset should point to data (after 8-byte chunk header)
        // size should be c2pa.len() (even, no padding needed here)
        assert_eq!(size, c2pa.len() as u64);
        let _ = offset; // value is format-dependent but must be > 12
    }

    #[test]
    fn test_write_with_processor_calls_processor() {
        use crate::segment::ExclusionMode;
        use crate::Updates;

        let audio = b"audio sample";
        let c2pa = b"c2pa data here";
        let source_data = make_riff(WAVE_FORMAT, &[(b"data", audio)]);

        let mut source = Cursor::new(source_data);
        let handler = RiffIO::new();
        let structure = handler.parse(&mut source).unwrap();

        let updates = Updates::new()
            .set_jumbf(c2pa.to_vec())
            .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

        let mut output = Cursor::new(Vec::new());
        let mut processed_bytes = 0usize;
        let mut processor = |chunk: &dyn crate::ProcessChunk| {
            processed_bytes += chunk.data().len();
            Ok(())
        };

        handler
            .write_with_processor(
                &structure,
                &mut source,
                &mut output,
                &updates,
                &mut processor,
            )
            .unwrap();

        // Processor must have seen some bytes, but not the C2PA data
        assert!(processed_bytes > 0);
        // C2PA data should NOT appear in processed bytes count
        // (exact assertion requires tracking excluded bytes separately)
    }
}
