//! Internal async I/O handles used by container code inside sans-IO machines.
//!
//! Container logic is written as ordinary `async fn`s over [`Src`] (source reads) and
//! [`Dst`]/[`PDst`] (destination writes and processing events). The futures never touch
//! real I/O: each leaf operation posts a request to the machine's channel and suspends
//! until the caller supplies a response via [`super::Machine`].

use super::{Chan, Chunk, RawReq};
use crate::error::{Error, Result};
use crate::segment::Segment;
use std::future::Future;
use std::io::SeekFrom;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Maximum read-ahead window for small metadata reads.
pub(crate) const READ_AHEAD: usize = 64 * 1024;
/// Initial read-ahead window; grows towards [`READ_AHEAD`] while reads stay sequential and
/// resets when the parser skips ahead (e.g. over large PNG/BMFF payloads), so header walks
/// don't fetch payload bytes they will never look at.
const READ_AHEAD_MIN: usize = 512;
/// Destination coalescing buffer size.
pub(crate) const WRITE_BUF: usize = 64 * 1024;
/// Writes at least this large bypass the coalescing buffer (avoids an extra copy).
const WRITE_DIRECT: usize = 8 * 1024;

struct IoFut<'b> {
    chan: &'b Chan,
    req: Option<RawReq>,
    _borrow: PhantomData<&'b mut [u8]>,
}

impl Future for IoFut<'_> {
    type Output = usize;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<usize> {
        if let Some(req) = self.req.take() {
            let mut st = self.chan.lock();
            st.resp = None;
            st.req = Some(req);
            return Poll::Pending;
        }
        match self.chan.lock().resp.take() {
            Some(n) => Poll::Ready(n),
            None => Poll::Pending,
        }
    }
}

/// Handle to the machine channel.
#[derive(Clone)]
pub(crate) struct Io(Arc<Chan>);

impl Io {
    pub(crate) fn new(chan: Arc<Chan>) -> Self {
        Self(chan)
    }

    fn fut<'b>(&'b self, req: RawReq) -> IoFut<'b> {
        IoFut {
            chan: &self.0,
            req: Some(req),
            _borrow: PhantomData,
        }
    }

    /// Request up to `buf.len()` source bytes at `offset`. Returns 0 at EOF.
    pub(crate) async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len();
        let n = self
            .fut(RawReq::Read {
                offset,
                ptr: buf.as_mut_ptr(),
                len,
            })
            .await;
        Ok(n.min(len))
    }

    /// Emit destination bytes at `offset`.
    pub(crate) async fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.fut(RawReq::Write {
            offset,
            ptr: data.as_ptr(),
            len: data.len(),
        })
        .await;
        Ok(())
    }

    /// Emit a processing event.
    pub(crate) async fn process(&self, chunk: Chunk<'_>) -> Result<()> {
        let req = match chunk {
            Chunk::Data(d) => RawReq::Data {
                ptr: d.as_ptr(),
                len: d.len(),
            },
            Chunk::Mdat {
                id,
                data,
                large_size,
            } => RawReq::Mdat {
                id,
                ptr: data.as_ptr(),
                len: data.len(),
                large_size,
            },
            Chunk::Boundary(seg) => RawReq::Boundary(seg as *const Segment),
        };
        self.fut(req).await;
        Ok(())
    }

    /// Set the expected total reported via [`super::Progress::total`].
    pub(crate) fn set_total(&self, total: Option<u64>) {
        self.0.lock().progress.total = total;
    }
}

fn eof() -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "failed to fill whole buffer",
    ))
}

/// Positioned source cursor with a read-ahead buffer.
///
/// Mirrors the subset of `Read + Seek` used by container code. Seeking is free (no I/O);
/// reads are served from the read-ahead window when possible so that many small header
/// reads turn into few, larger requests.
pub(crate) struct Src {
    io: Io,
    pos: u64,
    len: u64,
    buf: Vec<u8>,
    buf_off: u64,
    buf_len: usize,
    read_ahead: usize,
}

macro_rules! read_int {
    ($($name:ident => $ty:ty, $conv:ident;)*) => {
        $(
            #[allow(dead_code)]
            pub(crate) async fn $name(&mut self) -> Result<$ty> {
                let mut b = [0u8; std::mem::size_of::<$ty>()];
                self.read_exact(&mut b).await?;
                Ok(<$ty>::$conv(b))
            }
        )*
    };
}

#[allow(dead_code)]
impl Src {
    pub(crate) fn new(io: Io, len: u64) -> Self {
        Self {
            io,
            pos: 0,
            len,
            buf: Vec::new(),
            buf_off: 0,
            buf_len: 0,
            read_ahead: READ_AHEAD_MIN,
        }
    }

    pub(crate) fn io(&self) -> &Io {
        &self.io
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    pub(crate) fn stream_position(&mut self) -> Result<u64> {
        Ok(self.pos)
    }

    pub(crate) fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let new = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match new {
            Some(n) => {
                self.pos = n;
                Ok(n)
            }
            None => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            ))),
        }
    }

    /// Read without going through the read-ahead buffer (for large streaming copies).
    pub(crate) async fn read_direct(&mut self, out: &mut [u8]) -> Result<usize> {
        if out.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let want = (out.len() as u64).min(self.len - self.pos) as usize;
        let n = self.io.read_at(self.pos, &mut out[..want]).await?;
        self.pos += n as u64;
        Ok(n)
    }

    pub(crate) async fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        if out.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        if self.pos >= self.buf_off && self.pos < self.buf_off + self.buf_len as u64 {
            let start = (self.pos - self.buf_off) as usize;
            let n = out.len().min(self.buf_len - start);
            out[..n].copy_from_slice(&self.buf[start..start + n]);
            self.pos += n as u64;
            return Ok(n);
        }
        let sequential = self.buf_len > 0 && self.pos == self.buf_off + self.buf_len as u64;
        self.read_ahead = if sequential {
            (self.read_ahead * 2).min(READ_AHEAD)
        } else {
            READ_AHEAD_MIN
        };
        if out.len() >= self.read_ahead {
            return self.read_direct(out).await;
        }
        let want = (self.read_ahead as u64).min(self.len - self.pos) as usize;
        if self.buf.len() < want {
            self.buf.resize(want, 0);
        }
        self.buf_len = 0;
        let n = self.io.read_at(self.pos, &mut self.buf[..want]).await?;
        self.buf_off = self.pos;
        self.buf_len = n;
        if n == 0 {
            return Ok(0);
        }
        let n = out.len().min(n);
        out[..n].copy_from_slice(&self.buf[..n]);
        self.pos += n as u64;
        Ok(n)
    }

    pub(crate) async fn read_exact(&mut self, mut out: &mut [u8]) -> Result<()> {
        while !out.is_empty() {
            let n = self.read(out).await?;
            if n == 0 {
                return Err(eof());
            }
            out = &mut out[n..];
        }
        Ok(())
    }

    /// Like `read_exact` but bypasses the read-ahead buffer when possible.
    pub(crate) async fn read_exact_direct(&mut self, mut out: &mut [u8]) -> Result<()> {
        while !out.is_empty() {
            let n = if self.pos >= self.buf_off && self.pos < self.buf_off + self.buf_len as u64 {
                self.read(out).await?
            } else {
                self.read_direct(out).await?
            };
            if n == 0 {
                return Err(eof());
            }
            out = &mut out[n..];
        }
        Ok(())
    }

    pub(crate) async fn read_vec(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.read_exact(&mut v).await?;
        Ok(v)
    }

    /// Read until EOF (bounded by the source length).
    pub(crate) async fn read_to_end(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let start = out.len();
        let mut chunk = [0u8; 8192];
        loop {
            let n = self.read(&mut chunk).await?;
            if n == 0 {
                return Ok(out.len() - start);
            }
            out.extend_from_slice(&chunk[..n]);
        }
    }

    read_int! {
        read_u8 => u8, from_be_bytes;
        read_u16_be => u16, from_be_bytes;
        read_u16_le => u16, from_le_bytes;
        read_u32_be => u32, from_be_bytes;
        read_u32_le => u32, from_le_bytes;
        read_u64_be => u64, from_be_bytes;
        read_u64_le => u64, from_le_bytes;
        read_i32_be => i32, from_be_bytes;
    }
}

/// Destination writer with a coalescing buffer. Offsets are tracked locally.
pub(crate) struct Dst {
    io: Io,
    pos: u64,
    buf: Vec<u8>,
}

macro_rules! write_int {
    ($($name:ident => $ty:ty, $conv:ident;)*) => {
        $(
            #[allow(dead_code)]
            pub(crate) async fn $name(&mut self, v: $ty) -> Result<()> {
                self.write_all(&v.$conv()).await
            }
        )*
    };
}

#[allow(dead_code)]
impl Dst {
    pub(crate) fn new(io: Io) -> Self {
        Self {
            io,
            pos: 0,
            buf: Vec::new(),
        }
    }

    pub(crate) fn io(&self) -> &Io {
        &self.io
    }

    /// Logical write position (including buffered bytes).
    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    pub(crate) async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        if data.len() >= WRITE_DIRECT || self.buf.len() + data.len() > WRITE_BUF {
            self.flush().await?;
        }
        if data.len() >= WRITE_DIRECT {
            self.io.write_at(self.pos, data).await?;
        } else {
            self.buf.extend_from_slice(data);
        }
        self.pos += data.len() as u64;
        Ok(())
    }

    pub(crate) async fn flush(&mut self) -> Result<()> {
        if !self.buf.is_empty() {
            let off = self.pos - self.buf.len() as u64;
            self.io.write_at(off, &self.buf).await?;
            self.buf.clear();
        }
        Ok(())
    }

    /// Move the write position (flushes pending bytes first). Used for in-place updates.
    pub(crate) async fn seek_to(&mut self, pos: u64) -> Result<()> {
        self.flush().await?;
        self.pos = pos;
        Ok(())
    }

    write_int! {
        write_u8 => u8, to_be_bytes;
        write_u16_be => u16, to_be_bytes;
        write_u16_le => u16, to_le_bytes;
        write_u32_be => u32, to_be_bytes;
        write_u32_le => u32, to_le_bytes;
        write_u64_be => u64, to_be_bytes;
    }
}

/// Destination writer that also emits processing events
/// (processing events mirror what callers of `write_with_processing` observe).
///
/// When `process` is false, all processing events are suppressed so the same container
/// code serves both plain writes and write-with-processing.
pub(crate) struct PDst<'d> {
    dst: &'d mut Dst,
    process: bool,
    exclude: bool,
}

#[allow(dead_code)]
impl<'d> PDst<'d> {
    pub(crate) fn new(dst: &'d mut Dst, process: bool) -> Self {
        Self {
            dst,
            process,
            exclude: false,
        }
    }

    pub(crate) fn set_exclude_mode(&mut self, exclude: bool) {
        self.exclude = exclude;
    }

    pub(crate) fn is_exclude_mode(&self) -> bool {
        self.exclude
    }

    pub(crate) fn position(&self) -> u64 {
        self.dst.position()
    }

    pub(crate) fn stream_position(&mut self) -> Result<u64> {
        Ok(self.dst.position())
    }

    pub(crate) fn dst(&mut self) -> &mut Dst {
        self.dst
    }

    pub(crate) async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        if self.process && !self.exclude {
            self.dst.io().process(Chunk::Data(data)).await?;
        }
        self.dst.write_all(data).await
    }

    /// Like [`Self::write_all`], but emits processing events in `event_size` pieces while
    /// writing `data` as a single destination request (keeps event boundaries stable while
    /// using fewer, larger I/O round trips).
    pub(crate) async fn write_all_split(&mut self, data: &[u8], event_size: usize) -> Result<()> {
        if self.process && !self.exclude {
            for piece in data.chunks(event_size.max(1)) {
                self.dst.io().process(Chunk::Data(piece)).await?;
            }
        }
        self.dst.write_all(data).await
    }

    /// Hash an output offset as 8 big-endian bytes without writing it.
    pub(crate) async fn process_offset(&mut self, offset: u64) -> Result<()> {
        if self.process && !self.exclude {
            let b = offset.to_be_bytes();
            self.dst.io().process(Chunk::Data(&b)).await?;
        }
        Ok(())
    }

    /// Signal the start of a logical segment (fires regardless of exclude mode).
    pub(crate) async fn begin_segment(&mut self, segment: &Segment) -> Result<()> {
        if self.process {
            self.dst.io().process(Chunk::Boundary(segment)).await?;
        }
        Ok(())
    }

    /// Emit a typed chunk (fires regardless of exclude mode).
    pub(crate) async fn process_chunk(&mut self, chunk: Chunk<'_>) -> Result<()> {
        if self.process {
            self.dst.io().process(chunk).await?;
        }
        Ok(())
    }

    pub(crate) async fn flush(&mut self) -> Result<()> {
        self.dst.flush().await
    }

    write_int! {
        write_u8 => u8, to_be_bytes;
        write_u16_be => u16, to_be_bytes;
        write_u16_le => u16, to_le_bytes;
        write_u32_be => u32, to_be_bytes;
        write_u32_le => u32, to_le_bytes;
        write_u64_be => u64, to_be_bytes;
    }
}
