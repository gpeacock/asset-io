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

use asset_io::plan::validate;
use asset_io::{
    BoxFuture, ByteRange, Chunk, ContainerHandler, Error, FormatSpec, HandlerRegistry, Io,
    MetadataUpdate, Name, Result, Role, SegmentId, SegmentKind, Sink, Step, Structure,
    StructureBuilder, Updates, WritePlan,
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
    {
        // What each output record is: a source segment to copy, or new XMP.
        enum Rec<'a> {
            Copy(SegmentId),
            Xmp(&'a [u8]),
        }
        let mut recs = Vec::new();
        let mut xmp_done = false;
        for (i, seg) in src.segments().iter().enumerate() {
            let id = SegmentId::from_index(i)?;
            if seg.kind() != SegmentKind::Xmp {
                recs.push(Rec::Copy(id));
                continue;
            }
            match updates.xmp() {
                MetadataUpdate::Set(d) if !xmp_done => {
                    recs.push(Rec::Xmp(d));
                    xmp_done = true;
                }
                MetadataUpdate::Remove | MetadataUpdate::Set(_) => {}
                _ => recs.push(Rec::Copy(id)),
            }
        }
        if let (MetadataUpdate::Set(d), false) = (updates.xmp(), xmp_done) {
            recs.push(Rec::Xmp(d));
        }

        let len_of = |r: &Rec| match r {
            Rec::Copy(id) => src.segment(*id).map(|s| s.extent().len()),
            Rec::Xmp(d) => Ok(2 + d.len() as u64),
        };
        let mut total = 0;
        for r in &recs {
            total += len_of(r)?;
        }
        let mut b = StructureBuilder::new(src.handler().clone(), "application/x-toy", total);
        for r in &recs {
            match r {
                Rec::Copy(id) => {
                    let s = src.segment(*id)?;
                    let new = b.push_span(
                        s.kind(),
                        s.name(),
                        s.header().len() as u32,
                        s.payload().len(),
                        s.trailer().len() as u16,
                    )?;
                    if s.kind() == SegmentKind::Xmp {
                        b.entity_of_segments(SegmentKind::Xmp, &[new])?;
                    }
                }
                Rec::Xmp(d) => {
                    let new = b.push_span(SegmentKind::Xmp, Name::None, 2, d.len() as u64, 0)?;
                    b.entity_of_segments(SegmentKind::Xmp, &[new])?;
                }
            }
        }

        let mut plan = WritePlan::new(b.finish()?);
        for (i, r) in recs.iter().enumerate() {
            let dest = SegmentId::from_index(i)?;
            match r {
                Rec::Copy(id) => {
                    plan.copy_segment(dest, src.segment(*id)?);
                }
                Rec::Xmp(d) => {
                    plan.put(dest, Role::Header, vec![b'X', d.len() as u8]);
                    plan.put(dest, Role::Payload, *d);
                }
            }
        }
        Ok(plan)
    }
}

fn parse_toy(handler: Arc<Toy>, data: &[u8]) -> Result<Structure> {
    if !data.starts_with(b"TOY!") {
        return Err(Error::InvalidFormat("not a toy".into()));
    }
    let mut b = StructureBuilder::new(handler, "application/x-toy", data.len() as u64);
    b.push_span(SegmentKind::Header, Name::FourCC(*b"TOY!"), 4, 0, 0)?;
    let (mut at, mut idx) = (4usize, 0u64);
    while at < data.len() {
        let (kind, len) = match data[at..] {
            [k, l, ..] => (k, l as usize),
            _ => return Err(Error::InvalidFormat("truncated record".into())),
        };
        if at + 2 + len > data.len() {
            return Err(Error::InvalidFormat("record overruns".into()));
        }
        let seg_kind = if kind == b'X' {
            SegmentKind::Xmp
        } else {
            SegmentKind::Other
        };
        let id = b.push_span(seg_kind, Name::Id(idx), 2, len as u64, 0)?;
        if kind == b'X' {
            b.entity_of_segments(SegmentKind::Xmp, &[id])?;
        }
        at += 2 + len;
        idx += 1;
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
fn default_write_runs_the_plan() {
    let bytes = toy_bytes();
    let (out, structure) = write(&Updates::new(), &bytes);
    assert_eq!(out, bytes);
    assert_eq!(structure.total_size(), bytes.len() as u64);
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
fn output_parses_back_to_the_structure_that_was_planned() {
    let bytes = toy_bytes();
    let updates = Updates::new().set_xmp(b"packet".to_vec());
    let (out, structure) = write(&updates, &bytes);

    let mut c = Cursor::new(out);
    let reparsed = registry().parse("toy", &mut c).unwrap();
    assert_eq!(reparsed.total_size(), structure.total_size());
    assert_eq!(reparsed.segments().len(), structure.segments().len());
    assert_eq!(
        reparsed.xmp(&mut c).unwrap().as_deref(),
        Some(&b"packet"[..])
    );

    let mut src = Cursor::new(bytes);
    let s = registry().parse("toy", &mut src).unwrap();
    let planned = s.plan(&mut src, &updates).unwrap();
    assert_eq!(planned.total_size(), structure.total_size());
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
fn sink_sees_every_byte_once_with_segment_and_role() {
    let bytes = toy_bytes();
    let mut src = Cursor::new(bytes.clone());
    let s = registry().parse("toy", &mut src).unwrap();

    struct Collect(Vec<u8>, Vec<(usize, Role)>);
    impl Sink for Collect {
        fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
            assert_eq!(c.offset as usize, self.0.len());
            self.0.extend_from_slice(c.data);
            self.1.push((c.segment.index(), c.role));
            Ok(())
        }
    }
    let mut sink = Collect(Vec::new(), Vec::new());
    s.write(
        &mut src,
        &mut Cursor::new(Vec::new()),
        &Updates::new(),
        &mut sink,
    )
    .unwrap();
    assert_eq!(sink.0, bytes);
    assert!(sink.1.contains(&(2, Role::Header)));
    assert!(sink.1.contains(&(2, Role::Payload)));
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
fn run_by_hand<T>(mut task: asset_io::Task<'_, T>, source: &[u8]) -> (Result<T>, usize) {
    let mut requests = 0;
    loop {
        match task.resume() {
            Ok(asset_io::Progress::Read(mut req)) => {
                requests += 1;
                let from = (req.offset() as usize).min(source.len());
                let to = (from + req.window_len()).min(source.len());
                req.buffer().extend_from_slice(&source[from..to]);
            }
            Ok(asset_io::Progress::Done(v)) => return (Ok(v), requests),
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
        match run.step(&mut asset_io::NullSink).unwrap() {
            asset_io::Action::Read { offset, buf } => {
                let at = offset as usize;
                buf.copy_from_slice(&bytes[at..at + buf.len()]);
            }
            asset_io::Action::Write(b) => out.extend_from_slice(b),
            asset_io::Action::Done => break,
            _ => unreachable!(),
        }
    }
    let (want, _) = write(&u, &bytes);
    assert_eq!(out, want);
}
