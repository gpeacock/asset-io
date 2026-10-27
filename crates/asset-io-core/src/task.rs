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
use crate::io::{Io, Link};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

/// A suspended computation that reads a stream: run it with [`run`](Self::run), or
/// drive it yourself with [`resume`](Self::resume).
///
/// Code that reads is written once, as straight-line `async` code over an [`Io`]:
///
/// ```
/// # use asset_io_core::{Io, Progress, Result, Task};
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
        let io = Io::new(total, link.clone());
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
            Poll::Pending => match ReadRequest::take(&self.link) {
                Some(req) => Ok(Progress::Read(req)),
                None => {
                    self.finished = true;
                    Err(Error::InvalidFormat(
                        "task suspended on something other than a read".into(),
                    ))
                }
            },
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

/// Run code that reads a stream to completion on this thread, reading from `src`.
///
/// This is `Task::new(total, make).run(src)` for code that does not need to be
/// [`Send`]: it holds a callback that is not, say. The code is not boxed, and
/// nothing outlives the call.
pub fn block_on<R, T, F>(total: u64, src: &mut R, make: impl FnOnce(Io) -> F) -> Result<T>
where
    R: Read + Seek + ?Sized,
    F: Future<Output = Result<T>>,
{
    let link = Arc::new(Link::default());
    let io = Io::new(total, link.clone());
    let mut fut = std::pin::pin!(make(io));
    let mut cx = Context::from_waker(Waker::noop());
    let mut pos = None;
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(done) => return done,
            Poll::Pending => {
                let Some(req) = ReadRequest::take(&link) else {
                    return Err(Error::InvalidFormat(
                        "task suspended on something other than a read".into(),
                    ));
                };
                pos = Some(req.fill_from(src, pos)?);
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
    link: Arc<Link>,
    offset: u64,
    min_len: usize,
    window_len: usize,
    /// The window's buffer, handed to the caller until the request is dropped.
    buf: Vec<u8>,
    /// Keeps the task from resuming while the request is out.
    _task: PhantomData<&'a mut ()>,
}

impl Drop for ReadRequest<'_> {
    fn drop(&mut self) {
        // The task finds the buffer where it left it.
        self.link.lock().buf = std::mem::take(&mut self.buf);
    }
}

impl std::fmt::Debug for ReadRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadRequest")
            .field("offset", &self.offset)
            .field("min_len", &self.min_len)
            .field("window_len", &self.window_len)
            .finish()
    }
}

impl ReadRequest<'_> {
    /// The request a suspended task has made, with the buffer it left for it.
    fn take<'a>(link: &Arc<Link>) -> Option<ReadRequest<'a>> {
        let mut slot = link.lock();
        let (offset, min_len, window_len) = slot.request?;
        Some(ReadRequest {
            link: link.clone(),
            offset,
            min_len,
            window_len,
            buf: std::mem::take(&mut slot.buf),
            _task: PhantomData,
        })
    }

    /// Where in the stream the bytes are wanted.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// The fewest bytes the task can go on with.
    pub fn min_len(&self) -> usize {
        self.min_len
    }

    /// How many bytes the task would like: at least `min_len`, and never past the
    /// end of the stream.
    pub fn window_len(&self) -> usize {
        self.window_len
    }

    /// The buffer to fill. It starts empty; put the bytes in it, replacing what it
    /// holds. Its capacity is reused between requests. The request owns it, so a
    /// request can be held across an `.await`, and the task is given the buffer
    /// back when the request is dropped.
    pub fn buffer(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    /// Fill the buffer from `src`, a seekable stream: seek to the offset and read up
    /// to `window_len` bytes. The buffer is not zeroed first.
    pub fn fill<R: Read + Seek + ?Sized>(self, src: &mut R) -> Result<()> {
        self.fill_from(src, None).map(|_| ())
    }

    /// [`fill`](Self::fill), not seeking if `pos`, where the stream is now, is
    /// already the offset. Returns where the stream is afterwards.
    fn fill_from<R: Read + Seek + ?Sized>(mut self, src: &mut R, pos: Option<u64>) -> Result<u64> {
        let (offset, window) = (self.offset, self.window_len);
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
    use crate::io::DEFAULT_WINDOW;
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
    fn read_vec_of_nothing_is_empty_wherever_it_is() {
        let data = vec![1u8; 100];
        let t = Task::new(100, |mut io| async move {
            let first = io.read_vec(10, 50).await?; // takes the window
            let nothing = io.read_vec(100, 0).await?;
            let also = io.read_vec(40, 0).await?;
            Ok((first.len(), nothing, also))
        });
        let (n, nothing, also) = t.run(&mut Cursor::new(&data)).unwrap();
        assert_eq!(n, 50);
        assert!(nothing.is_empty() && also.is_empty());
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
    fn a_task_and_its_requests_are_send() {
        fn needs_send<T: Send>(_: &T) {}
        let mut t: Task<'_, ()> =
            Task::new(
                8,
                |mut io| async move { io.read_at(0, &mut [0u8; 4]).await },
            );
        needs_send(&t);
        // A request can be held across an `.await` in a spawned task.
        let Ok(Progress::Read(req)) = t.resume() else {
            panic!("expected a request");
        };
        needs_send(&req);
    }

    #[test]
    fn a_request_gives_the_buffer_back_when_dropped() {
        let mut task = Task::new(8_000, |mut io| async move {
            let mut b = [0u8; 4];
            io.read_at(0, &mut b).await?;
            io.read_at(5_000, &mut b).await?;
            Ok(b)
        });
        let data: Vec<u8> = (0..8_000u32).map(|i| i as u8).collect();
        let mut cap = 0;
        loop {
            match task.resume().unwrap() {
                Progress::Read(mut req) => {
                    let at = req.offset() as usize;
                    let to = (at + req.window_len()).min(data.len());
                    req.buffer().extend_from_slice(&data[at..to]);
                    cap = cap.max(req.buffer().capacity());
                }
                Progress::Done(b) => {
                    assert_eq!(b, [(5_000 % 256) as u8, 137, 138, 139]);
                    break;
                }
            }
        }
        assert!(cap >= 4096);
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
