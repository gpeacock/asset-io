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

//! RIFF handler on the real files in tests/fixtures.
#![cfg(feature = "riff")]

use asset_io::{HandlerRegistry, NullSink, SegmentKind, Updates};
use std::io::Cursor;

const FILES: &[&str] = &["tests/fixtures/sample1.webp", "tests/fixtures/sample1.wav"];

fn write(bytes: &[u8], u: &Updates) -> asset_io::Result<Vec<u8>> {
    let s = HandlerRegistry::builtin().parse_detect(&mut Cursor::new(bytes))?;
    let mut out = Cursor::new(Vec::new());
    let dest = s.write(&mut Cursor::new(bytes), &mut out, u, &mut NullSink)?;
    let out = out.into_inner();
    let again = HandlerRegistry::builtin().parse_detect(&mut Cursor::new(&out))?;
    assert_eq!(dest.segments(), again.segments());
    assert_eq!(dest.total_size(), out.len() as u64);
    Ok(out)
}

#[test]
fn real_files_round_trip_and_take_metadata() {
    let mut ran = 0;
    for path in FILES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        ran += 1;
        let s = HandlerRegistry::builtin()
            .parse_detect(&mut Cursor::new(&bytes))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let covered: u64 = s.regions().map(|r| r.range.len()).sum();
        assert_eq!(covered, bytes.len() as u64, "{path}");

        assert_eq!(
            write(&bytes, &Updates::new()).unwrap(),
            bytes,
            "{path}: keep"
        );

        let u = Updates::new()
            .set_jumbf(vec![0xAB; 1001])
            .set_xmp(b"<x:xmpmeta/>".to_vec());
        let out = write(&bytes, &u).unwrap_or_else(|e| panic!("{path}: {e}"));
        let t = HandlerRegistry::builtin()
            .parse_detect(&mut Cursor::new(&out))
            .unwrap();
        for (kind, want) in [
            (SegmentKind::Jumbf, vec![0xAB; 1001]),
            (SegmentKind::Xmp, b"<x:xmpmeta/>".to_vec()),
        ] {
            let e = t.require_item(kind).unwrap();
            assert_eq!(
                t.read_item(&mut Cursor::new(&out), e).unwrap(),
                want,
                "{path}"
            );
        }

        // Removing what was added gives back the media, whatever else changed
        // (a WebP gains a VP8X chunk when it takes XMP).
        let back = write(&out, &Updates::new().remove_jumbf().remove_xmp()).unwrap();
        let b = HandlerRegistry::builtin()
            .parse_detect(&mut Cursor::new(&back))
            .unwrap();
        assert!(b.item(SegmentKind::Jumbf).unwrap().is_none(), "{path}");
        assert!(b.item(SegmentKind::Xmp).unwrap().is_none(), "{path}");
    }
    assert!(ran > 0, "no RIFF fixtures found");
}
