#![no_main]

use libfuzzer_sys::fuzz_target;
use asset_io::{Asset, Updates};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Try to parse and then write with modifications
    let cursor = Cursor::new(data);

    if let Ok(mut asset) = Asset::from_source(cursor) {
        // Cursor<Vec<u8>> implements Read + Write + Seek (required for BMFF chunk offset adjustment)
        let mut output = Cursor::new(Vec::new());

        // Try writing with various update configurations
        let _ = asset.write(&mut output, &Updates::new());

        let mut output = Cursor::new(Vec::new());
        let _ = asset.write(
            &mut output,
            &Updates::new().set_xmp(b"<test>fuzz</test>".to_vec()),
        );

        let mut output = Cursor::new(Vec::new());
        let _ = asset.write(&mut output, &Updates::new().remove_jumbf());

        let mut output = Cursor::new(Vec::new());
        let _ = asset.write(&mut output, &Updates::new().remove_xmp());
    }
});
