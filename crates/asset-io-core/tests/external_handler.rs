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

//! A handler written outside the crate, with only its public API: it parses, and
//! it writes by describing a plan. If this file stops compiling, a third party's
//! handler would too.

use asset_io_core::plan::validate;
use asset_io_core::testing::Conformance;
use asset_io_core::{
    BoxFuture, ByteRange, Chunk, ContainerHandler, Error, Existing, FormatSpec, HandlerRegistry,
    Io, ItemInfo, ItemKind, MetadataSlot, Name, PlanBuilder, Result, Role, SegmentId, SegmentKind,
    Step, Structure, StructureBuilder, Updates, WritePlan,
};
use std::io::Cursor;
use std::sync::Arc;

/// `TOY!`, then records of `[kind][len][len bytes]`. Kind `X` is XMP.
#[derive(Debug)]
struct Toy;

impl ContainerHandler for Toy {
    fn id(&self) -> &'static str {
        "toy"
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[FormatSpec::new("application/x-toy", "toy")];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.starts_with(b"TOY!")
    }

    fn parse_async(self: Arc<Self>, mut io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async move {
            // Read the whole stream; a real handler reads only what it needs.
            let data = io.read_vec(0, io.len() as usize).await?;
            parse_toy(self, &data)
        })
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        src: &'f Structure,
        _io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move { write_toy_plan(src, updates) })
    }
}

fn write_toy_plan<'a>(src: &Structure, updates: &'a Updates) -> Result<WritePlan<'a>> {
    // Describe the output one segment at a time, in order.
    let mut b = PlanBuilder::new(src);
    let mut xmp = MetadataSlot::new(updates.xmp());
    for i in 0..src.segments().len() {
        let id = SegmentId::from_index(i)?;
        if !src.items_in(id).any(|e| e.kind() == ItemKind::Xmp) {
            b.copy_segment(id)?;
            continue;
        }
        match xmp.existing() {
            Existing::Keep => {
                b.copy_segment(id)?;
            }
            Existing::Replace(d) => new_xmp(&mut b, d)?,
            _ => {}
        }
    }
    // Nothing to replace: add it at the end.
    if let Some(d) = xmp.pending() {
        new_xmp(&mut b, d)?;
    }
    // The items of the segments that were copied follow them.
    b.carry_items(|_| true);
    b.finish()
}

fn new_xmp<'a>(b: &mut PlanBuilder<'_, 'a>, d: &'a [u8]) -> Result<()> {
    let header = vec![b'X', d.len() as u8];
    let id = b.put_segment(
        SegmentKind::Metadata,
        Name::Id(b'X' as u64),
        header,
        d,
        &[][..],
    )?;
    b.item(ItemInfo::xmp(), &[id])
}

fn parse_toy(handler: Arc<dyn ContainerHandler>, data: &[u8]) -> Result<Structure> {
    if !data.starts_with(b"TOY!") {
        return Err(Error::InvalidFormat("not a toy".into()));
    }
    let mut b = StructureBuilder::new(handler, "application/x-toy", data.len() as u64);
    b.push_span(SegmentKind::Preamble, Name::FourCC(*b"TOY!"), 4, 0, 0)?;
    let mut at = 4usize;
    while at < data.len() {
        let (kind, len) = match data[at..] {
            [k, l, ..] => (k, l as usize),
            _ => return Err(Error::InvalidFormat("truncated record".into())),
        };
        if at + 2 + len > data.len() {
            return Err(Error::InvalidFormat("record overruns".into()));
        }
        let seg_kind = if kind == b'X' {
            SegmentKind::Metadata
        } else {
            SegmentKind::Other
        };
        // Named by kind, not position, so a record keeps its name when others move.
        let id = b.push_span(seg_kind, Name::Id(kind as u64), 2, len as u64, 0)?;
        if kind == b'X' {
            b.item_of_segments(ItemInfo::xmp(), &[id])?;
        }
        at += 2 + len;
    }
    b.finish()
}

fn toy_bytes() -> Vec<u8> {
    let mut v = b"TOY!".to_vec();
    v.extend([b'A', 3, 1, 2, 3]);
    v.extend([b'X', 2, b'o', b'k']);
    v.extend([b'B', 1, 9]);
    v
}

fn registry() -> HandlerRegistry {
    HandlerRegistry::new().with_handler(Toy)
}

fn write(updates: &Updates, bytes: &[u8]) -> (Vec<u8>, Structure) {
    let mut src = Cursor::new(bytes.to_vec());
    let s = registry().parse("toy", &mut src).unwrap();
    let mut out = Cursor::new(Vec::new());
    let structure = s
        .write(&mut src, &mut out, updates, &mut |_: Chunk<'_>| Ok(()))
        .unwrap();
    (out.into_inner(), structure)
}

#[test]
fn set_replaces_in_place_and_remove_drops() {
    let bytes = toy_bytes();
    let (out, _) = write(&Updates::new().set_xmp(b"new!".to_vec()), &bytes);
    let mut want = b"TOY!".to_vec();
    want.extend([
        b'A', 3, 1, 2, 3, b'X', 4, b'n', b'e', b'w', b'!', b'B', 1, 9,
    ]);
    assert_eq!(out, want);

    let (out, _) = write(&Updates::new().remove_xmp(), &bytes);
    let mut want = b"TOY!".to_vec();
    want.extend([b'A', 3, 1, 2, 3, b'B', 1, 9]);
    assert_eq!(out, want);
}

#[test]
fn plan_is_data_that_can_be_checked_and_inspected() {
    let bytes = toy_bytes();
    let mut src = Cursor::new(bytes.clone());
    let s = registry().parse("toy", &mut src).unwrap();
    let updates = Updates::new().set_xmp(b"zz".to_vec());
    let plan = s.write_plan(&mut src, &updates).unwrap();
    validate(&plan, bytes.len() as u64).unwrap();

    let put: usize = plan
        .steps()
        .iter()
        .filter(|st| matches!(st, Step::Put { .. }))
        .map(|st| st.len() as usize)
        .sum();
    assert_eq!(put, 4); // the new record's header and payload
}

#[test]
fn the_toy_conforms() {
    Conformance::new(Arc::new(Toy)).check("toy", &toy_bytes());
}

/// Parses like the toy, but its writes ignore what it is asked to change.
#[derive(Debug)]
struct Deaf;

impl ContainerHandler for Deaf {
    fn id(&self) -> &'static str {
        "deaf"
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[FormatSpec::new("application/x-deaf", "deaf")];
        F
    }

    fn parse_async(self: Arc<Self>, mut io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async move {
            let data = io.read_vec(0, io.len() as usize).await?;
            parse_toy(self, &data)
        })
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        src: &'f Structure,
        _io: Io,
        _updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        static NOTHING: std::sync::OnceLock<Updates> = std::sync::OnceLock::new();
        let keep = NOTHING.get_or_init(Updates::new);
        Box::pin(async move { write_toy_plan(src, keep) })
    }
}

#[test]
#[should_panic(expected = "XMP is not what was set")]
fn conformance_catches_a_handler_that_ignores_updates() {
    Conformance::new(Arc::new(Deaf)).check("deaf", &toy_bytes());
}

#[test]
fn executor_rejects_steps_that_do_not_match_the_structure() {
    let bytes = toy_bytes();
    let mut src = Cursor::new(bytes.clone());
    let s = registry().parse("toy", &mut src).unwrap();
    let id = |i| SegmentId::from_index(i).unwrap();

    // A plan for the same structure, but one record's payload is left out.
    let mut short = WritePlan::new(s.plan(&mut src.clone(), &Updates::new()).unwrap());
    short.copy_segment(id(0), s.segment(id(0)).unwrap());
    short.copy_segment(id(1), s.segment(id(1)).unwrap());
    assert!(validate(&short, bytes.len() as u64).is_err());
    assert!(short
        .execute(&mut src, &mut Cursor::new(Vec::new()), &mut |_: Chunk<
            '_,
        >| Ok(
            ()
        ))
        .is_err());

    // Right size, wrong role: header bytes tagged as payload.
    let mut wrong = WritePlan::new(s.plan(&mut src.clone(), &Updates::new()).unwrap());
    wrong.copy(id(0), Role::Payload, ByteRange::new(0, 4).unwrap());
    assert!(validate(&wrong, bytes.len() as u64).is_err());
    assert!(wrong
        .execute(&mut src, &mut Cursor::new(Vec::new()), &mut |_: Chunk<
            '_,
        >| Ok(
            ()
        ))
        .is_err());
}

/// Run `task` to the end, answering each request from `source` by hand.
fn run_by_hand<T>(mut task: asset_io_core::Task<'_, T>, source: &[u8]) -> (Result<T>, usize) {
    let mut requests = 0;
    loop {
        match task.resume() {
            Ok(asset_io_core::Progress::Read(mut req)) => {
                requests += 1;
                let from = (req.offset() as usize).min(source.len());
                let to = (from + req.window_len()).min(source.len());
                req.buffer().extend_from_slice(&source[from..to]);
            }
            Ok(asset_io_core::Progress::Done(v)) => return (Ok(v), requests),
            Ok(_) => unreachable!(),
            Err(e) => return (Err(e), requests),
        }
    }
}

#[test]
fn a_third_party_handler_runs_under_a_task_with_no_changes() {
    let bytes = toy_bytes();
    let reg = registry();

    // Parsing, with the I/O done here.
    let (parsed, requests) =
        run_by_hand(reg.parse_task("toy", bytes.len() as u64).unwrap(), &bytes);
    let s = parsed.unwrap();
    assert_eq!(s.total_size(), bytes.len() as u64);
    assert_eq!(requests, 1); // the whole of a small stream, in one window

    // Detection by content, then the same parse.
    let (parsed, _) = run_by_hand(reg.parse_detect_task(bytes.len() as u64), &bytes);
    assert_eq!(parsed.unwrap().segments().len(), s.segments().len());

    // Planning a write, and running the plan step by step.
    let u = Updates::new().set_xmp(b"new!".to_vec());
    let (plan, _) = run_by_hand(s.write_plan_task(&u), &bytes);
    let plan = plan.unwrap();
    validate(&plan, bytes.len() as u64).unwrap();
    let mut run = plan.start();
    let mut out = Vec::new();
    loop {
        match run.step(&mut asset_io_core::NullSink).unwrap() {
            asset_io_core::Action::Read { offset, buf } => {
                let at = offset as usize;
                buf.copy_from_slice(&bytes[at..at + buf.len()]);
            }
            asset_io_core::Action::Write(b) => out.extend_from_slice(b),
            asset_io_core::Action::Done => break,
            _ => unreachable!(),
        }
    }
    let (want, _) = write(&u, &bytes);
    assert_eq!(out, want);
}
