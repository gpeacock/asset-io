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

//! Parsing through a `Task`, with the I/O done by the test: the same structure as
//! the blocking parse, from a few small requests.

#![cfg(all(feature = "png", feature = "riff"))]

use asset_io::{Error, HandlerRegistry, NullSink, Progress, Result, Structure, Task, Updates};
use std::io::Cursor;
use std::path::PathBuf;

fn fixture(name: &str) -> Option<Vec<u8>> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    std::fs::read(p).ok()
}

/// Run `task` against `source`, answering each request from the slice. Returns the
/// result and how many requests were made.
fn drive<T>(mut task: Task<'_, T>, source: &[u8]) -> (Result<T>, usize) {
    let mut requests = 0;
    loop {
        match task.resume() {
            Ok(Progress::Read(mut req)) => {
                requests += 1;
                let from = (req.offset() as usize).min(source.len());
                let to = (from + req.window_len()).min(source.len());
                let buf = req.buffer();
                buf.clear();
                buf.extend_from_slice(&source[from..to]);
            }
            Ok(Progress::Done(s)) => return (Ok(s), requests),
            Ok(_) => unreachable!("a progress this driver does not know"),
            Err(e) => return (Err(e), requests),
        }
    }
}

/// Everything observable about a structure's shape.
fn shape(s: &Structure) -> String {
    format!(
        "{} {} {:?} {:?}",
        s.media_type(),
        s.total_size(),
        s.segments(),
        s.regions().collect::<Vec<_>>()
    )
}

#[test]
fn a_task_parses_to_the_structure_the_blocking_parse_gives() {
    let reg = HandlerRegistry::builtin();
    let mut checked = 0;
    for name in ["GreenCat.png", "sample1.png"] {
        let Some(bytes) = fixture(name) else { continue };
        let want = reg.parse("png", &mut Cursor::new(&bytes)).unwrap();

        let (got, requests) = drive(reg.parse_task("png", bytes.len() as u64).unwrap(), &bytes);
        assert_eq!(shape(&got.unwrap()), shape(&want), "{name}");
        // Chunk headers are tiny and many; the window turns them into few requests.
        assert!(requests < 100, "{name}: {requests} requests");

        let (got, _) = drive(reg.parse_detect_task(bytes.len() as u64), &bytes);
        assert_eq!(shape(&got.unwrap()), shape(&want), "{name} (detected)");
        checked += 1;
    }
    assert!(checked > 0, "no fixtures found");
}

#[test]
fn a_damaged_file_fails_the_same_way_through_a_task() {
    let reg = HandlerRegistry::builtin();
    let Some(bytes) = fixture("sample1.png") else {
        return;
    };
    let mut cuts = vec![
        0,
        4,
        8,
        20,
        33,
        100,
        bytes.len() / 2,
        bytes.len() - 13,
        bytes.len() - 1,
    ];
    cuts.push(bytes.len() - 12);
    for cut in cuts {
        let cut = &bytes[..cut];
        let want = reg.parse("png", &mut Cursor::new(cut));
        let (got, _) = drive(reg.parse_task("png", cut.len() as u64).unwrap(), cut);
        match (&want, &got) {
            (Ok(a), Ok(b)) => assert_eq!(shape(a), shape(b)),
            (Err(_), Err(_)) => {}
            _ => panic!("{} bytes: {want:?} vs {got:?}", cut.len()),
        }
    }
}

#[test]
fn a_format_nobody_claims_is_unsupported() {
    let reg = HandlerRegistry::builtin();
    assert!(matches!(
        reg.parse_task("nope", 10),
        Err(Error::UnsupportedFormat)
    ));
    let (got, _) = drive(reg.parse_detect_task(12), b"not an image");
    assert!(matches!(got, Err(Error::UnsupportedFormat)));
}

#[test]
fn riff_parses_through_a_task_like_the_blocking_parse() {
    let reg = HandlerRegistry::builtin();
    let mut checked = 0;
    for (name, ext) in [("sample1.webp", "webp"), ("sample1.wav", "wav")] {
        let Some(bytes) = fixture(name) else { continue };
        let want = reg.parse(ext, &mut Cursor::new(&bytes)).unwrap();
        let (got, requests) = drive(reg.parse_task(ext, bytes.len() as u64).unwrap(), &bytes);
        assert_eq!(shape(&got.unwrap()), shape(&want), "{name}");
        assert!(requests < 100, "{name}: {requests} requests");
        let (got, _) = drive(reg.parse_detect_task(bytes.len() as u64), &bytes);
        assert_eq!(shape(&got.unwrap()), shape(&want), "{name} (detected)");

        // Every way of cutting the file short fails or parses the same way.
        for cut in (0..bytes.len().min(200)).chain(bytes.len() - 20..bytes.len()) {
            let part = &bytes[..cut];
            let a = reg.parse(ext, &mut Cursor::new(part));
            let (b, _) = drive(reg.parse_task(ext, part.len() as u64).unwrap(), part);
            match (&a, &b) {
                (Ok(x), Ok(y)) => assert_eq!(shape(x), shape(y), "{name} cut {cut}"),
                (Err(_), Err(_)) => {}
                _ => panic!("{name} cut {cut}: {a:?} vs {b:?}"),
            }
        }
        checked += 1;
    }
    assert!(checked > 0, "no fixtures found");
}

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn riff(form: &[u8; 4], kids: &[Vec<u8>]) -> Vec<u8> {
    let mut body = form.to_vec();
    body.extend(kids.concat());
    let mut v = b"RIFF".to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    v
}

/// The bytes a plan would write.
fn execute(plan: &asset_io::WritePlan<'_>, source: &[u8]) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    plan.execute(&mut Cursor::new(source), &mut out, &mut NullSink)
        .unwrap();
    out.into_inner()
}

#[test]
fn a_write_plan_task_reads_what_it_needs_and_gives_the_same_plan() {
    let reg = HandlerRegistry::builtin();
    // A WebP with no VP8X chunk needs its size read from the image chunk, and one
    // with VP8X needs its flags read: the two reads a RIFF plan can make.
    let mut vp8l = vec![0x2f, 0, 0, 0, 0];
    vp8l.extend([1u8; 20]);
    let mut vp8x = vec![0x08, 0, 0, 0];
    vp8x.extend([0u8; 6]);
    let files = [
        riff(b"WEBP", &[chunk(b"VP8L", &vp8l)]),
        riff(
            b"WEBP",
            &[
                chunk(b"VP8X", &vp8x),
                chunk(b"VP8L", &vp8l),
                chunk(b"EXIF", b"abc"),
            ],
        ),
        riff(
            b"WAVE",
            &[chunk(b"fmt ", &[1; 16]), chunk(b"data", &[5; 7])],
        ),
    ];
    let updates = [
        Updates::new(),
        Updates::new().set_xmp(b"<x/>".to_vec()),
        Updates::new()
            .set_xmp(b"<x/>".to_vec())
            .set_jumbf(b"S".to_vec()),
        Updates::new().remove_xmp(),
    ];
    for (i, f) in files.iter().enumerate() {
        let ext = if i < 2 { "webp" } else { "wav" };
        let s = reg.parse(ext, &mut Cursor::new(f)).unwrap();
        for (j, u) in updates.iter().enumerate() {
            let want = s.write_plan(&mut Cursor::new(f), u).unwrap();
            let (got, requests) = drive(s.write_plan_task(u), f);
            let got = got.unwrap();
            assert_eq!(execute(&got, f), execute(&want, f), "file {i} updates {j}");
            // A handful of tiny reads at most, and none for a plan that needs none.
            assert!(requests <= 3, "file {i} updates {j}: {requests} requests");
        }
    }
    // A plan that cannot be built (no image chunk to take the WebP size from) fails
    // the same way through a task.
    let none = riff(b"WEBP", &[chunk(b"ICCP", b"x")]);
    let s = reg.parse("webp", &mut Cursor::new(&none)).unwrap();
    let u = Updates::new().set_xmp(b"<x/>".to_vec());
    assert!(s.write_plan(&mut Cursor::new(&none), &u).is_err());
    assert!(drive(s.write_plan_task(&u), &none).0.is_err());
}
