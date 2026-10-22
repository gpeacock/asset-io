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

use asset_io::containers::bmff::BmffUpdates;
use asset_io::{HandlerRegistry, NullSink, Updates};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

const XPACKET: &[u8] = b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/><?xpacket end=\"w\"?>";

fuzz_target!(|data: &[u8]| {
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(data);
    let structure = match registry.parse_detect(&mut src) {
        Ok(s) => s,
        Err(_) => match registry.parse("svg", &mut src) {
            Ok(s) => s,
            Err(_) => return,
        },
    };

    let cases = [
        Updates::new(),
        Updates::new().set_xmp(b"<test>fuzz</test>".to_vec()),
        Updates::new().set_xmp(XPACKET.to_vec()),
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
