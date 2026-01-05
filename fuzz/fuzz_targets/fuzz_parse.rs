#![no_main]

use libfuzzer_sys::fuzz_target;
use asset_io::Asset;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Try to parse any input as an asset (auto-detect format)
    // This should NEVER panic, only return errors
    let cursor = Cursor::new(data);
    
    if let Ok(mut asset) = Asset::from_source(cursor) {
        // Try to access various properties - these should all be safe
        let _ = asset.media_type();
        let _ = asset.container();
        let _ = asset.structure();
        
        // Try to read XMP if present
        let _ = asset.xmp();
        
        // Try to read JUMBF if present
        let _ = asset.jumbf();
        
        // Try to read embedded thumbnail
        let _ = asset.read_embedded_thumbnail();
        
        // Try EXIF if feature enabled
        #[cfg(feature = "exif")]
        {
            let _ = asset.exif_info();
        }
    }
});
