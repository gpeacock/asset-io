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
