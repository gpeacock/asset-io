//! Processing writer wrapper for single-pass write-and-process operations
//!
//! This module provides a `Write` wrapper that intercepts write calls and
//! processes data through a callback before forwarding to the underlying writer.

use std::io::{Result, Write};

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
/// ```rust
/// use std::io::Write;
/// # use asset_io::processing_writer::ProcessingWriter;
/// use sha2::{Sha256, Digest};
///
/// let mut output = Vec::new();
/// let mut hasher = Sha256::new();
///
/// let mut writer = ProcessingWriter::new(&mut output, |data| {
///     hasher.update(data);
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
pub struct ProcessingWriter<W: Write, F: FnMut(&[u8])> {
    writer: W,
    processor: F,
    exclude_mode: bool,
}

impl<W: Write, F: FnMut(&[u8])> ProcessingWriter<W, F> {
    /// Create a new processing writer
    ///
    /// # Arguments
    ///
    /// * `writer` - The underlying writer to forward data to
    /// * `processor` - Callback function that processes each chunk of data
    pub fn new(writer: W, processor: F) -> Self {
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
    #[allow(dead_code)]  // Used by container handlers when they override write_with_processor
    pub fn set_exclude_mode(&mut self, exclude: bool) {
        self.exclude_mode = exclude;
    }

    /// Check if exclude mode is currently active
    #[allow(dead_code)]  // Provided for completeness
    pub fn is_exclude_mode(&self) -> bool {
        self.exclude_mode
    }

    /// Consume the wrapper and return the underlying writer
    #[allow(dead_code)]  // Provided for completeness
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Get a reference to the underlying writer
    #[allow(dead_code)]  // Provided for completeness
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Get a mutable reference to the underlying writer
    #[allow(dead_code)]  // Provided for completeness
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }
}

impl<W: Write, F: FnMut(&[u8])> Write for ProcessingWriter<W, F> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // Process data if not in exclude mode
        if !self.exclude_mode {
            (self.processor)(buf);
        }

        // Forward to underlying writer
        self.writer.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        // Process data if not in exclude mode
        if !self.exclude_mode {
            (self.processor)(buf);
        }

        // Forward to underlying writer
        self.writer.write_all(buf)
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }

    // Forward other Write methods for optimal performance
    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> Result<usize> {
        if !self.exclude_mode {
            // Process each buffer
            for buf in bufs {
                (self.processor)(buf);
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

// Implement Seek if the underlying writer supports it
impl<W: Write + std::io::Seek, F: FnMut(&[u8])> std::io::Seek for ProcessingWriter<W, F> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> Result<u64> {
        self.writer.seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn test_basic_processing() {
        let mut output = Vec::new();
        let mut hasher = Sha256::new();

        let mut writer = ProcessingWriter::new(&mut output, |data| {
            hasher.update(data);
        });

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

        let mut writer = ProcessingWriter::new(&mut output, |data| {
            processed.extend_from_slice(data);
        });

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
    fn test_multiple_writes() {
        let mut output = Vec::new();
        let mut count = 0;

        let mut writer = ProcessingWriter::new(&mut output, |data| {
            count += data.len();
        });

        writer.write_all(b"a").unwrap();
        writer.write_all(b"bc").unwrap();
        writer.write_all(b"def").unwrap();

        assert_eq!(output, b"abcdef");
        assert_eq!(count, 6);
    }
}
