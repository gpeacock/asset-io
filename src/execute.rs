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

//! Running a [`WritePlan`] one piece of I/O at a time; see [`Execution`].

use crate::error::{Error, Result};
use crate::plan::{Relocate, Step, WritePlan};
use crate::range::ByteRange;
use crate::segment::Role;
use crate::sink::{Chunk, Sink};
use crate::structure::{SegmentId, Structure};
use std::mem;

/// What the caller of [`Execution::step`] must do before calling it again.
#[derive(Debug)]
#[non_exhaustive]
pub enum Action<'a> {
    /// Fill `buf` completely with the source asset's bytes from `offset`.
    ///
    /// A source that has fewer bytes than that is not the stream the plan was made
    /// from; report it as an error and stop.
    Read {
        /// Where in the source asset the bytes are.
        offset: u64,
        /// The buffer to fill. Its length is how many bytes are wanted.
        buf: &'a mut [u8],
    },
    /// Write these bytes to the output, which is written from its start, in order.
    Write(&'a [u8]),
    /// The plan has been carried out in full.
    Done,
}

/// How many copy buffers a run may span before it is flushed. Bounds the pieces
/// held at once, however many small segments the asset has.
const RUN_WINDOWS: u64 = 16;

/// Copy buffer for large writes. Measured on one machine (Apple M2, 128 MB PNGs): 64
/// and 128 KiB were slow in memory, 16 and 32 KiB slow from disk, 256 KiB good in
/// both. Not measured on other CPUs.
const WRITE_BUFFER: usize = 256 * 1024;

/// Smallest copy buffer. It must hold at least one entry of any offset table
/// ([`MAX_RELOCATE_STRIDE`](crate::plan::MAX_RELOCATE_STRIDE) bytes).
const MIN_WRITE_BUFFER: usize = 4096;

/// Size of the copy buffer for writing an output of `total` bytes. It is also the
/// read and write window of a run, so a small asset gets a buffer no larger than
/// itself and does not pay for a large allocation.
fn default_buffer_len(total: u64) -> usize {
    (total.min(WRITE_BUFFER as u64) as usize).max(MIN_WRITE_BUFFER)
}

/// One source region of a run, and the segment and role it lands in.
#[derive(Debug, Clone, Copy)]
struct Piece {
    dest: SegmentId,
    role: Role,
    range: ByteRange,
}

/// A run of source regions that are contiguous in the source and land in
/// consecutive output segments. Each window of it is read once, shown to the sink as
/// slices tagged with their segment and role, and written once, so reads and writes
/// are a function of the run's length, not of how many segments it crosses.
struct Run {
    pieces: Vec<Piece>,
    /// Source offset of the next window to read.
    at: u64,
    /// Bytes of the run not yet read.
    left: u64,
    /// The piece the next delivered byte belongs to, and how much of it is done.
    piece: usize,
    done: u64,
}

/// A table of offsets being copied and rewritten, block by block.
struct Table<'a> {
    role: Role,
    rule: &'a Relocate,
    at: u64,
    entries_left: u64,
    per_block: u64,
}

/// What a transition of the state machine asks for, as plain data, so that no
/// borrow of the machine outlives the transition.
enum Out<'a> {
    Continue,
    Read { offset: u64, len: usize },
    WriteBuf(usize),
    WriteData(&'a [u8]),
    Done,
}

enum State<'a> {
    Start,
    Idle,
    /// A read of `n` bytes of a run was handed out.
    RunRead(Run, usize),
    /// The write of a window of a run was handed out.
    RunWrite(Run),
    /// A read of `n` entries of a table was handed out.
    TableRead(Table<'a>, usize),
    /// The write of a block of a table was handed out.
    TableWrite(Table<'a>),
    Done,
}

/// Checks that every byte lands where the structure says, and shows it to the sink.
struct Tracker<'a> {
    structure: &'a Structure,
    pos: u64,
    cur: Option<SegmentId>,
}

impl Tracker<'_> {
    /// Start a segment. Fails if the output position has drifted from the plan.
    fn begin_segment(&mut self, id: SegmentId) -> Result<()> {
        let seg = self.structure.segment(id)?;
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
    fn enter_segment(&mut self, id: SegmentId) -> Result<()> {
        if self.cur == Some(id) {
            return Ok(());
        }
        self.begin_segment(id)
    }

    /// Show `data` to the sink as bytes of the current segment and advance the
    /// output position.
    fn deliver<S: Sink + ?Sized>(&mut self, sink: &mut S, role: Role, data: &[u8]) -> Result<()> {
        let id = self
            .cur
            .ok_or_else(|| Error::InvalidFormat("write outside of a segment".into()))?;
        let seg = self.structure.segment(id)?;
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
        if sink.wants(seg, role) {
            sink.chunk(Chunk::new(id, role, self.pos, data))?;
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

    /// Verify the whole structure was written.
    fn finish(&self) -> Result<()> {
        if self.pos != self.structure.total_size() {
            return Err(Error::InvalidSegment {
                offset: self.pos,
                reason: format!(
                    "wrote {} bytes, planned structure is {}",
                    self.pos,
                    self.structure.total_size()
                ),
            });
        }
        Ok(())
    }
}

/// A [`WritePlan`] being carried out, one piece of I/O at a time.
///
/// An `Execution` does no I/O. It tells its caller what to do next: read these
/// source bytes into this buffer, write these bytes to the output, or stop. The
/// caller performs that however it likes (blocking, `async`, from a state machine
/// of its own) and calls [`Execution::step`] again. The sink, the checking of every
/// byte against the plan's structure, and the coalescing of reads all happen
/// inside `step`, so every way of running a plan behaves the same.
///
/// ```no_run
/// # use asset_io::{Action, NullSink, WritePlan};
/// # use std::io::{Read, Seek, SeekFrom, Write};
/// # fn run(plan: &WritePlan<'_>, mut src: impl Read + Seek, mut out: impl Write)
/// #     -> asset_io::Result<()> {
/// let mut run = plan.start();
/// loop {
///     match run.step(&mut NullSink)? {
///         Action::Read { offset, buf } => {
///             src.seek(SeekFrom::Start(offset))?;
///             src.read_exact(buf)?;
///         }
///         Action::Write(bytes) => out.write_all(bytes)?,
///         Action::Done => break,
///         _ => unreachable!(),
///     }
/// }
/// # Ok(())
/// # }
/// ```
///
/// [`WritePlan::execute`] is that loop.
pub struct Execution<'a, 'p> {
    plan: &'a WritePlan<'p>,
    tracker: Tracker<'a>,
    next: usize,
    buf: Vec<u8>,
    state: State<'a>,
}

impl<'a, 'p> Execution<'a, 'p> {
    pub(crate) fn new(plan: &'a WritePlan<'p>) -> Self {
        let structure = plan.structure();
        Self {
            plan,
            tracker: Tracker {
                structure,
                pos: 0,
                cur: None,
            },
            next: 0,
            buf: vec![0u8; default_buffer_len(structure.total_size())],
            state: State::Start,
        }
    }

    /// Use a buffer (the most bytes one [`Action::Read`] asks for) of `len` bytes,
    /// at least 4 KiB. The default is 256 KiB, or the output size if that is
    /// smaller. Call it before the first [`step`](Self::step).
    pub fn with_buffer_len(mut self, len: usize) -> Self {
        self.buf = vec![0u8; len.max(MIN_WRITE_BUFFER)];
        self
    }

    /// Carry the plan one step further: do the sink's part, check the bytes against
    /// the plan's structure, and say what the caller must do next. Call it again
    /// when that is done. Errors are final.
    ///
    /// Every byte of the output reaches `sink` once, in order, tagged with its
    /// segment and role, before the [`Action::Write`] that carries it.
    pub fn step<S: Sink + ?Sized>(&mut self, sink: &mut S) -> Result<Action<'_>> {
        loop {
            return Ok(match self.advance(sink)? {
                Out::Continue => continue,
                Out::Read { offset, len } => Action::Read {
                    offset,
                    buf: &mut self.buf[..len],
                },
                Out::WriteBuf(len) => Action::Write(&self.buf[..len]),
                Out::WriteData(data) => Action::Write(data),
                Out::Done => Action::Done,
            });
        }
    }

    /// Move the state machine on by one transition.
    fn advance<S: Sink + ?Sized>(&mut self, sink: &mut S) -> Result<Out<'a>> {
        match mem::replace(&mut self.state, State::Idle) {
            State::Start => {
                sink.begin(self.plan.structure())?;
                Ok(Out::Continue)
            }
            State::Idle => self.begin_next(sink),
            State::RunRead(mut run, n) => {
                self.deliver_run(sink, &mut run, n)?;
                self.state = State::RunWrite(run);
                Ok(Out::WriteBuf(n))
            }
            State::RunWrite(run) => Ok(if run.left == 0 {
                Out::Continue
            } else {
                self.read_run(run)
            }),
            State::TableRead(table, n) => {
                let bytes = n * table.rule.stride() as usize;
                table.rule.apply(&mut self.buf[..bytes])?;
                self.tracker.deliver(sink, table.role, &self.buf[..bytes])?;
                self.state = State::TableWrite(table);
                Ok(Out::WriteBuf(bytes))
            }
            State::TableWrite(table) => Ok(if table.entries_left == 0 {
                Out::Continue
            } else {
                self.read_table(table)
            }),
            State::Done => {
                self.state = State::Done;
                Ok(Out::Done)
            }
        }
    }

    /// Look at the next step of the plan.
    fn begin_next<S: Sink + ?Sized>(&mut self, sink: &mut S) -> Result<Out<'a>> {
        let plan = self.plan;
        let Some(step) = plan.steps().get(self.next) else {
            self.tracker.finish()?;
            self.state = State::Done;
            return Ok(Out::Continue);
        };
        match step {
            Step::Copy { .. } => {
                let run = self.collect_run();
                match run.pieces.first() {
                    Some(first) => {
                        self.tracker.enter_segment(first.dest)?;
                        Ok(self.read_run(run))
                    }
                    None => Ok(Out::Continue),
                }
            }
            Step::Put { dest, role, data } => {
                self.next += 1;
                if data.is_empty() {
                    return Ok(Out::Continue);
                }
                self.tracker.enter_segment(*dest)?;
                self.tracker.deliver(sink, *role, data)?;
                Ok(Out::WriteData(data))
            }
            Step::Relocate {
                dest,
                role,
                src,
                rule,
            } => {
                self.next += 1;
                let stride = rule.stride() as u64;
                if !src.len().is_multiple_of(stride) {
                    return Err(Error::InvalidSegment {
                        offset: src.offset(),
                        reason: "offset table is not a whole number of entries".into(),
                    });
                }
                if src.is_empty() {
                    return Ok(Out::Continue);
                }
                self.tracker.enter_segment(*dest)?;
                let table = Table {
                    role: *role,
                    rule,
                    at: src.offset(),
                    entries_left: src.len() / stride,
                    per_block: (self.buf.len() as u64 / stride).max(1),
                };
                Ok(self.read_table(table))
            }
        }
    }

    /// Gather the copy steps from `next` on that form one run: contiguous in the
    /// source, and cut between segments once the run spans enough buffers.
    fn collect_run(&mut self) -> Run {
        let run_limit = (self.buf.len() as u64).saturating_mul(RUN_WINDOWS);
        let mut pieces: Vec<Piece> = Vec::new();
        let mut run_len = 0u64;
        while let Some(Step::Copy { dest, role, src }) = self.plan.steps().get(self.next) {
            if !src.is_empty() {
                if let Some(last) = pieces.last() {
                    let gap = last.range.end() != src.offset();
                    if gap || (last.dest != *dest && run_len >= run_limit) {
                        break;
                    }
                }
                run_len = run_len.saturating_add(src.len());
                pieces.push(Piece {
                    dest: *dest,
                    role: *role,
                    range: *src,
                });
            }
            self.next += 1;
        }
        Run {
            at: pieces.first().map_or(0, |p| p.range.offset()),
            left: run_len,
            pieces,
            piece: 0,
            done: 0,
        }
    }

    /// Ask for the next window of `run`.
    fn read_run(&mut self, mut run: Run) -> Out<'a> {
        let len = run.left.min(self.buf.len() as u64) as usize;
        let offset = run.at;
        run.at += len as u64;
        run.left -= len as u64;
        self.state = State::RunRead(run, len);
        Out::Read { offset, len }
    }

    /// Ask for the next block of `table`.
    fn read_table(&mut self, mut table: Table<'a>) -> Out<'a> {
        let n = table.entries_left.min(table.per_block) as usize;
        let offset = table.at;
        let len = n * table.rule.stride() as usize;
        table.at += len as u64;
        table.entries_left -= n as u64;
        self.state = State::TableRead(table, n);
        Out::Read { offset, len }
    }

    /// Show the `n` bytes of the buffer to the sink, as slices of the pieces they
    /// belong to.
    fn deliver_run<S: Sink + ?Sized>(
        &mut self,
        sink: &mut S,
        run: &mut Run,
        n: usize,
    ) -> Result<()> {
        let mut w = 0usize;
        while w < n {
            let p = run
                .pieces
                .get(run.piece)
                .ok_or_else(|| Error::InvalidFormat("copy run exhausted".into()))?;
            if run.done == 0 && self.tracker.cur != Some(p.dest) {
                self.tracker.begin_segment(p.dest)?;
            }
            let take = (p.range.len() - run.done).min((n - w) as u64) as usize;
            self.tracker.deliver(sink, p.role, &self.buf[w..w + take])?;
            w += take;
            run.done += take as u64;
            if run.done == p.range.len() {
                run.piece += 1;
                run.done = 0;
            }
        }
        Ok(())
    }
}
