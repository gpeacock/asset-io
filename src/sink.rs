//! Streaming observation: a [`Sink`] sees every byte of an asset, in order, as it
//! is written ([`Structure::write`]) or read ([`Structure::read`]).

use crate::error::{Error, Result};
use crate::range::ByteRange;
use crate::segment::Role;
use crate::structure::{SegmentId, Structure};
use std::io::{Read, Seek, SeekFrom, Write};

/// A run of bytes, borrowed from the buffer being read or written. No copy is made.
#[derive(Debug, Clone, Copy)]
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

/// Receives bytes during a read or a write.
///
/// Every byte the sink wants is delivered exactly once, in order, tagged with
/// its segment and role. The sink decides what the bytes mean; asset-io attaches
/// no hashing semantics to them. Return [`Error::UserCanceled`] from `chunk` to
/// stop the operation.
pub trait Sink {
    /// Called once before any bytes, with the complete layout of the asset being
    /// read or of the output being written.
    fn begin(&mut self, _layout: &Structure) -> Result<()> {
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

/// Writes to `out`, forwards to the sink, and checks the bytes land where the
/// destination layout says they should.
pub(crate) struct Emitter<'a, W: Write, S: Sink + ?Sized> {
    out: &'a mut W,
    sink: &'a mut S,
    dest: &'a Structure,
    pos: u64,
    cur: Option<SegmentId>,
}

impl<'a, W: Write, S: Sink + ?Sized> Emitter<'a, W, S> {
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
                    "output position diverged from planned layout (expected {})",
                    seg.extent().offset()
                ),
            });
        }
        self.cur = Some(id);
        Ok(())
    }

    /// Write bytes for the current segment.
    pub(crate) fn put(&mut self, role: Role, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let id = self
            .cur
            .ok_or_else(|| Error::InvalidFormat("write outside of a segment".into()))?;
        let seg = self.dest.segment(id)?;
        if self.sink.wants(seg, role) {
            self.sink.chunk(Chunk {
                segment: id,
                role,
                offset: self.pos,
                data,
            })?;
        }
        self.out.write_all(data)?;
        self.pos = self
            .pos
            .checked_add(data.len() as u64)
            .ok_or(Error::LimitExceeded {
                what: "output size",
                limit: u64::MAX,
            })?;
        Ok(())
    }

    /// Copy `range` of `src` into the current segment through a reusable buffer.
    /// The buffer is the only copy: the same slice goes to the sink and the writer.
    pub(crate) fn copy<R: Read + Seek>(
        &mut self,
        src: &mut R,
        role: Role,
        range: ByteRange,
        buf: &mut [u8],
    ) -> Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        src.seek(SeekFrom::Start(range.offset()))?;
        let mut remaining = range.len();
        while remaining > 0 {
            let n = remaining.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..n])?;
            self.put(role, &buf[..n])?;
            remaining -= n as u64;
        }
        Ok(())
    }

    /// Verify the whole destination layout was written.
    pub(crate) fn finish(self) -> Result<()> {
        if self.pos != self.dest.total_size() {
            return Err(Error::InvalidSegment {
                offset: self.pos,
                reason: format!(
                    "wrote {} bytes, planned layout is {}",
                    self.pos,
                    self.dest.total_size()
                ),
            });
        }
        self.out.flush()?;
        Ok(())
    }
}
