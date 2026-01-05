#![no_main]

use libfuzzer_sys::fuzz_target;
use asset_io::MiniXmp;

fuzz_target!(|data: &[u8]| {
    // Try to parse as UTF-8 XMP
    if let Ok(xmp_str) = std::str::from_utf8(data) {
        let xmp = MiniXmp::new(xmp_str);
        
        // Try various XMP operations
        let _ = xmp.get("dc:title");
        let _ = xmp.get("dc:creator");
        let _ = xmp.get("dc:format");
        
        // Try modifications - these should all be safe
        let _ = xmp.set("dc:title", "Test");
        let _ = xmp.set("dc:creator", "Fuzzer");
        let _ = xmp.remove("dc:format");
        let _ = xmp.remove("nonexistent");
        
        // Try batch updates
        let updates = vec![
            ("dc:title", Some("New Title")),
            ("dc:creator", Some("New Creator")),
            ("dc:format", None),  // Remove
        ];
        let _ = xmp.apply_updates(&updates);
        
        // Try with potentially malicious keys
        let _ = xmp.get("");
        let _ = xmp.get("x".repeat(1000).as_str());
        let _ = xmp.set("", "");
        let _ = xmp.set("x".repeat(1000).as_str(), "value");
    }
});
