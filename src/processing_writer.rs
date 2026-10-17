//! Processing callback types for single-pass write-and-process operations.

use crate::segment::Segment;

/// A chunk of data passed to a processor during write.
///
/// Implementors provide the data and optional format-specific metadata.
/// For BMFF mdat boxes, use [`MdatChunk`]; for all other data use [`SimpleChunk`].
///
/// # Example
///
/// ```ignore
/// // Processor that handles both flat hash and mdat
/// let mut processor = |chunk: &dyn ProcessChunk| {
///     if let Some(id) = chunk.id() {
///         builder.hash_bmff_mdat_bytes(id, chunk.data(), chunk.large_size().unwrap_or(false))?;
///     } else {
///         hasher.update(chunk.data());
///     }
///     Ok(())
/// };
/// ```
pub trait ProcessChunk {
    /// The chunk data.
    fn data(&self) -> &[u8];

    /// Optional format-specific id (e.g., mdat box index for BMFF).
    fn id(&self) -> Option<usize> {
        None
    }

    /// Optional format-specific flag (e.g., mdat uses 64-bit size for BMFF).
    fn large_size(&self) -> Option<bool> {
        None
    }

    /// Full logical segment for boundary signals.
    ///
    /// Returns the [`Segment`] when this chunk is a boundary marker, giving
    /// the processor access to the complete logical view — including `is_c2pa()`,
    /// `path`, `ranges`, and any loaded data — without coupling to container
    /// internals.
    fn segment(&self) -> Option<&Segment> {
        None
    }
}

/// Generic chunk for non-mdat data (JPEG, PNG, RIFF, BMFF non-mdat).
#[derive(Debug)]
pub struct SimpleChunk<'a>(pub &'a [u8]);

impl ProcessChunk for SimpleChunk<'_> {
    fn data(&self) -> &[u8] {
        self.0
    }
}

/// BMFF mdat box chunk with format-specific metadata.
#[derive(Debug)]
pub struct MdatChunk<'a> {
    /// Index of the mdat box (0, 1, 2, ...).
    pub id: usize,
    /// Chunk of mdat content (excludes box header).
    pub data: &'a [u8],
    /// True if mdat uses 64-bit size (box > 4GB).
    pub large_size: bool,
}

impl ProcessChunk for MdatChunk<'_> {
    fn data(&self) -> &[u8] {
        self.data
    }

    fn id(&self) -> Option<usize> {
        Some(self.id)
    }

    fn large_size(&self) -> Option<bool> {
        Some(self.large_size)
    }
}

/// Write-side streaming processor: invoked for each chunk during
/// [`crate::Asset::write_with_processing`] (and [`crate::sans_io`] write machines).
///
/// This uses the usual supertrait + blanket impl pattern for the higher-ranked
/// `FnMut` bound (an object-safe `ProcessChunk` reference for any lifetime).
pub trait ProcessChunkFn: for<'a> FnMut(&'a (dyn ProcessChunk + 'a)) -> crate::Result<()> {}

impl<T> ProcessChunkFn for T where T: for<'a> FnMut(&'a (dyn ProcessChunk + 'a)) -> crate::Result<()>
{}

/// Read-side streaming processor: invoked for each byte slice during
/// [`crate::Asset::read_with_processing`].
pub trait ReadChunkFn: FnMut(&[u8]) -> crate::Result<()> {}

impl<T> ReadChunkFn for T where T: FnMut(&[u8]) -> crate::Result<()> {}
