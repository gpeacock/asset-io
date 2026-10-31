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

//! Assembling a [`WritePlan`] segment by segment.

use crate::error::{Error, Result};
use crate::item::{Item, ItemInfo};
use crate::plan::{Relocate, Step, WritePlan};
use crate::range::ByteRange;
use crate::segment::{Name, Role, SegmentKind};
use crate::structure::{Attached, SegmentId, Structure, StructureBuilder, MAX_SEGMENTS};
use crate::transfer::TransferEncoding;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A segment of the output: its shape, and the source segment it is made from.
struct Shape {
    kind: SegmentKind,
    name: Name,
    header_len: u32,
    payload_len: u64,
    trailer_len: u16,
    subdivides: bool,
    from: Option<SegmentId>,
}

/// An item of the output. An extent is a segment of the output and, for an item
/// carried over from the source, the source range it covers; without one it is the
/// segment's whole payload.
struct ItemSpec {
    info: ItemInfo,
    extents: Vec<(SegmentId, Option<ByteRange>)>,
}

/// Builds the [`WritePlan`] for a write that rearranges the segments of a source
/// asset: some copied, some new.
///
/// A handler decides what the output holds, then describes it one segment at a time
/// and in output order. The builder does the bookkeeping that every such write
/// needs:
///
/// - the size of the output, and the offset of each segment;
/// - the output [`Structure`], including where the container's own structure ends
///   and where each sub-asset starts, which follow the source segments that began
///   them;
/// - items, including those of the source that survive, wherever their segments
///   land;
/// - the steps.
///
/// A segment is one of
///
/// - [`copy_segment`](Self::copy_segment): a source segment, unchanged;
/// - [`put_segment`](Self::put_segment): a new segment whose bytes the handler has;
/// - [`shape`](Self::shape) or [`shape_like`](Self::shape_like): only its size, with
///   the steps added by [`copy`](Self::copy), [`put`](Self::put) and
///   [`relocate`](Self::relocate), for output that is part copied and part changed.
///   Without any it has no steps, which suits a plan that is wanted only for its
///   structure.
///
/// # Example
///
/// Copy a source, dropping its first segment and appending a new one:
///
/// ```
/// use asset_io_core::{Name, PlanBuilder, SegmentId, SegmentKind, Structure};
///
/// # fn plan(src: &Structure) -> asset_io_core::Result<()> {
/// let mut b = PlanBuilder::new(src);
/// for i in 1..src.segments().len() {
///     b.copy_segment(SegmentId::from_index(i)?)?;
/// }
/// let id = b.put_segment(
///     SegmentKind::Other,
///     Name::None,
///     &[][..],
///     &b"new data"[..],
///     &[][..],
/// )?;
/// b.carry_items(|_| true);
/// let plan = b.finish()?;
/// # let _ = (id, plan);
/// # Ok(())
/// # }
/// ```
pub struct PlanBuilder<'s, 'a> {
    src: &'s Structure,
    shapes: Vec<Shape>,
    steps: Vec<Step<'a>>,
    items: Vec<ItemSpec>,
    /// For each source segment, the output segment it became.
    dest_of: Vec<Option<SegmentId>>,
    len: u64,
    attachment: Option<Arc<dyn Attached>>,
}

impl<'s, 'a> PlanBuilder<'s, 'a> {
    /// Start a plan that writes `src`, rearranged.
    pub fn new(src: &'s Structure) -> Self {
        Self {
            src,
            shapes: Vec::with_capacity(src.segments().len() + 2),
            steps: Vec::new(),
            items: Vec::new(),
            dest_of: vec![None; src.segments().len()],
            len: 0,
            attachment: None,
        }
    }

    /// Attach `value` to the output structure, as
    /// [`StructureBuilder::attach`](crate::StructureBuilder::attach) does to a parsed one,
    /// so a write's result can be read and written again without parsing it.
    pub fn attach<T: std::any::Any + std::fmt::Debug + Send + Sync>(
        &mut self,
        value: T,
    ) -> &mut Self {
        self.attachment = Some(Arc::new(value));
        self
    }

    /// The source structure.
    pub fn source(&self) -> &'s Structure {
        self.src
    }

    /// How many bytes the output has so far.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the output has no segments yet.
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// For each source segment, the output segment it became (see
    /// [`dest_of`](Self::dest_of)).
    pub fn dest_map(&self) -> &[Option<SegmentId>] {
        &self.dest_of
    }

    /// The output segment that source segment `src` became, if it has been copied or
    /// shaped like it.
    pub fn dest_of(&self, src: SegmentId) -> Option<SegmentId> {
        self.dest_of.get(src.index()).copied().flatten()
    }

    fn add(&mut self, shape: Shape) -> Result<SegmentId> {
        if self.shapes.len() >= MAX_SEGMENTS {
            return Err(Error::LimitExceeded {
                what: "segments",
                limit: MAX_SEGMENTS as u64,
            });
        }
        let id = SegmentId::from_index(self.shapes.len())?;
        self.len = (shape.header_len as u64)
            .checked_add(shape.payload_len)
            .and_then(|v| v.checked_add(shape.trailer_len as u64))
            .and_then(|v| self.len.checked_add(v))
            .ok_or(Error::LimitExceeded {
                what: "output size",
                limit: u64::MAX,
            })?;
        self.shapes.push(shape);
        Ok(id)
    }

    /// Add a segment of the given size, with no steps yet. The segment holds no
    /// bytes until [`copy`](Self::copy), [`put`](Self::put) or
    /// [`relocate`](Self::relocate) supply them, in output order.
    pub fn shape(
        &mut self,
        kind: SegmentKind,
        name: Name,
        header_len: u32,
        payload_len: u64,
        trailer_len: u16,
    ) -> Result<SegmentId> {
        self.add(Shape {
            kind,
            name,
            header_len,
            payload_len,
            trailer_len,
            subdivides: false,
            from: None,
        })
    }

    /// Add a segment shaped like source segment `src`, with no steps yet. It takes
    /// the place of `src` for items, sub-assets and
    /// [`dest_of`](Self::dest_of).
    pub fn shape_like(&mut self, src: SegmentId) -> Result<SegmentId> {
        let s = self.src.segment(src)?;
        let id = self.add(Shape {
            kind: s.kind(),
            name: s.name(),
            header_len: s.header().len() as u32,
            payload_len: s.payload().len(),
            trailer_len: s.trailer().len() as u16,
            subdivides: s.subdivides(),
            from: Some(src),
        })?;
        // The first output segment made from a source segment stands for it.
        let slot = &mut self.dest_of[src.index()];
        if slot.is_none() {
            *slot = Some(id);
        }
        Ok(id)
    }

    /// Add source segment `src`, copied as it is.
    pub fn copy_segment(&mut self, src: SegmentId) -> Result<SegmentId> {
        let id = self.shape_like(src)?;
        for (role, range) in self.src.segment(src)?.regions() {
            self.copy(id, role, range);
        }
        Ok(id)
    }

    /// Add a new segment from the bytes of its header, payload and trailer. Empty
    /// ones get no step.
    pub fn put_segment(
        &mut self,
        kind: SegmentKind,
        name: Name,
        header: impl Into<Cow<'a, [u8]>>,
        payload: impl Into<Cow<'a, [u8]>>,
        trailer: impl Into<Cow<'a, [u8]>>,
    ) -> Result<SegmentId> {
        let (header, payload, trailer) = (header.into(), payload.into(), trailer.into());
        let too_long = |what, limit| Error::LimitExceeded { what, limit };
        let id = self.shape(
            kind,
            name,
            u32::try_from(header.len()).map_err(|_| too_long("header size", u32::MAX as u64))?,
            payload.len() as u64,
            u16::try_from(trailer.len()).map_err(|_| too_long("trailer size", u16::MAX as u64))?,
        )?;
        for (role, data) in [
            (Role::Header, header),
            (Role::Payload, payload),
            (Role::Trailer, trailer),
        ] {
            if !data.is_empty() {
                self.put(id, role, data);
            }
        }
        Ok(id)
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

    /// Declare an item made of the payloads of whole output segments, in order.
    pub fn item(&mut self, info: ItemInfo, ids: &[SegmentId]) -> Result<()> {
        if ids.is_empty() {
            return Err(Error::InvalidFormat(
                "item needs at least one extent".into(),
            ));
        }
        if ids.iter().any(|id| id.index() >= self.shapes.len()) {
            return Err(Error::NotFound { what: "segment" });
        }
        self.items.push(ItemSpec {
            info,
            extents: ids.iter().map(|&id| (id, None)).collect(),
        });
        Ok(())
    }

    /// Declare the items of the source that `keep` accepts and whose segments were
    /// all copied or shaped like a source segment. Each covers the same bytes as
    /// before, wherever they now are; one with a segment that is gone is dropped.
    pub fn carry_items(&mut self, keep: impl Fn(&Item) -> bool) {
        for e in self.src.items() {
            if !keep(e) {
                continue;
            }
            let extents: Option<Vec<_>> = e
                .extents()
                .map(|p| {
                    self.dest_of
                        .get(p.segment().index())
                        .copied()
                        .flatten()
                        .map(|d| (d, Some(p.range())))
                })
                .collect();
            if let Some(extents) = extents {
                self.items.push(ItemSpec {
                    info: e.info().clone(),
                    extents,
                });
            }
        }
    }

    /// Build the plan.
    ///
    /// The container's own structure is taken to end where the source's does, and a
    /// sub-asset to start where its source segment landed. Fails if the segment
    /// that began either was not carried over, or if the result is not a valid
    /// structure.
    pub fn finish(self) -> Result<WritePlan<'a>> {
        let src = self.src;
        let first_of = |i: usize| -> Result<usize> {
            self.dest_of
                .get(i)
                .copied()
                .flatten()
                .map(SegmentId::index)
                .ok_or(Error::InvalidFormat(
                    "a segment that starts part of the structure was dropped".into(),
                ))
        };
        let trailing = match src.box_count() {
            k if k < src.segments().len() => Some(first_of(k)?),
            _ => None,
        };
        let mut subs = BTreeMap::new();
        for sub in src.subassets().iter().skip(1) {
            subs.insert(
                first_of(sub.segments().start)?,
                sub.media_type().map(|m| Cow::Owned(m.to_owned())),
            );
        }

        let mut b =
            StructureBuilder::new(src.handler().clone(), src.media_type().to_owned(), self.len);
        b.attach_shared(self.attachment.clone());
        for (i, s) in self.shapes.iter().enumerate() {
            if trailing == Some(i) {
                b.end_boxes()?;
            }
            if let Some(media) = subs.remove(&i) {
                b.begin_subasset(media)?;
            }
            let id = b.push_span(s.kind, s.name, s.header_len, s.payload_len, s.trailer_len)?;
            if s.subdivides {
                b.mark_subdivides(id)?;
                if let Some(from) = s.from {
                    b.set_origin(id, src.segment(from)?.range().offset());
                }
            }
        }
        if !subs.is_empty() {
            return Err(Error::InvalidFormat(
                "a sub-asset starts after the last segment".into(),
            ));
        }

        for e in self.items {
            let first = e.extents.first().map(|&(dest, _)| dest);
            let info = match first {
                Some(dest) if e.info.lacks_jumbf_type() => {
                    let from = self.shapes[dest.index()].from;
                    let ty =
                        jumbf_type_of(src, &self.steps, dest, from, e.info.transfer_encoding());
                    e.info.jumbf_type(ty)
                }
                _ => e.info,
            };
            let mut parts = Vec::with_capacity(e.extents.len());
            for (dest, carried) in e.extents {
                let at = b.segment(dest)?.range().offset();
                let range = match carried {
                    None => b.segment(dest)?.payload(),
                    Some(r) => {
                        let from = self.shapes[dest.index()].from.ok_or(Error::NotFound {
                            what: "source segment",
                        })?;
                        let moved = at + (r.offset() - src.segment(from)?.range().offset());
                        ByteRange::new(moved, r.len())?
                    }
                };
                parts.push((dest, range));
            }
            b.item(info, &parts)?;
        }
        Ok(WritePlan::from_parts(b.finish()?, self.steps))
    }
}

/// The superbox type of a JUMBF item that starts in output segment `dest`, so a
/// planned structure knows it without the output existing: from the source item
/// in the segment `dest` was made from, or else the bytes first put in its payload.
/// `None` when neither tells.
fn jumbf_type_of(
    src: &Structure,
    steps: &[Step<'_>],
    dest: SegmentId,
    from: Option<SegmentId>,
    transfer: &TransferEncoding,
) -> Option<[u8; 16]> {
    const STORED_HEAD: usize = 4096;
    if let Some(from) = from {
        return src
            .items_of(crate::ItemKind::Jumbf)
            .find(|item| item.extents().next().is_some_and(|x| x.segment() == from))
            .and_then(Item::jumbf_type);
    }
    match steps
        .iter()
        .find(|s| s.dest() == dest && s.role() == Role::Payload)?
    {
        Step::Put { data, .. } => {
            let stored = &data[..data.len().min(STORED_HEAD)];
            let head = transfer.decode_prefix(stored, 32).ok()?;
            crate::jumbf::superbox_type(&head)
        }
        _ => None,
    }
}
