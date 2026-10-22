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

//! Code that reads a stream, written once, run by any I/O; see [`Task`].

use crate::error::{Error, Result};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// How much of the stream is requested at a time unless the code asks for more. The
/// markers of a typical JPEG, and the headers of a typical PNG, fit in a few KiB.
/// Measured on 24 fixtures (Apple M2): 4 KiB was as fast as or faster than 8, 16
/// and 32 KiB, from memory and from a file.
pub const DEFAULT_WINDOW: usize = 4 * 1024;

/// A boxed future that can move between threads, as the `async` parts of a
/// [`ContainerHandler`](crate::ContainerHandler) return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a suspended task is waiting for, and the buffer it will be given.
#[derive(Default)]
struct Slot {
    /// `(offset, minimum length, window length)` of the outstanding request.
    request: Option<(u64, usize, usize)>,
    /// The window's buffer while the task is suspended, so the caller can fill it
    /// and no copy is made.
    buf: Vec<u8>,
}

/// The one thing a task and its driver share. It is locked only when the task
/// suspends or resumes, never on a read that the window already holds.
#[derive(Default)]
struct Link {
    slot: Mutex<Slot>,
}

impl Link {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A window onto a stream of known length, for code that reads it.
///
/// An `Io` belongs to one [`Task`]. Its reads are `async`: each either completes
/// at once from the window, or suspends the task until its driver has supplied the
/// bytes.
pub struct Io {
    link: Arc<Link>,
    total: u64,
    /// The window: stream bytes `[start, start + win.len())`.
    win: Vec<u8>,
    start: u64,
}

impl Io {
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

/// A suspended computation that reads a stream: run it with [`run`](Self::run), or
/// drive it yourself with [`resume`](Self::resume).
///
/// Code that reads is written once, as straight-line `async` code over an [`Io`]:
///
/// ```
/// # use asset_io::{Io, Progress, Result, Task};
/// async fn first_byte(io: &mut Io) -> Result<u8> {
///     let mut b = [0u8; 1];
///     io.read_at(0, &mut b).await?;
///     Ok(b[0])
/// }
///
/// # fn main() -> Result<()> {
/// let data = [7u8, 8, 9];
/// let mut task = Task::new(data.len() as u64, |mut io| async move { first_byte(&mut io).await });
/// let first = loop {
///     match task.resume()? {
///         // The task has nothing to read from; it asks. Answer however you like.
///         Progress::Read(mut req) => {
///             let at = req.offset() as usize;
///             req.buffer().extend_from_slice(&data[at..]);
///         }
///         Progress::Done(v) => break v,
///         _ => unreachable!(),
///     }
/// };
/// assert_eq!(first, 7);
/// # Ok(())
/// # }
/// ```
///
/// The task does not read anything itself. When it needs bytes that its [`Io`] does
/// not already hold, it suspends, and [`resume`](Self::resume) returns a
/// [`ReadRequest`]: where to read and how much. The caller reads however it likes
/// (blocking, `async`, from a network buffer it already has), puts the bytes in the
/// request's buffer, and resumes. [`run`](Self::run) is the blocking driver for a
/// `Read + Seek`. No runtime is involved and no I/O trait is required: the task is
/// driven by plain method calls, so it is `Send` and works under any executor.
///
/// [`Io`] keeps a window of the stream, so a run of tiny reads (a chunk header, a
/// box header) becomes one request for a few KiB, and the code makes no system call
/// of its own.
pub struct Task<'f, T> {
    fut: Pin<Box<dyn Future<Output = Result<T>> + Send + 'f>>,
    link: Arc<Link>,
    finished: bool,
}

impl<'f, T> Task<'f, T> {
    /// A task for the stream of `total` bytes that `make` reads through the `Io` it
    /// is given. Nothing runs until the first [`resume`](Self::resume).
    pub fn new<F>(total: u64, make: impl FnOnce(Io) -> F) -> Self
    where
        F: Future<Output = Result<T>> + Send + 'f,
    {
        let link = Arc::new(Link::default());
        let io = Io {
            link: link.clone(),
            total,
            win: Vec::new(),
            start: 0,
        };
        Self {
            fut: Box::pin(make(io)),
            link,
            finished: false,
        }
    }

    /// Run until the task needs bytes it does not have, or finishes.
    ///
    /// On [`Progress::Read`], fill the request's buffer and call `resume` again.
    /// An error is final. Calling `resume` after the task has finished is an
    /// error.
    pub fn resume(&mut self) -> Result<Progress<'_, T>> {
        if self.finished {
            return Err(Error::InvalidFormat("task already finished".into()));
        }
        let mut cx = Context::from_waker(Waker::noop());
        match self.fut.as_mut().poll(&mut cx) {
            Poll::Ready(done) => {
                self.finished = true;
                done.map(Progress::Done)
            }
            Poll::Pending => {
                let slot = self.link.lock();
                if slot.request.is_none() {
                    self.finished = true;
                    return Err(Error::InvalidFormat(
                        "task suspended on something other than a read".into(),
                    ));
                }
                Ok(Progress::Read(ReadRequest { slot }))
            }
        }
    }

    /// Run to completion, reading from `src`, which must be the stream the task was
    /// made for.
    pub fn run<R: Read + Seek + ?Sized>(mut self, src: &mut R) -> Result<T> {
        // Where the stream is, so that a request that continues the last one does
        // not seek.
        let mut pos = None;
        loop {
            match self.resume()? {
                Progress::Read(req) => pos = Some(req.fill_from(src, pos)?),
                Progress::Done(v) => return Ok(v),
            }
        }
    }
}

/// What [`Task::resume`] found.
#[derive(Debug)]
#[non_exhaustive]
pub enum Progress<'a, T> {
    /// The task needs bytes; supply them and resume.
    Read(ReadRequest<'a>),
    /// The task finished with this result.
    Done(T),
}

/// Bytes a suspended task is waiting for.
///
/// Put the stream's bytes from [`offset`](Self::offset) in
/// [`buffer`](Self::buffer): at least [`min_len`](Self::min_len) of them, and up to
/// [`window_len`](Self::window_len) if they are as easy to read. Fewer than
/// `min_len` is an unexpected end of the stream, and the task fails. Then resume the
/// task.
pub struct ReadRequest<'a> {
    slot: MutexGuard<'a, Slot>,
}

impl std::fmt::Debug for ReadRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadRequest")
            .field("offset", &self.offset())
            .field("min_len", &self.min_len())
            .field("window_len", &self.window_len())
            .finish()
    }
}

impl ReadRequest<'_> {
    fn request(&self) -> (u64, usize, usize) {
        self.slot.request.unwrap_or_default()
    }

    /// Where in the stream the bytes are wanted.
    pub fn offset(&self) -> u64 {
        self.request().0
    }

    /// The fewest bytes the task can go on with.
    pub fn min_len(&self) -> usize {
        self.request().1
    }

    /// How many bytes the task would like: at least `min_len`, and never past the
    /// end of the stream.
    pub fn window_len(&self) -> usize {
        self.request().2
    }

    /// The buffer to fill. It starts empty; put the bytes in it, replacing what it
    /// holds. Its capacity is reused between requests.
    pub fn buffer(&mut self) -> &mut Vec<u8> {
        &mut self.slot.buf
    }

    /// Fill the buffer from `src`, a seekable stream: seek to the offset and read up
    /// to `window_len` bytes. The buffer is not zeroed first.
    pub fn fill<R: Read + Seek + ?Sized>(self, src: &mut R) -> Result<()> {
        self.fill_from(src, None).map(|_| ())
    }

    /// [`fill`](Self::fill), not seeking if `pos`, where the stream is now, is
    /// already the offset. Returns where the stream is afterwards.
    fn fill_from<R: Read + Seek + ?Sized>(mut self, src: &mut R, pos: Option<u64>) -> Result<u64> {
        let (offset, _, window) = self.request();
        if pos != Some(offset) {
            src.seek(SeekFrom::Start(offset))?;
        }
        let buf = self.buffer();
        buf.clear();
        // Room for the whole window, so that the read does not probe and grow.
        buf.reserve(window);
        Read::take(&mut *src, window as u64).read_to_end(buf)?;
        Ok(offset + buf.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Records `[len][len bytes]`; the sum of each record's bytes.
    async fn sums(io: &mut Io) -> Result<Vec<u32>> {
        let (mut at, mut out) = (0u64, Vec::new());
        while at < io.len() {
            let mut l = [0u8; 1];
            io.read_at(at, &mut l).await?;
            let body = io.fetch(at + 1, l[0] as usize).await?;
            out.push(body[..l[0] as usize].iter().map(|&b| b as u32).sum());
            at += 1 + l[0] as u64;
        }
        Ok(out)
    }

    fn records(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            let len = (i * 7 % 23) as u8;
            v.push(len);
            v.extend((0..len).map(|j| (i as u8).wrapping_add(j)));
        }
        v
    }

    fn direct(data: &[u8]) -> Vec<u32> {
        let (mut at, mut out) = (0usize, Vec::new());
        while at < data.len() {
            let l = data[at] as usize;
            out.push(data[at + 1..at + 1 + l].iter().map(|&b| b as u32).sum());
            at += 1 + l;
        }
        out
    }

    /// Run `sums`, counting the requests made.
    fn run_counting(data: &[u8]) -> (Vec<u32>, usize) {
        let mut task = Task::new(
            data.len() as u64,
            |mut io| async move { sums(&mut io).await },
        );
        let mut src = Cursor::new(data);
        let mut requests = 0;
        loop {
            match task.resume().unwrap() {
                Progress::Read(req) => {
                    requests += 1;
                    req.fill(&mut src).unwrap();
                }
                Progress::Done(v) => return (v, requests),
            }
        }
    }

    #[test]
    fn a_task_runs_to_the_same_result_as_direct_code() {
        let data = records(5_000);
        let (got, _) = run_counting(&data);
        assert_eq!(got, direct(&data));
        // And through the blocking driver.
        let t = Task::new(
            data.len() as u64,
            |mut io| async move { sums(&mut io).await },
        );
        assert_eq!(t.run(&mut Cursor::new(&data)).unwrap(), direct(&data));
    }

    #[test]
    fn tiny_reads_become_few_requests() {
        let data = records(5_000); // ~60 KB of records, ~10,000 reads
        let (_, requests) = run_counting(&data);
        let windows = data.len().div_ceil(DEFAULT_WINDOW);
        assert!(
            requests <= windows + 2,
            "{requests} requests, {windows} windows"
        );
    }

    #[test]
    fn nothing_is_read_before_the_first_resume() {
        let t = Task::new(10, |mut io| async move {
            let mut b = [0u8; 1];
            io.read_at(0, &mut b).await?;
            Ok(b[0])
        });
        // Dropping an unstarted task is fine.
        drop(t);
    }

    #[test]
    fn requests_say_where_and_how_much() {
        let mut task = Task::new(100_000, |mut io| async move {
            let mut b = [0u8; 4];
            io.read_at(50_000, &mut b).await?;
            io.fetch_with(60_000, 10, 32 * 1024).await?;
            Ok(())
        });
        let mut seen = Vec::new();
        let src = vec![0u8; 100_000];
        while let Progress::Read(req) = task.resume().unwrap() {
            seen.push((req.offset(), req.min_len(), req.window_len()));
            req.fill(&mut Cursor::new(&src)).unwrap();
        }
        assert_eq!(seen, vec![(50_000, 4, 4096), (60_000, 10, 32 * 1024)]);
    }

    #[test]
    fn a_window_is_not_asked_for_past_the_end() {
        let mut task = Task::new(100, |mut io| async move {
            io.fetch(90, 4).await?;
            Ok(())
        });
        let Progress::Read(req) = task.resume().unwrap() else {
            panic!("expected a request");
        };
        assert_eq!((req.offset(), req.min_len(), req.window_len()), (90, 4, 10));
    }

    #[test]
    fn reads_outside_the_stream_fail_without_a_request() {
        let mut task = Task::new(10, |mut io| async move {
            let mut b = [0u8; 4];
            io.read_at(8, &mut b).await
        });
        assert!(matches!(task.resume(), Err(Error::Io(_))));
        assert!(task.resume().is_err()); // finished
    }

    #[test]
    fn a_short_answer_is_an_unexpected_end() {
        let mut task = Task::new(100, |mut io| async move {
            let mut b = [0u8; 8];
            io.read_at(0, &mut b).await
        });
        match task.resume().unwrap() {
            Progress::Read(mut req) => req.buffer().extend_from_slice(&[1, 2, 3]),
            Progress::Done(_) => panic!("expected a request"),
        }
        let Err(Error::Io(e)) = task.resume() else {
            panic!("expected an I/O error");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_vec_takes_the_window_without_a_copy() {
        let data: Vec<u8> = (0..=255u8).cycle().take(50_000).collect();
        let t = Task::new(data.len() as u64, |mut io| async move {
            let mut head = [0u8; 2];
            io.read_at(10, &mut head).await?;
            let big = io.read_vec(1_000, 30_000).await?;
            // The window was handed over; the next read asks again.
            let mut again = [0u8; 2];
            io.read_at(1_000, &mut again).await?;
            Ok((head, big, again))
        });
        let (head, big, again) = t.run(&mut Cursor::new(&data)).unwrap();
        assert_eq!(head, [10, 11]);
        assert_eq!(big, data[1_000..31_000]);
        assert_eq!(again, [(1_000 % 256) as u8, (1_001 % 256) as u8]);
    }

    #[test]
    fn a_read_inside_the_window_makes_no_request() {
        let data = vec![7u8; 8_000];
        let mut task = Task::new(8_000, |mut io| async move {
            let mut b = [0u8; 8];
            for at in [0u64, 8, 100, 4_000] {
                io.read_at(at, &mut b).await?;
            }
            Ok(())
        });
        let mut n = 0;
        while let Progress::Read(req) = task.resume().unwrap() {
            n += 1;
            req.fill(&mut Cursor::new(&data)).unwrap();
        }
        assert_eq!(n, 1); // offsets 0..4095 held by the first window
    }

    #[test]
    fn a_reader_reads_fields_in_order_and_tracks_its_position() {
        let data: Vec<u8> = (0u8..64).collect();
        let t = Task::new(64, |mut io| async move {
            let mut r = io.reader(4);
            let a = r.u8().await?;
            let b = r.u16_be().await?;
            let c = r.u16_le().await?;
            let d = r.u32_be().await?;
            let e = r.u32_le().await?;
            let f = r.u64_be().await?;
            let g = r.u64_le().await?;
            let arr = r.array::<2>().await?;
            let pos = r.pos();
            r.skip(3);
            let taken = r.take(2).await?.to_vec();
            let after = r.pos();
            r.seek(0);
            let rem = r.remaining();
            Ok((a, b, c, d, e, f, g, arr, pos, taken, after, rem))
        });
        let (a, b, c, d, e, f, g, arr, pos, taken, after, rem) =
            t.run(&mut Cursor::new(&data)).unwrap();
        assert_eq!(a, 4);
        assert_eq!(b, 0x0506);
        assert_eq!(c, 0x0807);
        assert_eq!(d, 0x090A_0B0C);
        assert_eq!(e, 0x100F_0E0D);
        assert_eq!(f, 0x1112_1314_1516_1718);
        assert_eq!(g, 0x201F_1E1D_1C1B_1A19);
        assert_eq!(arr, [0x21, 0x22]);
        assert_eq!(pos, 4 + 1 + 2 + 2 + 4 + 4 + 8 + 8 + 2);
        assert_eq!(taken, vec![pos as u8 + 3, pos as u8 + 4]);
        assert_eq!(after, pos + 3 + 2);
        assert_eq!(rem, 64);
    }

    #[test]
    fn a_reader_past_the_end_fails() {
        let t = Task::new(6, |mut io| async move {
            let mut r = io.reader(4);
            r.u32_be().await
        });
        assert!(matches!(
            t.run(&mut Cursor::new(vec![0u8; 6])),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn empty_reads_need_nothing() {
        let t = Task::new(4, |mut io| async move {
            io.read_at(3, &mut []).await?;
            assert!(io.fetch(2, 0).await?.is_empty());
            Ok(io.reader(1).take(0).await?.len())
        });
        let mut n = 0;
        let mut task = t;
        while let Ok(Progress::Read(_)) = task.resume() {
            n += 1;
        }
        assert_eq!(n, 0);
    }

    #[test]
    fn a_task_is_send() {
        fn needs_send<T: Send>(_: &T) {}
        let t: Task<'_, ()> = Task::new(1, |_| async { Ok(()) });
        needs_send(&t);
    }

    #[test]
    fn a_task_that_suspends_on_something_else_is_an_error() {
        struct Never;
        impl Future for Never {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
                Poll::Pending
            }
        }
        let mut task: Task<'_, ()> = Task::new(1, |_| async {
            Never.await;
            Ok(())
        });
        assert!(task.resume().is_err());
    }
}
