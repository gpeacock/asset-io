// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

// Unless required by applicable law or agreed to in writing,
// this software is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR REPRESENTATIONS OF ANY KIND, either express or
// implied. See the LICENSE-MIT and LICENSE-APACHE files for the
// specific language governing permissions and limitations under
// each license.

//! A read-ahead window for parsers.
//!
//! Parsing a container is a long run of tiny reads (a chunk header, a box header),
//! each at an offset taken from the one before and each preceded by a seek. From a
//! `File` that is a pair of system calls apiece; through a window it is a copy.
//! Every handler parses through [`ReadAhead`].

use std::io::{Read, Seek, SeekFrom};

/// Size of the read-ahead window used while parsing. The markers of a typical JPEG
/// fit in a few KiB. Measured on 24 fixtures (Apple M2): 4 KiB was as fast as or
/// faster than 8, 16 and 32 KiB, from memory and from a file.
pub(crate) const PARSE_WINDOW: usize = 4 * 1024;

/// A read-ahead window over a seekable stream, for a parser that makes many small
/// reads and seeks. A seek only records the position; the stream is touched when a
/// read misses the window. A read at least as large as the window goes straight to
/// the stream. Positions are the stream's own, and the stream's position after use
/// is unspecified. The first seek must be absolute (`Start` or `End`).
pub(crate) struct ReadAhead<'a, R: Read + Seek + ?Sized> {
    inner: &'a mut R,
    /// The window's contents; empty when there is none. Its capacity is never used
    /// uninitialised: `read_to_end` fills spare capacity without zeroing it first.
    buf: Vec<u8>,
    window: usize,
    /// Stream offset of `buf[0]`.
    start: u64,
    /// Logical position.
    pos: u64,
}

impl<'a, R: Read + Seek + ?Sized> ReadAhead<'a, R> {
    pub(crate) fn new(inner: &'a mut R, window: usize) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(window),
            window,
            start: 0,
            pos: 0,
        }
    }
}

impl<R: Read + Seek + ?Sized> Read for ReadAhead<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let in_window = self.pos >= self.start && self.pos < self.start + self.buf.len() as u64;
        if !in_window {
            self.inner.seek(SeekFrom::Start(self.pos))?;
            if out.len() >= self.window {
                let n = self.inner.read(out)?;
                self.pos += n as u64;
                return Ok(n);
            }
            self.buf.clear();
            let filled =
                Read::take(&mut *self.inner, self.window as u64).read_to_end(&mut self.buf);
            if let Err(e) = filled {
                self.buf.clear();
                return Err(e);
            }
            self.start = self.pos;
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let off = (self.pos - self.start) as usize;
        let n = out.len().min(self.buf.len() - off);
        out[..n].copy_from_slice(&self.buf[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek + ?Sized> Seek for ReadAhead<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start")
            })?,
            SeekFrom::End(d) => self.inner.seek(SeekFrom::End(d))?,
        };
        Ok(self.pos)
    }
}
