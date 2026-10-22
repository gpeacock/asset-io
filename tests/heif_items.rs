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

//! HEIF items (EXIF, thumbnails) on the real files in tests/fixtures.
#![cfg(feature = "bmff")]

use asset_io::{HandlerRegistry, NullSink, SegmentKind, Structure, Updates};
use std::io::Cursor;

const FILES: &[&str] = &[
    "tests/fixtures/sample1.heic",
    "tests/fixtures/sample1.heif",
    "tests/fixtures/sample1.avif",
];

fn items(s: &Structure, bytes: &[u8]) -> Vec<(SegmentKind, Option<String>, Vec<u8>)> {
    [SegmentKind::Exif, SegmentKind::Thumbnail]
        .into_iter()
        .flat_map(|k| s.items(k))
        .map(|e| {
            (
                e.kind(),
                e.label().map(str::to_owned),
                s.read_item(&mut Cursor::new(bytes), e).unwrap(),
            )
        })
        .collect()
}

#[test]
fn items_survive_a_write_and_stay_readable() {
    let mut ran = 0;
    for path in FILES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        ran += 1;
        let s = HandlerRegistry::builtin()
            .parse_detect(&mut Cursor::new(&bytes))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let before = items(&s, &bytes);
        eprintln!(
            "{path}: {} exif, {} thumbnail",
            before.iter().filter(|i| i.0 == SegmentKind::Exif).count(),
            before
                .iter()
                .filter(|i| i.0 == SegmentKind::Thumbnail)
                .count()
        );

        // Writing a manifest moves the data; the items must move with it, in both
        // the structure the write returns and a fresh parse of the output.
        let mut out = Cursor::new(Vec::new());
        let dest = s
            .write(
                &mut Cursor::new(&bytes),
                &mut out,
                &Updates::new().set_jumbf(vec![0u8; 100]),
                &mut NullSink,
            )
            .unwrap();
        let out = out.into_inner();
        let again = HandlerRegistry::builtin()
            .parse_detect(&mut Cursor::new(&out))
            .unwrap();
        assert_eq!(items(&dest, &out), before, "{path}: returned structure");
        assert_eq!(items(&again, &out), before, "{path}: reparsed output");

        #[cfg(feature = "exif")]
        {
            // Real EXIF must parse, or the item was not the TIFF data.
            if before.iter().any(|i| i.0 == SegmentKind::Exif) {
                assert!(s.exif_info(&mut Cursor::new(&bytes)).is_ok(), "{path}");
            }
        }
    }
    assert!(ran > 0, "no HEIF fixtures found");
}
