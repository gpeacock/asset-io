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

//! Read-side sink and zero-copy slices on real files.

#![cfg(all(feature = "jpeg", feature = "png", feature = "bmff"))]

use asset_io::{ByteRange, Chunk, Error, HandlerRegistry, NullSink, Result, Role, Structure};
use std::borrow::Cow;
use std::io::Cursor;

const FILES: &[&str] = &[
    "tests/fixtures/FireflyTrain.jpg",
    "tests/fixtures/P1000708.jpg",
    "tests/fixtures/GreenCat.png",
    "tests/fixtures/sample1.png",
    "tests/fixtures/sample1.heic",
    "tests/fixtures/sample1.avif",
    "tests/fixtures/sample1.m4a",
];

fn load() -> Vec<(&'static str, Vec<u8>, Structure)> {
    FILES
        .iter()
        .filter_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            let s = HandlerRegistry::builtin()
                .parse_detect(&mut Cursor::new(&bytes))
                .ok()?;
            Some((*p, bytes, s))
        })
        .collect()
}

/// Rebuild the file from chunks, checking order, tags and coverage.
fn rebuild(s: &Structure, read: impl FnOnce(&mut dyn asset_io::Sink) -> Result<()>) -> Vec<u8> {
    let mut out = vec![0u8; s.total_size() as usize];
    let mut next = 0u64;
    let mut sink = |c: Chunk<'_>| -> Result<()> {
        assert_eq!(c.offset, next, "chunks must arrive in order, without gaps");
        let seg = s.segment(c.segment)?;
        assert!(seg
            .range()
            .contains(&ByteRange::new(c.offset, c.data.len() as u64)?));
        out[c.offset as usize..c.offset as usize + c.data.len()].copy_from_slice(c.data);
        next += c.data.len() as u64;
        Ok(())
    };
    read(&mut sink).unwrap();
    assert_eq!(next, s.total_size());
    out
}

#[test]
fn read_sinks_deliver_the_whole_file() {
    let files = load();
    assert!(!files.is_empty(), "no fixtures found");
    for (name, bytes, s) in &files {
        let streamed = rebuild(s, |k| s.read(&mut Cursor::new(bytes), k));
        assert_eq!(&streamed, bytes, "{name}: streamed");
        let sliced = rebuild(s, |k| s.read_slice(bytes, k));
        assert_eq!(&sliced, bytes, "{name}: sliced");
    }
}

#[test]
fn unwanted_runs_are_skipped_and_cancel_stops() {
    struct PayloadOnly(u64);
    impl asset_io::Sink for PayloadOnly {
        fn wants(&self, _: &asset_io::Segment, role: Role) -> bool {
            role == Role::Payload
        }
        fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
            assert_eq!(c.role, Role::Payload);
            self.0 += c.data.len() as u64;
            Ok(())
        }
    }
    for (name, bytes, s) in &load() {
        let mut k = PayloadOnly(0);
        s.read(&mut Cursor::new(bytes), &mut k).unwrap();
        let expect: u64 = s
            .regions()
            .filter(|r| r.role == Role::Payload)
            .map(|r| r.range.len())
            .sum();
        assert_eq!(k.0, expect, "{name}");

        s.read(&mut Cursor::new(bytes), &mut NullSink).unwrap();
        let mut cancel = |_: Chunk<'_>| -> Result<()> { Err(Error::UserCanceled) };
        assert!(matches!(
            s.read_slice(bytes, &mut cancel),
            Err(Error::UserCanceled)
        ));
    }
}

#[test]
fn slices_match_reads_and_borrow_when_possible() {
    let mut borrowed = 0;
    for (name, bytes, s) in &load() {
        for e in s.items() {
            let kind = e.kind();
            let read = s.read_item(&mut Cursor::new(bytes), e).unwrap();
            let slice = s.slice_item(bytes, e).unwrap();
            assert_eq!(&*slice, &read[..], "{name} {kind}");
            if e.extent_count() == 1 {
                assert!(matches!(slice, Cow::Borrowed(_)), "{name} {kind}");
                borrowed += 1;
            }
        }
    }
    assert!(borrowed > 0, "no single-extent item in any fixture");
}

#[test]
fn slice_rejects_bad_input() {
    for (_, bytes, s) in &load() {
        let all = ByteRange::new(0, s.total_size()).unwrap();
        assert_eq!(s.slice(bytes, all).unwrap(), &bytes[..]);
        // Buffer shorter than the asset.
        assert!(s.slice(&bytes[..bytes.len() - 1], all).is_err());
        // Range past the end.
        assert!(s
            .slice(bytes, ByteRange::new(s.total_size(), 1).unwrap())
            .is_err());
        assert!(s
            .slice(bytes, ByteRange::new(u64::MAX - 1, 1).unwrap())
            .is_err());
    }
}

#[cfg(feature = "memory-mapped")]
#[test]
fn mapped_file_parses_and_slices() {
    let path = "tests/fixtures/FireflyTrain.jpg";
    let file = std::fs::File::open(path).unwrap();
    // SAFETY: the fixture is not modified during the test.
    let map = unsafe { asset_io::MappedFile::map(&file).unwrap() };
    assert_eq!(&*map, &std::fs::read(path).unwrap()[..]);
    let s = map.parse_detect(&HandlerRegistry::builtin()).unwrap();
    let streamed = rebuild(&s, |k| s.read_slice(&map, k));
    assert_eq!(&streamed, &*map);
}
