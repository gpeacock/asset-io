//! Processing writer wrapper for single-pass write-and-process operations
//!
//! This module provides a `Write` wrapper that intercepts write calls and
//! processes data through a callback before forwarding to the underlying writer.

use crate::error::set_pending_processor_error;
use std::io::{Read, Result, Write};

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
/// [`crate::Asset::write_with_processing`] and [`ProcessingWriter`].
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

/// A writer wrapper that processes data through a callback before writing
///
/// This enables single-pass operations where data is processed (e.g., hashed)
/// as it's being written, without needing to re-read the output.
///
/// # Exclude Mode
///
/// The wrapper supports an "exclude mode" that temporarily disables processing.
/// This is useful for writing metadata segments that should be excluded from
/// processing (e.g., C2PA JUMBF should not be included in the asset hash).
///
/// # Example
///
/// ```ignore
/// use std::io::Write;
/// use asset_io::ProcessingWriter;
/// use sha2::{Sha256, Digest};
///
/// let mut output = Vec::new();
/// let mut hasher = Sha256::new();
///
/// let mut writer = ProcessingWriter::new(&mut output, |chunk: &dyn ProcessChunk| {
///     hasher.update(chunk.data());
///     Ok(())
/// });
///
/// // This data will be processed
/// writer.write_all(b"hello")?;
///
/// // This data will NOT be processed (excluded)
/// writer.set_exclude_mode(true);
/// writer.write_all(b"excluded")?;
/// writer.set_exclude_mode(false);
///
/// // This data will be processed again
/// writer.write_all(b"world")?;
///
/// let hash = hasher.finalize();
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct ProcessingWriter<'a, W: Write, F>
where
    F: ProcessChunkFn,
{
    writer: W,
    processor: &'a mut F,
    exclude_mode: bool,
}

impl<'a, W: Write, F> ProcessingWriter<'a, W, F>
where
    F: ProcessChunkFn,
{
    /// Create a new processing writer
    ///
    /// # Arguments
    ///
    /// * `writer` - The underlying writer to forward data to
    /// * `processor` - Callback that receives each chunk of data
    pub fn new(writer: W, processor: &'a mut F) -> Self {
        Self {
            writer,
            processor,
            exclude_mode: false,
        }
    }

    /// Set exclude mode
    ///
    /// When exclude mode is enabled, data is written but not processed.
    /// This is useful for writing segments that should be excluded from
    /// processing (e.g., metadata that shouldn't be included in a hash).
    ///
    /// # Arguments
    ///
    /// * `exclude` - If true, temporarily disable processing. If false, re-enable.
    #[allow(dead_code)] // Used by container handlers when they override write_with_processor
    pub fn set_exclude_mode(&mut self, exclude: bool) {
        self.exclude_mode = exclude;
    }

    /// Check if exclude mode is currently active
    #[allow(dead_code)] // Provided for completeness
    pub fn is_exclude_mode(&self) -> bool {
        self.exclude_mode
    }

    /// Consume the wrapper and return the underlying writer
    #[allow(dead_code)] // Provided for completeness
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Get a reference to the underlying writer
    #[allow(dead_code)] // Provided for completeness
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Get a mutable reference to the underlying writer
    #[allow(dead_code)] // Provided for completeness
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Process an offset value for BMFF V2 hashing without writing it
    ///
    /// In BMFF V2 hashing, top-level box offsets are hashed as 8-byte
    /// big-endian values instead of hashing the box content itself.
    /// This method adds the offset to the hash without writing it to the output.
    ///
    /// # Arguments
    ///
    /// * `offset` - The file offset to hash (as 8-byte big-endian)
    #[allow(dead_code)]
    pub fn process_offset(&mut self, offset: u64) -> Result<()> {
        if !self.exclude_mode {
            let chunk = SimpleChunk(&offset.to_be_bytes());
            if let Err(e) = (self.processor)(&chunk as &dyn ProcessChunk) {
                set_pending_processor_error(e);
                return Err(io_processor_fail());
            }
        }
        Ok(())
    }

    /// Process a chunk (for BMFF handler to call when streaming mdat boxes).
    #[allow(dead_code)]
    pub fn process_chunk(&mut self, chunk: impl ProcessChunk) -> Result<()> {
        if let Err(e) = (self.processor)(&chunk as &dyn ProcessChunk) {
            set_pending_processor_error(e);
            return Err(io_processor_fail());
        }
        Ok(())
    }
}

fn io_processor_fail() -> std::io::Error {
    std::io::Error::other(crate::error::PROCESSOR_IO_SENTINEL)
}

impl<W: Write, F> Write for ProcessingWriter<'_, W, F>
where
    F: ProcessChunkFn,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if !self.exclude_mode {
            let chunk = SimpleChunk(buf);
            if let Err(e) = (self.processor)(&chunk as &dyn ProcessChunk) {
                set_pending_processor_error(e);
                return Err(io_processor_fail());
            }
        }
        self.writer.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        if !self.exclude_mode {
            let chunk = SimpleChunk(buf);
            if let Err(e) = (self.processor)(&chunk as &dyn ProcessChunk) {
                set_pending_processor_error(e);
                return Err(io_processor_fail());
            }
        }
        self.writer.write_all(buf)
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> Result<usize> {
        if !self.exclude_mode {
            for buf in bufs {
                let chunk = SimpleChunk(buf);
                if let Err(e) = (self.processor)(&chunk as &dyn ProcessChunk) {
                    set_pending_processor_error(e);
                    return Err(io_processor_fail());
                }
            }
        }
        self.writer.write_vectored(bufs)
    }

    fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> Result<()> {
        // For write_fmt, we need to format to a buffer first to process it
        let formatted = format!("{}", fmt);
        self.write_all(formatted.as_bytes())
    }
}

// Implement Read if the underlying writer supports it (needed for BMFF chunk offset adjustment)
impl<W: Read + Write, F> Read for ProcessingWriter<'_, W, F>
where
    F: ProcessChunkFn,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.writer.read(buf)
    }
}

// Implement Seek if the underlying writer supports it
impl<W: Write + std::io::Seek, F> std::io::Seek for ProcessingWriter<'_, W, F>
where
    F: ProcessChunkFn,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> Result<u64> {
        self.writer.seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use sha2::{Digest, Sha256};

    #[test]
    fn test_basic_processing() {
        let mut output = Vec::new();
        let mut hasher = Sha256::new();
        let mut processor = |chunk: &dyn ProcessChunk| {
            hasher.update(chunk.data());
            Ok(())
        };

        let mut writer = ProcessingWriter::new(&mut output, &mut processor);

        writer.write_all(b"hello world").unwrap();

        let hash1 = hasher.finalize();

        // Verify data was written
        assert_eq!(output, b"hello world");

        // Verify hash matches direct hashing
        let mut direct_hasher = Sha256::new();
        direct_hasher.update(b"hello world");
        let hash2 = direct_hasher.finalize();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_exclude_mode() {
        let mut output = Vec::new();
        let mut processed = Vec::new();
        let mut processor = |chunk: &dyn ProcessChunk| {
            processed.extend_from_slice(chunk.data());
            Ok(())
        };

        let mut writer = ProcessingWriter::new(&mut output, &mut processor);

        // Write with processing
        writer.write_all(b"included1").unwrap();

        // Write without processing
        writer.set_exclude_mode(true);
        writer.write_all(b"excluded").unwrap();
        writer.set_exclude_mode(false);

        // Write with processing again
        writer.write_all(b"included2").unwrap();

        // Verify all data was written
        assert_eq!(output, b"included1excludedincluded2");

        // Verify only non-excluded data was processed
        assert_eq!(processed, b"included1included2");
    }

    #[test]
    fn test_processor_user_canceled_surfaces_via_from_io() {
        let mut output = Vec::new();
        let mut processor = |_chunk: &dyn ProcessChunk| Err(Error::UserCanceled);
        let mut writer = ProcessingWriter::new(&mut output, &mut processor);
        let io_err = writer.write_all(b"hello").expect_err("expected io error");
        let asset_err: Error = io_err.into();
        assert!(matches!(asset_err, Error::UserCanceled));
    }

    #[test]
    fn test_multiple_writes() {
        let mut output = Vec::new();
        let mut count = 0;
        let mut processor = |chunk: &dyn ProcessChunk| {
            count += chunk.data().len();
            Ok(())
        };

        let mut writer = ProcessingWriter::new(&mut output, &mut processor);

        writer.write_all(b"a").unwrap();
        writer.write_all(b"bc").unwrap();
        writer.write_all(b"def").unwrap();

        assert_eq!(output, b"abcdef");
        assert_eq!(count, 6);
    }
}
