#![no_main]

use asset_io::{Asset, ContainerKind, Updates};
use libfuzzer_sys::fuzz_target;
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

        if asset.container() == ContainerKind::Pdf {
            for updates in [
                Updates::new().set_jumbf(b"manifest".to_vec()),
                Updates::new().set_xmp(b"<xmp/>".to_vec()).remove_jumbf(),
                Updates::new().remove_xmp().remove_jumbf(),
            ] {
                let mut output = Cursor::new(Vec::new());
                if let Ok(expected) = asset.write(&mut output, &updates) {
                    assert!(output.get_ref().starts_with(data));
                    let actual = Asset::from_source(output).expect("written PDF must reparse");
                    assert_eq!(expected.total_size, actual.structure().total_size);
                    let layout = |structure: &asset_io::Structure| {
                        structure
                            .segments()
                            .iter()
                            .map(|segment| (segment.kind, segment.ranges.clone()))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(layout(&expected), layout(actual.structure()));
                }
            }
        }
    }
});
