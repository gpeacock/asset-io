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

use asset_io::{HandlerRegistry, NullSink, Updates};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let mut src = Cursor::new(data);
    let Ok(structure) = HandlerRegistry::builtin().parse_detect(&mut src) else {
        return;
    };

    let cases = [
        Updates::new(),
        Updates::new().set_xmp(b"<test>fuzz</test>".to_vec()),
        Updates::new().set_jumbf(b"jumbf".to_vec()),
        Updates::new().set_jumbf(b"jumbf".to_vec()).as_update(),
        Updates::new().set_merkle(vec![vec![0u8; 8]; 2]),
        Updates::new().set_jumbf(b"jumbf".to_vec()).remove_merkle(),
        Updates::new().remove_jumbf(),
        Updates::new().remove_xmp(),
    ];
    for updates in &cases {
        // Cursor<Vec<u8>> is Read + Write + Seek, as BMFF offset patching needs.
        let mut out = Cursor::new(Vec::new());
        let _ = structure.write(&mut src, &mut out, updates, &mut NullSink);
    }
});
