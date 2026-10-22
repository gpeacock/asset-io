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

//! Declarative write plans.
//!
//! A handler describes a write as data: the structure of the output and an ordered
//! list of [`Step`]s that produce it. It does not touch the output stream or the
//! sink. [`WritePlan::execute`] is the reference executor: it runs the steps
//! synchronously, coalesces reads, delivers every byte to the sink tagged with its
//! segment and role, and checks that the bytes land where the structure says. A caller
//! can run the same plan some other way (asynchronously, or from a state machine
//! that does no I/O itself), because the plan is plain data.

use crate::error::{Error, Result};
use crate::execute::{Action, Execution};
use crate::range::ByteRange;
use crate::segment::{Role, Segment};
use crate::sink::Sink;
use crate::structure::{SegmentId, Structure};
use crate::task::Task;
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

/// Where bytes moved: `(end of the old region, size change)` for each edit.
///
/// A file offset that points at or past the end of an edited region moves by that
/// edit's size change. Containers that store absolute file offsets (ISO BMFF chunk
/// tables, for one) use it to say how those offsets change when an edit shifts the
/// data after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffsetMap {
    edits: Vec<(u64, i64)>,
}

impl OffsetMap {
    /// A map that moves nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the region of the source that ended at `old_end` changed size
    /// by `delta` bytes. A zero change is not recorded.
    pub fn add(&mut self, old_end: u64, delta: i64) {
        if delta != 0 {
            self.edits.push((old_end, delta));
        }
    }

    /// Whether no offset moves.
    pub fn is_identity(&self) -> bool {
        self.edits.is_empty()
    }

    /// Where the source offset `v` is in the output.
    pub fn apply(&self, v: u64) -> Result<u64> {
        let shift: i128 = self
            .edits
            .iter()
            .filter(|(end, _)| *end <= v)
            .map(|(_, d)| *d as i128)
            .sum();
        u64::try_from(v as i128 + shift)
            .map_err(|_| Error::InvalidFormat("offset adjustment out of range".into()))
    }
}

/// Largest entry [`Relocate`] handles.
pub const MAX_RELOCATE_STRIDE: u32 = 4096;

/// How the bytes of a [`Step::Relocate`] are rewritten: they are a table of
/// `stride`-byte entries, each holding a big-endian file offset `width` bytes
/// wide (4 or 8) at `field_off`, and every offset is replaced by its position in
/// the output according to an [`OffsetMap`]. The rest of each entry is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocate {
    map: Arc<OffsetMap>,
    stride: u32,
    field_off: u32,
    width: u8,
}

impl Relocate {
    /// A rewrite rule. Fails if `width` is not 4 or 8, the field does not fit in an
    /// entry, or the entry is empty or longer than [`MAX_RELOCATE_STRIDE`].
    pub fn new(map: Arc<OffsetMap>, stride: u32, field_off: u32, width: u8) -> Result<Self> {
        let fits = field_off
            .checked_add(width as u32)
            .is_some_and(|end| end <= stride);
        if !matches!(width, 4 | 8) || stride == 0 || stride > MAX_RELOCATE_STRIDE || !fits {
            return Err(Error::InvalidFormat("invalid offset table geometry".into()));
        }
        Ok(Self {
            map,
            stride,
            field_off,
            width,
        })
    }

    /// Bytes per entry.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Rewrite the offset in each whole entry of `block`.
    pub(crate) fn apply(&self, block: &mut [u8]) -> Result<()> {
        let (off, w) = (self.field_off as usize, self.width as usize);
        for entry in block.chunks_exact_mut(self.stride as usize) {
            let f = &mut entry[off..off + w];
            let v = self
                .map
                .apply(f.iter().fold(0u64, |a, &b| (a << 8) | b as u64))?;
            let range = || Error::InvalidFormat("offset adjustment does not fit its field".into());
            match w {
                4 => f.copy_from_slice(&u32::try_from(v).map_err(|_| range())?.to_be_bytes()),
                _ => f.copy_from_slice(&v.to_be_bytes()),
            }
        }
        Ok(())
    }
}

/// One piece of the output.
///
/// Steps are executed in order and must tile the output: each step supplies the
/// next bytes of segment `dest`, in the `role` (header, payload or trailer) of that
/// segment's structure, and the segments follow one another as the structure lists them.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Step<'a> {
    /// Copy bytes of the source asset.
    Copy {
        /// The output segment the bytes belong to.
        dest: SegmentId,
        /// Which part of that segment they are.
        role: Role,
        /// The source bytes.
        src: ByteRange,
    },
    /// Write bytes the handler supplies.
    Put {
        /// The output segment the bytes belong to.
        dest: SegmentId,
        /// Which part of that segment they are.
        role: Role,
        /// The bytes.
        data: Cow<'a, [u8]>,
    },
    /// Copy bytes of the source asset that are a table of file offsets, rewriting
    /// the offsets. The source range is a whole number of entries.
    Relocate {
        /// The output segment the bytes belong to.
        dest: SegmentId,
        /// Which part of that segment they are.
        role: Role,
        /// The source bytes.
        src: ByteRange,
        /// How the offsets change.
        rule: Relocate,
    },
}

impl Step<'_> {
    /// The output segment this step writes to.
    pub fn dest(&self) -> SegmentId {
        match self {
            Step::Copy { dest, .. } | Step::Put { dest, .. } | Step::Relocate { dest, .. } => *dest,
        }
    }

    /// Which part of the output segment this step writes.
    pub fn role(&self) -> Role {
        match self {
            Step::Copy { role, .. } | Step::Put { role, .. } | Step::Relocate { role, .. } => *role,
        }
    }

    /// How many output bytes this step produces.
    pub fn len(&self) -> u64 {
        match self {
            Step::Copy { src, .. } | Step::Relocate { src, .. } => src.len(),
            Step::Put { data, .. } => data.len() as u64,
        }
    }

    /// Whether this step produces no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A write, described but not performed.
///
/// Holds the [`Structure`] of the output and the [`Step`]s that produce it.
/// Borrows from the [`Updates`](crate::Updates) it was made from, so the new
/// metadata is not copied.
#[derive(Debug)]
pub struct WritePlan<'a> {
    structure: Structure,
    steps: Vec<Step<'a>>,
}

impl<'a> WritePlan<'a> {
    /// A plan for the output `structure`, with no steps yet.
    pub fn new(structure: Structure) -> Self {
        Self {
            structure,
            steps: Vec::new(),
        }
    }

    /// Append a step that copies `src` of the source asset into `role` of `dest`.
    pub fn copy(&mut self, dest: SegmentId, role: Role, src: ByteRange) -> &mut Self {
        self.steps.push(Step::Copy { dest, role, src });
        self
    }

    /// Append a step that writes `data` as `role` of `dest`.
    pub fn put(
        &mut self,
        dest: SegmentId,
        role: Role,
        data: impl Into<Cow<'a, [u8]>>,
    ) -> &mut Self {
        self.steps.push(Step::Put {
            dest,
            role,
            data: data.into(),
        });
        self
    }

    /// Append a step that copies `src` of the source asset into `role` of `dest`,
    /// rewriting the file offsets in it as `rule` says.
    pub fn relocate(
        &mut self,
        dest: SegmentId,
        role: Role,
        src: ByteRange,
        rule: Relocate,
    ) -> &mut Self {
        self.steps.push(Step::Relocate {
            dest,
            role,
            src,
            rule,
        });
        self
    }

    /// Append the steps that copy the source segment `src` unchanged into `dest`.
    pub fn copy_segment(&mut self, dest: SegmentId, src: &Segment) -> &mut Self {
        for (role, range) in src.regions() {
            self.copy(dest, role, range);
        }
        self
    }

    /// The structure of the output.
    pub fn structure(&self) -> &Structure {
        &self.structure
    }

    /// The steps, in output order.
    pub fn steps(&self) -> &[Step<'a>] {
        &self.steps
    }

    /// The structure of the output, discarding the steps.
    pub fn into_structure(self) -> Structure {
        self.structure
    }

    /// Start carrying the plan out, step by step, with the I/O left to the caller;
    /// see [`Execution`].
    pub fn start(&self) -> Execution<'_, 'a> {
        Execution::new(self)
    }

    /// Perform the write: run every step against `src`, write the bytes to `out`
    /// and deliver them to `sink`. `out` is written from its current position;
    /// offsets reported to the sink are relative to the first byte written.
    ///
    /// Fails, and may leave partial output, if the steps do not produce exactly
    /// the structure: bytes at the wrong offset, in the wrong role, or too few or too
    /// many of them.
    ///
    /// This drives [`start`](Self::start) with blocking reads and writes.
    pub fn execute<R, W, S>(&self, src: &mut R, out: &mut W, sink: &mut S) -> Result<()>
    where
        R: Read + Seek + ?Sized,
        W: Write + ?Sized,
        S: Sink + ?Sized,
    {
        let mut run = self.start();
        // Where the source stream is, so that a read that continues the last one
        // does not seek.
        let mut at: Option<u64> = None;
        loop {
            match run.step(sink)? {
                Action::Read { offset, buf } => {
                    if at != Some(offset) {
                        src.seek(SeekFrom::Start(offset))?;
                    }
                    src.read_exact(buf)?;
                    at = Some(offset + buf.len() as u64);
                }
                Action::Write(bytes) => out.write_all(bytes)?,
                Action::Done => break,
            }
        }
        out.flush()?;
        Ok(())
    }
}

impl Structure {
    /// The plan for writing this asset with `updates`; see [`WritePlan`]. `src` must
    /// be the stream this structure was parsed from. The container's handler builds
    /// it, and reports [`Error::UnsupportedFormat`] if it describes its writes some
    /// other way.
    pub fn write_plan<'a>(
        &self,
        src: &mut dyn crate::handler::ReadSeek,
        updates: &'a crate::Updates,
    ) -> Result<WritePlan<'a>> {
        self.handler().write_plan(self, src, updates)
    }
}

impl Structure {
    /// A task that builds the plan for writing this asset with `updates`, for a
    /// caller that does its own I/O; see [`Task`]. The task reads the source (a
    /// stream of [`total_size`](Self::total_size) bytes), which it asks for as it
    /// needs it. The result is the plan [`write_plan`](Self::write_plan) returns.
    pub fn write_plan_task<'f, 'a: 'f>(
        &'f self,
        updates: &'a crate::Updates,
    ) -> Task<'f, WritePlan<'a>> {
        Task::new(self.total_size(), move |io| {
            self.handler().write_plan_async(self, io, updates)
        })
    }
}

/// Checks a plan's steps against its structure without performing any I/O.
///
/// Useful in tests of a handler: the steps must tile the structure exactly, in the
/// structure's segment and role order, and copy only from inside `source_len` bytes.
pub fn validate(plan: &WritePlan<'_>, source_len: u64) -> Result<()> {
    let structure = plan.structure();
    let mut pos = 0u64;
    for step in plan.steps() {
        let seg = structure.segment(step.dest())?;
        if step.is_empty() {
            continue;
        }
        let region = match step.role() {
            Role::Header => seg.header(),
            Role::Payload => seg.payload(),
            Role::Trailer => seg.trailer(),
        };
        let end = pos.checked_add(step.len());
        let fits = region.offset() <= pos && end.is_some_and(|e| e <= region.end());
        if !fits {
            return Err(Error::InvalidSegment {
                offset: pos,
                reason: format!(
                    "step of {} bytes does not fit the {:?} of its segment",
                    step.len(),
                    step.role()
                ),
            });
        }
        if let Step::Relocate { src, rule, .. } = step {
            if !src.len().is_multiple_of(rule.stride() as u64) {
                return Err(Error::InvalidSegment {
                    offset: src.offset(),
                    reason: "offset table is not a whole number of entries".into(),
                });
            }
        }
        if let Step::Copy { src, .. } | Step::Relocate { src, .. } = step {
            if src.end() > source_len {
                return Err(Error::InvalidSegment {
                    offset: src.offset(),
                    reason: "copy reads past the end of the source".into(),
                });
            }
        }
        pos += step.len();
    }
    if pos != structure.total_size() {
        return Err(Error::InvalidSegment {
            offset: pos,
            reason: format!(
                "steps write {pos} bytes, structure is {}",
                structure.total_size()
            ),
        });
    }
    Ok(())
}
