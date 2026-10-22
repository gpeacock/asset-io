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

#![cfg(feature = "png")]

use asset_io::{Error, HandlerRegistry, Progress, Result, Structure, Task};
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
fn drive(mut task: Task<'_, Structure>, source: &[u8]) -> (Result<Structure>, usize) {
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
