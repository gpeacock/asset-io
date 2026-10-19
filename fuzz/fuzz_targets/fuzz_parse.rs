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
