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

#![no_main]

use asset_io::{HandlerRegistry, SegmentKind};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Any input must parse or return an error; never panic.
    let mut src = Cursor::new(data);
    let Ok(structure) = HandlerRegistry::builtin().parse_detect(&mut src) else {
        return;
    };

    // Read what the structure describes.
    for kind in [SegmentKind::Xmp, SegmentKind::Jumbf, SegmentKind::Exif] {
        for e in structure.entities(kind) {
            let _ = structure.read_entity(&mut src, e);
        }
    }
    // Hierarchical traversal.
    let _ = structure.walk(&mut src, |_| Ok(()));
});
