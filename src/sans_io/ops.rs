//! Public constructors for sans-IO machines.

use super::io::{Dst, PDst, Src};
use super::{Chunk, Machine};
use crate::containers::get_handler;
use crate::segment::DEFAULT_CHUNK_SIZE;
use crate::{Structure, Updates};

/// Byte ranges `(offset, size)` that are processed when hashing `structure` with the
/// exclusions configured in `updates.processing`.
pub(crate) fn processing_ranges(structure: &Structure, updates: &Updates) -> Vec<(u64, u64)> {
    let exclude_segments = &updates.processing.exclude_segments;

    let excluded_indices: Vec<usize> = exclude_segments
        .iter()
        .filter_map(|kind| structure.segments.iter().position(|s| s.is_type(*kind)))
        .collect();

    let mut ranges = Vec::new();
    let mut last_end = 0u64;

    // For BMFF, always exclude the ftyp box (per BmffHash spec).
    #[cfg(feature = "bmff")]
    if structure.container == crate::ContainerKind::Bmff {
        last_end = structure
            .segments
            .iter()
            .flat_map(|s| s.ranges.iter())
            .map(|r| r.offset)
            .min()
            .unwrap_or(24);
    }

    for (idx, segment) in structure.segments.iter().enumerate() {
        if !excluded_indices.contains(&idx) {
            continue;
        }
        let (exclude_start, exclude_size) = exclude_segments
            .iter()
            .find(|k| segment.is_type(**k))
            .and_then(|kind| structure.exclusion_range_for_segment(*kind))
            .unwrap_or_else(|| {
                let loc = segment.location();
                (loc.offset, loc.size)
            });

        if exclude_start > last_end {
            ranges.push((last_end, exclude_start - last_end));
        }
        last_end = exclude_start + exclude_size;
    }

    if last_end < structure.total_size {
        ranges.push((last_end, structure.total_size - last_end));
    }
    ranges
}

/// Stream through `structure`'s source, emitting [`Chunk::Data`] events for every byte that
/// is not excluded by `updates.processing` (e.g. to hash an existing asset).
///
/// Only issues [`super::Step::Read`] and [`super::Step::Process`]. Reads are monotonically
/// increasing, so forward-only sources work. [`super::Progress::total`] is the number of
/// bytes that will be processed.
pub fn read_process<'a>(structure: &'a Structure, updates: &'a Updates) -> Machine<'a, ()> {
    Machine::new(move |io| async move {
        let ranges = processing_ranges(structure, updates);
        io.set_total(Some(ranges.iter().map(|r| r.1).sum()));
        let buffer_size = updates
            .processing
            .effective_chunk_size()
            .min(DEFAULT_CHUNK_SIZE);
        let mut src = Src::new(io.clone(), structure.total_size);
        let mut buf =
            vec![0u8; buffer_size.min(ranges.iter().map(|r| r.1).max().unwrap_or(0) as usize,)];
        for (offset, size) in ranges {
            src.seek(std::io::SeekFrom::Start(offset))?;
            let mut remaining = size;
            while remaining > 0 {
                let n = remaining.min(buffer_size as u64) as usize;
                src.read_exact_direct(&mut buf[..n]).await?;
                io.process(Chunk::Data(&buf[..n])).await?;
                remaining -= n as u64;
            }
        }
        Ok(())
    })
}

/// Detect the container format and parse the structure of a source that is `len` bytes long.
///
/// Only issues [`super::Step::Read`]. The resulting [`Structure`] is the input to every other
/// machine in this module.
pub fn parse(len: u64) -> Machine<'static, Structure> {
    Machine::new(move |io| async move {
        let mut src = Src::new(io, len);
        let container = crate::containers::detect_container_async(&mut src).await?;
        src.seek(std::io::SeekFrom::Start(0))?;
        get_handler(container)?.parse_async(&mut src).await
    })
}

/// Read the XMP packet described by `structure` (assembling extended XMP where applicable).
///
/// Only issues [`super::Step::Read`].
pub fn read_xmp(structure: &Structure) -> Machine<'_, Option<Vec<u8>>> {
    Machine::new(move |io| async move {
        let mut src = Src::new(io, structure.total_size);
        get_handler(structure.container)?
            .read_xmp_async(structure, &mut src)
            .await
    })
}

/// Read and assemble the JUMBF (C2PA) data described by `structure`.
///
/// Only issues [`super::Step::Read`].
pub fn read_jumbf(structure: &Structure) -> Machine<'_, Option<Vec<u8>>> {
    Machine::new(move |io| async move {
        let mut src = Src::new(io, structure.total_size);
        get_handler(structure.container)?
            .read_jumbf_async(structure, &mut src)
            .await
    })
}

/// Extract the embedded (EXIF) thumbnail, if any. Always `None` without the `exif` feature.
///
/// Only issues [`super::Step::Read`].
pub fn read_embedded_thumbnail(structure: &Structure) -> Machine<'_, Option<crate::Thumbnail>> {
    Machine::new(move |io| async move {
        #[cfg(feature = "exif")]
        {
            let mut src = Src::new(io, structure.total_size);
            let info = get_handler(structure.container)?
                .read_embedded_thumbnail_info_async(structure, &mut src)
                .await?;
            match info {
                Some(info) => {
                    src.seek(std::io::SeekFrom::Start(info.offset))?;
                    let data = src.read_vec(info.size as usize).await?;
                    Ok(Some(crate::Thumbnail {
                        data,
                        format: info.format,
                        width: info.width,
                        height: info.height,
                    }))
                }
                None => Ok(None),
            }
        }
        #[cfg(not(feature = "exif"))]
        {
            let _ = (io, structure);
            Ok(None)
        }
    })
}

/// Read basic EXIF information (make, model, date, ...).
///
/// Only issues [`super::Step::Read`].
#[cfg(feature = "exif")]
pub fn read_exif_info(structure: &Structure) -> Machine<'_, Option<crate::ExifInfo>> {
    Machine::new(move |io| async move {
        let mut src = Src::new(io, structure.total_size);
        get_handler(structure.container)?
            .read_exif_info_async(structure, &mut src)
            .await
    })
}

/// Write a new asset: copies `structure`'s source to the destination, applying `updates`.
///
/// When `process` is true, [`super::Step::Process`] events are emitted for the output bytes
/// (honouring `updates.processing` exclusions), e.g. for hashing while writing. Destination
/// writes are strictly sequential, starting at offset 0. Returns the destination
/// [`Structure`], which can be used with [`update_segment`] once the output is complete.
/// [`super::Progress::total`] is the expected output size.
pub fn write<'a>(
    structure: &'a Structure,
    updates: &'a Updates,
    process: bool,
) -> Machine<'a, Structure> {
    Machine::new(move |io| async move {
        let handler = get_handler(structure.container)?;
        let dest = handler.calculate_updated_structure(structure, updates)?;
        io.set_total(Some(dest.total_size));
        let mut src = Src::new(io.clone(), structure.total_size);
        let mut dst = Dst::new(io);
        let mut pw = PDst::new(&mut dst, process);
        handler
            .write_async(structure, &mut src, &mut pw, updates)
            .await?;
        pw.flush().await?;
        Ok(dest)
    })
}

/// Overwrite an existing segment of `kind` in place (e.g. replace a C2PA placeholder with the
/// signed manifest). `structure` describes the *destination* file.
///
/// Only issues positioned [`super::Step::Write`]s; never reads. Returns the number of payload
/// bytes written (padded to the segment capacity, except for PDF which requires exact length).
pub fn update_segment(
    structure: &Structure,
    kind: crate::SegmentKind,
    data: Vec<u8>,
) -> Machine<'_, usize> {
    Machine::new(move |io| async move {
        let (writes, written) = structure.segment_update_plan(kind, data)?;
        io.set_total(Some(writes.iter().map(|w| w.1.len() as u64).sum()));
        for (offset, bytes) in &writes {
            io.write_at(*offset, bytes).await?;
        }
        Ok(written)
    })
}
