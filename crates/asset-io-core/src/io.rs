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

//! Windowed, asynchronous stream reads for container handlers.

use crate::error::{Error, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

/// How much of the stream is requested at a time unless the code asks for more. The
/// markers of a typical JPEG, and the headers of a typical PNG, fit in a few KiB.
/// Measured on 24 fixtures (Apple M2): 4 KiB was as fast as or faster than 8, 16
/// and 32 KiB, from memory and from a file.
pub const DEFAULT_WINDOW: usize = 4 * 1024;

/// A boxed future that can move between threads, as the `async` parts of a
/// [`ContainerHandler`](crate::ContainerHandler) return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a suspended reader is waiting for, and the buffer it will be given.
#[derive(Default)]
pub(super) struct Slot {
    /// `(offset, minimum length, window length)` of the outstanding request.
    pub(super) request: Option<(u64, usize, usize)>,
    /// The window's buffer while the reader is suspended, so the caller can fill it
    /// and no copy is made.
    pub(super) buf: Vec<u8>,
}

/// The one thing a reader and its driver share. It is locked only when the reader
/// suspends or resumes, never on a read that the window already holds.
#[derive(Default)]
pub(super) struct Link {
    slot: Mutex<Slot>,
}

impl Link {
    pub(super) fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A window onto a stream of known length, for code that reads it.
///
/// Reads are `async`: each either completes at once from the window, or suspends
/// until the driver has supplied the bytes.
pub struct Io {
    link: Arc<Link>,
    total: u64,
    /// The window: stream bytes `[start, start + win.len())`.
    win: Vec<u8>,
    start: u64,
}

impl Io {
    pub(super) fn new(total: u64, link: Arc<Link>) -> Self {
        Self {
            link,
            total,
            win: Vec::new(),
            start: 0,
        }
    }

    /// Length of the stream.
    pub fn len(&self) -> u64 {
        self.total
    }

    /// Whether the stream is empty.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    fn covers(&self, offset: u64, len: usize) -> bool {
        offset >= self.start
            && offset
                .checked_add(len as u64)
                .is_some_and(|end| end <= self.start + self.win.len() as u64)
    }

    /// Read exactly `buf.len()` bytes at `offset`. Fails with an unexpected-end
    /// error if they are not all inside the stream.
    pub async fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        // Most reads are held by the window already.
        if buf.is_empty() {
            return Ok(());
        }
        if self.covers(offset, buf.len()) {
            let from = (offset - self.start) as usize;
            buf.copy_from_slice(&self.win[from..from + buf.len()]);
            return Ok(());
        }
        let bytes = self.fetch(offset, buf.len()).await?;
        buf.copy_from_slice(&bytes[..buf.len()]);
        Ok(())
    }

    /// The bytes of the stream from `offset` to the end of the window, at least
    /// `min` of them, without copying. Use them before the next read: the slice
    /// borrows the `Io`.
    pub async fn fetch(&mut self, offset: u64, min: usize) -> Result<&[u8]> {
        self.fetch_with(offset, min, DEFAULT_WINDOW).await
    }

    /// Like [`fetch`](Self::fetch), asking the driver for a window of `window`
    /// bytes when the data is not already held: a scan that will read on from here
    /// asks for more than the default.
    pub async fn fetch_with(&mut self, offset: u64, min: usize, window: usize) -> Result<&[u8]> {
        if min == 0 {
            return Ok(&[]);
        }
        if self.covers(offset, min) {
            return Ok(&self.win[(offset - self.start) as usize..]);
        }
        Fetch {
            io: &mut *self,
            offset,
            min,
            window: window.max(min),
            asked: false,
        }
        .await?;
        let from = (offset - self.start) as usize;
        Ok(&self.win[from..])
    }

    /// `len` bytes at `offset` in a new `Vec`. The bytes are read straight into it,
    /// so a large read is not copied. The caller bounds `len`.
    pub async fn read_vec(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        // A window of exactly the length wanted, taken whole.
        self.fetch_with(offset, len, len).await?;
        let from = (offset - self.start) as usize;
        let mut v = std::mem::take(&mut self.win);
        v.drain(..from);
        v.truncate(len);
        self.start = 0;
        Ok(v)
    }
}

impl Io {
    /// A cursor over the stream, starting at `at`, for reading a run of fields one
    /// after another. It borrows the `Io`, so its position is visible only where it
    /// is used; the `Io` itself has no position.
    pub fn reader(&mut self, at: u64) -> Reader<'_> {
        Reader { io: self, pos: at }
    }
}

/// A position in the stream and the reads that move it; see [`Io::reader`].
pub struct Reader<'a> {
    io: &'a mut Io,
    pos: u64,
}

impl Reader<'_> {
    /// Where the next read starts.
    pub fn pos(&self) -> u64 {
        self.pos
    }

    /// Move to `to`.
    pub fn seek(&mut self, to: u64) {
        self.pos = to;
    }

    /// Move `n` bytes on, without reading them.
    pub fn skip(&mut self, n: u64) {
        self.pos = self.pos.saturating_add(n);
    }

    /// Bytes from the position to the end of the stream.
    pub fn remaining(&self) -> u64 {
        self.io.total.saturating_sub(self.pos)
    }

    /// The next `n` bytes, borrowed from the window, and move past them.
    pub async fn take(&mut self, n: usize) -> Result<&[u8]> {
        let at = self.pos;
        self.pos = at.saturating_add(n as u64);
        let bytes = self.io.fetch(at, n).await?;
        Ok(&bytes[..n])
    }

    /// Fill `buf` from the next bytes, and move past them.
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let at = self.pos;
        self.pos = at.saturating_add(buf.len() as u64);
        self.io.read_at(at, buf).await
    }

    /// The next `N` bytes, if the window holds them: no suspension, so no future to
    /// build. Most reads of a field are this.
    fn held<const N: usize>(&mut self) -> Option<[u8; N]> {
        if !self.io.covers(self.pos, N) {
            return None;
        }
        let from = (self.pos - self.io.start) as usize;
        self.pos += N as u64;
        self.io.win[from..from + N].try_into().ok()
    }

    /// The next `N` bytes.
    pub async fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        if let Some(a) = self.held::<N>() {
            return Ok(a);
        }
        let mut a = [0u8; N];
        self.read_exact(&mut a).await?;
        Ok(a)
    }

    /// One byte.
    pub async fn u8(&mut self) -> Result<u8> {
        if let Some([b]) = self.held::<1>() {
            return Ok(b);
        }
        Ok(self.array::<1>().await?[0])
    }

    /// A big-endian `u16`.
    pub async fn u16_be(&mut self) -> Result<u16> {
        if let Some(a) = self.held() {
            return Ok(u16::from_be_bytes(a));
        }
        Ok(u16::from_be_bytes(self.array().await?))
    }

    /// A little-endian `u16`.
    pub async fn u16_le(&mut self) -> Result<u16> {
        if let Some(a) = self.held() {
            return Ok(u16::from_le_bytes(a));
        }
        Ok(u16::from_le_bytes(self.array().await?))
    }

    /// A big-endian `u32`.
    pub async fn u32_be(&mut self) -> Result<u32> {
        if let Some(a) = self.held() {
            return Ok(u32::from_be_bytes(a));
        }
        Ok(u32::from_be_bytes(self.array().await?))
    }

    /// A little-endian `u32`.
    pub async fn u32_le(&mut self) -> Result<u32> {
        if let Some(a) = self.held() {
            return Ok(u32::from_le_bytes(a));
        }
        Ok(u32::from_le_bytes(self.array().await?))
    }

    /// A big-endian `u64`.
    pub async fn u64_be(&mut self) -> Result<u64> {
        if let Some(a) = self.held() {
            return Ok(u64::from_be_bytes(a));
        }
        Ok(u64::from_be_bytes(self.array().await?))
    }

    /// A little-endian `u64`.
    pub async fn u64_le(&mut self) -> Result<u64> {
        if let Some(a) = self.held() {
            return Ok(u64::from_le_bytes(a));
        }
        Ok(u64::from_le_bytes(self.array().await?))
    }
}

/// The future behind [`Io::fetch_with`].
struct Fetch<'a> {
    io: &'a mut Io,
    offset: u64,
    min: usize,
    window: usize,
    asked: bool,
}

/// The error a short read gives: the same one `Read::read_exact` does, so that the
/// message does not depend on how the bytes were read.
fn eof() -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "failed to fill whole buffer",
    ))
}

impl Future for Fetch<'_> {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        if this.asked {
            // Resumed: the driver has filled the buffer.
            let mut slot = this.io.link.lock();
            this.io.win = std::mem::take(&mut slot.buf);
            slot.request = None;
            this.io.start = this.offset;
            this.asked = false;
            return Poll::Ready(if this.io.covers(this.offset, this.min) {
                Ok(())
            } else {
                Err(eof())
            });
        }
        if this.io.covers(this.offset, this.min) {
            return Poll::Ready(Ok(()));
        }
        let inside = this
            .offset
            .checked_add(this.min as u64)
            .is_some_and(|end| end <= this.io.total);
        if !inside {
            return Poll::Ready(Err(eof()));
        }
        // Not held: suspend and ask for a window, no longer than the stream.
        let window = (this.window as u64).min(this.io.total - this.offset) as usize;
        let mut buf = std::mem::take(&mut this.io.win);
        buf.clear();
        let mut slot = this.io.link.lock();
        slot.request = Some((this.offset, this.min, window.max(this.min)));
        slot.buf = buf;
        this.asked = true;
        Poll::Pending
    }
}
