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

//! Running write plans through the step-by-step interface, with the I/O done by
//! the test: the same bytes and sink calls as `Structure::write`, whatever the
//! buffer size.

#![cfg(all(feature = "jpeg", feature = "png", feature = "bmff", feature = "riff"))]

use asset_io::{
    Action, ByteRange, Chunk, Error, HandlerRegistry, NullSink, Result, Role, Sink, Updates,
    WritePlan,
};
use std::io::Cursor;
use std::path::PathBuf;

const FILES: &[(&str, &str)] = &[
    ("GreenCat.png", "png"),
    ("sample1.png", "png"),
    ("P1000708.jpg", "jpg"),
    ("FireflyTrain.jpg", "jpg"),
    ("sample1.heic", "heic"),
    ("sample1.m4a", "m4a"),
    ("sample1.webp", "webp"),
    ("sample1.wav", "wav"),
];

fn fixture(name: &str) -> Option<Vec<u8>> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    std::fs::read(p).ok()
}

/// What a sink saw: segment, role, offset and bytes of each chunk.
#[derive(Default, PartialEq, Debug)]
struct Seen(Vec<(usize, Role, u64, Vec<u8>)>);

impl Sink for Seen {
    fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
        self.0
            .push((c.segment.index(), c.role, c.offset, c.data.to_vec()));
        Ok(())
    }
}

/// What the driver observed.
#[derive(Default)]
struct Trace {
    reads: usize,
    max_read: usize,
    writes: usize,
}

/// Carry out `plan` against `source` with a buffer of `buf_len` bytes (or the
/// default), doing the I/O here: reads are copies out of a slice, writes append to a
/// `Vec`. Nothing in it touches a `Read` or `Write`.
fn drive(
    plan: &WritePlan<'_>,
    source: &[u8],
    buf_len: Option<usize>,
    sink: &mut dyn Sink,
) -> Result<(Vec<u8>, Trace)> {
    let mut run = plan.start();
    if let Some(n) = buf_len {
        run = run.with_buffer_len(n);
    }
    let (mut out, mut t) = (Vec::new(), Trace::default());
    loop {
        match run.step(sink)? {
            Action::Read { offset, buf } => {
                let start = offset as usize;
                let from = source
                    .get(start..start + buf.len())
                    .ok_or(Error::InvalidFormat(
                        "read past the end of the source".into(),
                    ))?;
                buf.copy_from_slice(from);
                t.reads += 1;
                t.max_read = t.max_read.max(buf.len());
            }
            Action::Write(bytes) => {
                out.extend_from_slice(bytes);
                t.writes += 1;
            }
            Action::Done => return Ok((out, t)),
            _ => unreachable!("an action this driver does not know"),
        }
    }
}

fn updates() -> Vec<Updates> {
    let jumbf = {
        let mut v = 3000u32.to_be_bytes().to_vec();
        v.extend(b"jumb");
        v.extend(vec![7u8; 2992]);
        v
    };
    let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>".to_vec();
    vec![
        Updates::new(),
        Updates::new().set_jumbf(jumbf.clone()),
        Updates::new().set_xmp(xmp.clone()).set_jumbf(jumbf),
        Updates::new().remove_xmp().remove_jumbf(),
    ]
}

#[test]
fn stepping_gives_the_bytes_and_sink_calls_of_write() {
    let reg = HandlerRegistry::builtin();
    let mut checked = 0;
    for (name, ext) in FILES {
        let Some(bytes) = fixture(name) else { continue };
        let s = reg.parse(ext, &mut Cursor::new(&bytes)).unwrap();
        for (n, u) in updates().iter().enumerate() {
            // What write does.
            let mut want_sink = Seen::default();
            let mut want = Cursor::new(Vec::new());
            let layout = s
                .write(&mut Cursor::new(&bytes), &mut want, u, &mut want_sink)
                .unwrap();
            let want = want.into_inner();

            let plan = s.write_plan(&mut Cursor::new(&bytes), u).unwrap();
            // Small, awkward and default buffers.
            for buf_len in [Some(4096), Some(5000), Some(70_001), None] {
                let mut sink = Seen::default();
                let (got, t) = drive(&plan, &bytes, buf_len, &mut sink).unwrap();
                assert_eq!(got.len() as u64, layout.total_size(), "{name} #{n}");
                assert!(got == want, "{name} #{n} buffer {buf_len:?}: bytes differ");
                // The sink sees the same bytes in the same places, though a
                // different buffer cuts them into different chunks.
                // Where each (segment, role) run starts.
                let join = |seen: &Seen| -> Vec<(usize, Role, u64)> {
                    let mut v: Vec<(usize, Role, u64)> = Vec::new();
                    for (seg, role, at, _) in &seen.0 {
                        if !matches!(v.last(), Some(&(s, r, _)) if (s, r) == (*seg, *role)) {
                            v.push((*seg, *role, *at));
                        }
                    }
                    v
                };
                assert_eq!(join(&sink), join(&want_sink), "{name} #{n}");
                let all: Vec<u8> = sink.0.iter().flat_map(|c| c.3.clone()).collect();
                assert!(all == want, "{name} #{n}: sink bytes differ");
                assert!(t.max_read <= buf_len.unwrap_or(256 * 1024).max(4096));
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no fixtures found");
}

#[test]
fn copies_are_read_in_large_windows_not_per_segment() {
    let Some(bytes) = fixture("GreenCat.png") else {
        return;
    };
    let s = HandlerRegistry::builtin()
        .parse("png", &mut Cursor::new(&bytes))
        .unwrap();
    let keep = Updates::new();
    let plan = s.write_plan(&mut Cursor::new(&bytes), &keep).unwrap();
    let (out, t) = drive(&plan, &bytes, None, &mut NullSink).unwrap();
    assert_eq!(out, bytes);
    // About one read and one write per 256 KiB of a 2.4 MB file, not one per chunk.
    assert!(
        t.reads <= bytes.len() / (256 * 1024) + 3,
        "{} reads",
        t.reads
    );
    assert!(t.writes <= t.reads + 3, "{} writes", t.writes);
}

#[test]
fn a_short_source_is_the_callers_error_to_report() {
    let Some(bytes) = fixture("sample1.png") else {
        return;
    };
    let s = HandlerRegistry::builtin()
        .parse("png", &mut Cursor::new(&bytes))
        .unwrap();
    let keep = Updates::new();
    let plan = s.write_plan(&mut Cursor::new(&bytes), &keep).unwrap();
    let short = &bytes[..bytes.len() - 100];
    assert!(drive(&plan, short, None, &mut NullSink).is_err());
}

#[test]
fn a_sink_can_cancel_and_the_run_ends() {
    let Some(bytes) = fixture("sample1.png") else {
        return;
    };
    let s = HandlerRegistry::builtin()
        .parse("png", &mut Cursor::new(&bytes))
        .unwrap();
    let keep = Updates::new();
    let plan = s.write_plan(&mut Cursor::new(&bytes), &keep).unwrap();
    let mut n = 0;
    let mut cancel = |_: Chunk<'_>| -> Result<()> {
        n += 1;
        if n == 3 {
            Err(Error::UserCanceled)
        } else {
            Ok(())
        }
    };
    let r = drive(&plan, &bytes, None, &mut cancel);
    assert!(matches!(r, Err(Error::UserCanceled)));
}

#[test]
fn steps_that_do_not_match_the_structure_fail_while_stepping() {
    let Some(bytes) = fixture("sample1.png") else {
        return;
    };
    let reg = HandlerRegistry::builtin();
    let s = reg.parse("png", &mut Cursor::new(&bytes)).unwrap();
    let id = asset_io::SegmentId::from_index;

    // A header copied as if it were a payload.
    let mut wrong = WritePlan::new(s.plan(&mut Cursor::new(&bytes), &Updates::new()).unwrap());
    wrong.copy(id(0).unwrap(), Role::Payload, ByteRange::new(0, 8).unwrap());
    assert!(drive(&wrong, &bytes, None, &mut NullSink).is_err());

    // Too few bytes: the structure is not covered.
    let mut short = WritePlan::new(s.plan(&mut Cursor::new(&bytes), &Updates::new()).unwrap());
    short.copy_segment(id(0).unwrap(), s.segment(id(0).unwrap()).unwrap());
    assert!(drive(&short, &bytes, None, &mut NullSink).is_err());
}

#[test]
fn stepping_past_the_end_keeps_saying_done() {
    let Some(bytes) = fixture("sample1.png") else {
        return;
    };
    let s = HandlerRegistry::builtin()
        .parse("png", &mut Cursor::new(&bytes))
        .unwrap();
    let keep = Updates::new();
    let plan = s.write_plan(&mut Cursor::new(&bytes), &keep).unwrap();
    let mut run = plan.start();
    let mut sink = NullSink;
    let mut src = Cursor::new(&bytes);
    loop {
        match run.step(&mut sink).unwrap() {
            Action::Read { offset, buf } => {
                use std::io::{Read, Seek, SeekFrom};
                src.seek(SeekFrom::Start(offset)).unwrap();
                src.read_exact(buf).unwrap();
            }
            Action::Write(_) => {}
            Action::Done => break,
            _ => unreachable!(),
        }
    }
    assert!(matches!(run.step(&mut sink).unwrap(), Action::Done));
}
