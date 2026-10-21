#![no_main]

//! Multi-Picture Format: the fuzzer's bytes are the MPF index of an otherwise valid
//! two-image JPEG, followed by the trailing data. Parsing and copying must always
//! work; an edit may be refused, but one that is accepted must write a file that
//! parses to the layout `write` reported.

use asset_io::{HandlerRegistry, NullSink, Updates};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

/// The largest block an APP2 segment can hold after `MPF\0`.
const MAX_BLOCK: usize = 60_000;

fn marker(m: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, m];
    v.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// A small complete JPEG, with the MPF block in an APP2 segment when given.
fn image(mpf: Option<&[u8]>) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend(marker(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
    if let Some(block) = mpf {
        let mut p = b"MPF\0".to_vec();
        p.extend_from_slice(block);
        v.extend(marker(0xE2, &p));
    }
    v.extend(marker(0xDB, &[0u8; 65]));
    v.extend(marker(0xDA, &[1, 1, 0, 0, 0x3F, 0]));
    v.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00, 0x56, 0xFF, 0xD9]);
    v
}

fuzz_target!(|data: &[u8]| {
    let (block, tail) = data.split_at(data.len() * 3 / 4);
    let block = &block[..block.len().min(MAX_BLOCK)];
    let mut file = image(Some(block));
    file.extend(image(None));
    file.extend_from_slice(tail);

    let registry = HandlerRegistry::builtin();
    let Ok(structure) = registry.parse("jpg", &mut Cursor::new(&file)) else {
        return;
    };

    let cases = [
        Updates::new(),
        Updates::new().set_jumbf(vec![0, 0, 0, 16, b'j', b'u', b'm', b'b', 1, 2, 3, 4, 5, 6, 7, 8]),
        Updates::new().set_xmp(b"<x:xmpmeta/>".to_vec()),
        Updates::remove_all(),
    ];
    for updates in &cases {
        let mut out = Cursor::new(Vec::new());
        let Ok(dest) = structure.write(&mut Cursor::new(&file), &mut out, updates, &mut NullSink)
        else {
            continue;
        };
        let out = out.into_inner();
        assert_eq!(dest.total_size(), out.len() as u64);
        let again = registry
            .parse("jpg", &mut Cursor::new(&out))
            .expect("the output of an accepted edit parses");
        assert_eq!(again.subassets(), dest.subassets());
    }
});
