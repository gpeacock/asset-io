#![no_main]

use libfuzzer_sys::fuzz_target;
use asset_io::{Asset, Updates};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Try to parse and then write with modifications
    let cursor = Cursor::new(data);
    
    if let Ok(mut asset) = Asset::from_source(cursor) {
        // Try writing with various update configurations
        let updates = Updates::new();
        let mut output = Vec::new();
        let _ = asset.write(&mut output, &updates);
        
        // Try with XMP update
        let updates = Updates::new()
            .set_xmp(b"<test>fuzz</test>".to_vec());
        let mut output = Vec::new();
        let _ = asset.write(&mut output, &updates);
        
        // Try with JUMBF removal
        let updates = Updates::new().remove_jumbf();
        let mut output = Vec::new();
        let _ = asset.write(&mut output, &updates);
        
        // Try with XMP removal
        let updates = Updates::new().remove_xmp();
        let mut output = Vec::new();
        let _ = asset.write(&mut output, &updates);
    }
});
