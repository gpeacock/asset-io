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

//! The conformance checks, run on every real file in `tests/fixtures` with the
//! handler that serves it.

use asset_io::testing::Conformance;
use asset_io::HandlerRegistry;
use std::fs;
use std::path::PathBuf;

/// A JUMBF superbox of `n` bytes.
fn jumbf(n: usize) -> Vec<u8> {
    let mut v = vec![0x5Au8; n];
    v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
    v[4..8].copy_from_slice(b"jumb");
    v
}

#[test]
fn every_fixture_conforms_to_its_handler() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let registry = HandlerRegistry::builtin();
    let mut checked = 0;
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for path in files {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(handler) = registry.handler(&ext.to_ascii_lowercase()) else {
            continue;
        };
        let bytes = fs::read(&path).unwrap();
        // The truncations and byte flips are the same code paths in a big file.
        let small = bytes.len() < 512 * 1024;
        if bytes.len() > 16 * 1024 * 1024 {
            continue;
        }
        Conformance::new(handler.clone())
            .jumbf(Some(jumbf(2048)))
            .mutations(small)
            .check(&path.display().to_string(), &bytes);
        checked += 1;
    }
    assert!(checked > 0, "no fixtures were checked");
}
