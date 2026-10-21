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

//! Streaming observation: a [`Sink`] sees every byte of an asset, in order, as it
//! is written ([`Structure::write`]) or read ([`Structure::read`]).

use crate::error::{Error, Result};
use crate::range::ByteRange;
use crate::segment::Role;
use crate::structure::{SegmentId, Structure};
use std::io::{Read, Seek, SeekFrom, Write};

/// A run of bytes, borrowed from the buffer being read or written. No copy is made.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Chunk<'a> {
    /// Segment these bytes belong to, in the structure given to [`Sink::begin`].
    pub segment: SegmentId,
    /// Header, payload or trailer.
    pub role: Role,
    /// Offset of `data[0]` in the asset.
    pub offset: u64,
    /// The bytes.
    pub data: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// A chunk of `data` at `offset` in the asset, belonging to `role` of `segment`.
    pub const fn new(segment: SegmentId, role: Role, offset: u64, data: &'a [u8]) -> Self {
        Self {
            segment,
            role,
            offset,
            data,
        }
    }
}

/// Receives bytes during a read or a write.
///
/// Every byte the sink wants is delivered exactly once, in order, tagged with
/// its segment and role. The sink decides what the bytes mean; asset-io attaches
/// no hashing semantics to them. Return [`Error::UserCanceled`] from `chunk` to
/// stop the operation.
pub trait Sink {
    /// Called once before any bytes, with the complete structure of the asset being
    /// read or of the output being written.
    fn begin(&mut self, _structure: &Structure) -> Result<()> {
        Ok(())
    }

    /// Return `false` to skip delivery of a run. A read does not fetch skipped
    /// runs; a write still writes them.
    fn wants(&self, _segment: &crate::segment::Segment, _role: Role) -> bool {
        true
    }

    /// Receive a run of output bytes.
    fn chunk(&mut self, chunk: Chunk<'_>) -> Result<()>;
}

/// A sink that ignores everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl Sink for NullSink {
    fn wants(&self, _: &crate::segment::Segment, _: Role) -> bool {
        false
    }
    fn chunk(&mut self, _: Chunk<'_>) -> Result<()> {
        Ok(())
    }
}

impl<F> Sink for F
where
    F: for<'a> FnMut(Chunk<'a>) -> Result<()>,
{
    fn chunk(&mut self, chunk: Chunk<'_>) -> Result<()> {
        self(chunk)
    }
}

/// One source region of a run for [`Emitter::copy_run`], and the segment and role it
/// lands in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Piece {
    pub(crate) dest: SegmentId,
    pub(crate) role: Role,
    pub(crate) range: ByteRange,
}

/// Writes to `out`, forwards to the sink, and checks the bytes land where the
/// destination structure says they should.
pub(crate) struct Emitter<'a, W: Write + ?Sized, S: Sink + ?Sized> {
    out: &'a mut W,
    sink: &'a mut S,
    dest: &'a Structure,
    pos: u64,
    cur: Option<SegmentId>,
}

impl<'a, W: Write + ?Sized, S: Sink + ?Sized> Emitter<'a, W, S> {
    pub(crate) fn new(out: &'a mut W, sink: &'a mut S, dest: &'a Structure) -> Self {
        Self {
            out,
            sink,
            dest,
            pos: 0,
            cur: None,
        }
    }

    /// Start a segment. Fails if the output position has drifted from the plan.
    pub(crate) fn begin_segment(&mut self, id: SegmentId) -> Result<()> {
        let seg = self.dest.segment(id)?;
        if self.pos != seg.extent().offset() {
            return Err(Error::InvalidSegment {
                offset: self.pos,
                reason: format!(
                    "output position diverged from planned structure (expected {})",
                    seg.extent().offset()
                ),
            });
        }
        self.cur = Some(id);
        Ok(())
    }

    /// Start `id` unless it is already the current segment, which a run split
    /// inside one segment leaves it.
    pub(crate) fn enter_segment(&mut self, id: SegmentId) -> Result<()> {
        if self.cur == Some(id) {
            return Ok(());
        }
        self.begin_segment(id)
    }

    /// Show `data` to the sink as bytes of the current segment and advance the
    /// output position. Writing the bytes to `out` is the caller's job.
    fn deliver(&mut self, role: Role, data: &[u8]) -> Result<()> {
        let id = self
            .cur
            .ok_or_else(|| Error::InvalidFormat("write outside of a segment".into()))?;
        let seg = self.dest.segment(id)?;
        let region = match role {
            Role::Header => seg.header(),
            Role::Payload => seg.payload(),
            Role::Trailer => seg.trailer(),
        };
        let end = self.pos.saturating_add(data.len() as u64);
        if self.pos < region.offset() || end > region.end() {
            return Err(Error::InvalidSegment {
                offset: self.pos,
                reason: format!(
                    "{} bytes do not lie in the {role:?} of their segment",
                    data.len()
                ),
            });
        }
        if self.sink.wants(seg, role) {
            self.sink.chunk(Chunk {
                segment: id,
                role,
                offset: self.pos,
                data,
            })?;
        }
        self.pos = self
            .pos
            .checked_add(data.len() as u64)
            .ok_or(Error::LimitExceeded {
                what: "output size",
                limit: u64::MAX,
            })?;
        Ok(())
    }

    /// Write bytes for the current segment.
    pub(crate) fn put(&mut self, role: Role, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.deliver(role, data)?;
        self.out.write_all(data)?;
        Ok(())
    }

    /// Copy a run of source regions that are contiguous in `src` and land in
    /// consecutive output segments. Each window of the run is read once into `buf`,
    /// shown to the sink as slices tagged with their segment and role, and written
    /// to `out` once. Reads and writes are then a function of the run's length, not
    /// of how many segments it crosses. Still two copies per byte (src to `buf` to
    /// `out`); the sink sees borrowed slices of `buf`.
    ///
    /// The caller has begun the first piece's segment; later segments are begun here.
    pub(crate) fn copy_run<R: Read + Seek + ?Sized>(
        &mut self,
        src: &mut R,
        pieces: &[Piece],
        buf: &mut [u8],
    ) -> Result<()> {
        let Some(first) = pieces.first() else {
            return Ok(());
        };
        if buf.is_empty() {
            return Err(Error::InvalidFormat("empty copy buffer".into()));
        }
        let start = first.range.offset();
        let mut end = start;
        for p in pieces {
            if p.range.offset() != end {
                return Err(Error::InvalidSegment {
                    offset: p.range.offset(),
                    reason: "copy run is not contiguous".into(),
                });
            }
            end = p.range.end();
        }

        src.seek(SeekFrom::Start(start))?;
        let mut left = end - start;
        let mut pi = 0usize;
        let mut done = 0u64; // bytes of pieces[pi] already delivered
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..n])?;
            let mut w = 0usize;
            while w < n {
                let p = pieces
                    .get(pi)
                    .ok_or_else(|| Error::InvalidFormat("copy run exhausted".into()))?;
                if done == 0 && self.cur != Some(p.dest) {
                    self.begin_segment(p.dest)?;
                }
                let take = (p.range.len() - done).min((n - w) as u64) as usize;
                self.deliver(p.role, &buf[w..w + take])?;
                w += take;
                done += take as u64;
                if done == p.range.len() {
                    pi += 1;
                    done = 0;
                }
            }
            self.out.write_all(&buf[..n])?;
            left -= n as u64;
        }
        Ok(())
    }

    /// Verify the whole destination structure was written.
    pub(crate) fn finish(self) -> Result<()> {
        if self.pos != self.dest.total_size() {
            return Err(Error::InvalidSegment {
                offset: self.pos,
                reason: format!(
                    "wrote {} bytes, planned structure is {}",
                    self.pos,
                    self.dest.total_size()
                ),
            });
        }
        self.out.flush()?;
        Ok(())
    }
}

/// How many copy buffers a run may span before it is flushed. Bounds the pieces a
/// writer holds, however many small segments the asset has.
pub(crate) const RUN_WINDOWS: u64 = 16;

/// Copy buffer for large writes. Measured on one machine (Apple M2, 128 MB PNGs): 64
/// and 128 KiB were slow in memory, 16 and 32 KiB slow from disk, 256 KiB good in
/// both. Not measured on other CPUs.
const WRITE_BUFFER: usize = 256 * 1024;

/// Smallest copy buffer.
const MIN_WRITE_BUFFER: usize = 4096;

/// Size of the copy buffer for writing an output of `total` bytes. It is also the
/// read and write window of a run, so a small asset gets a buffer no larger than
/// itself and does not pay for a large allocation.
pub(crate) fn write_buffer_len(total: u64) -> usize {
    (total.min(WRITE_BUFFER as u64) as usize).max(MIN_WRITE_BUFFER)
}

/// Copy the pending run, if any, and clear it.
pub(crate) fn flush_run<R, W, S>(
    em: &mut Emitter<'_, W, S>,
    src: &mut R,
    pieces: &mut Vec<Piece>,
    buf: &mut [u8],
) -> Result<()>
where
    R: Read + Seek + ?Sized,
    W: Write + ?Sized,
    S: Sink + ?Sized,
{
    if let Some(first) = pieces.first() {
        em.enter_segment(first.dest)?;
        em.copy_run(src, pieces, buf)?;
    }
    pieces.clear();
    Ok(())
}
