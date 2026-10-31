//! Finding the C2PA manifest store among whatever JUMBF an asset holds.
#![cfg(all(
    feature = "jpeg",
    feature = "png",
    feature = "riff",
    feature = "svg",
    feature = "bmff"
))]

use asset_io::jumbf::C2PA_STORE_TYPE;
use asset_io::{Error, HandlerRegistry, ItemKind, NullSink, Updates};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

/// A JUMBF superbox of type `ty` holding one `json` child per entry of `children`.
fn superbox(ty: [u8; 16], children: &[&[u8]]) -> Vec<u8> {
    let mut d = ty.to_vec();
    d.push(3);
    d.extend_from_slice(b"c2pa\0");
    let mut body = boxed(b"jumd", &d);
    for c in children {
        body.extend_from_slice(&boxed(b"json", c));
    }
    boxed(b"jumb", &body)
}

fn write(format: &str, input: &[u8], updates: &Updates) -> Vec<u8> {
    let registry = HandlerRegistry::builtin();
    let structure = registry.parse(format, &mut Cursor::new(input)).unwrap();
    let mut out = Cursor::new(Vec::new());
    structure
        .write(&mut Cursor::new(input), &mut out, updates, &mut NullSink)
        .unwrap();
    out.into_inner()
}

#[test]
fn jpeg_store_is_found_beside_other_jumbf() {
    let other = superbox([1; 16], &[b"not c2pa"]);
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let with_other = write(
        "jpg",
        &[0xff, 0xd8, 0xff, 0xd9],
        &Updates::new().set_jumbf(other),
    );

    let registry = HandlerRegistry::builtin();
    let mut source = Cursor::new(&with_other);
    let structure = registry.parse("jpg", &mut source).unwrap();
    assert_eq!(structure.c2pa_item(&mut source).unwrap().map(|_| ()), None);
    assert_eq!(structure.c2pa_store(&mut source).unwrap(), None);

    // A store of another type is kept when a C2PA store is added.
    let both = write("jpg", &with_other, &Updates::new().set_jumbf(c2pa.clone()));
    let mut source = Cursor::new(&both);
    let structure = registry.parse("jpg", &mut source).unwrap();
    assert_eq!(structure.items_of(ItemKind::Jumbf).count(), 2);
    assert!(matches!(
        structure.store(&mut source),
        Err(Error::Ambiguous { .. })
    ));
    let item = structure.c2pa_item(&mut source).unwrap().unwrap();
    assert_eq!(item.jumbf_type(), Some(C2PA_STORE_TYPE));
    assert_eq!(structure.c2pa_store(&mut source).unwrap(), Some(c2pa));
}

#[test]
fn svg_store_is_found_through_its_base64_transfer_encoding() {
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let svg = write(
        "svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
        &Updates::new().set_jumbf(c2pa.clone()),
    );
    let mut source = Cursor::new(&svg);
    let structure = HandlerRegistry::builtin()
        .parse("svg", &mut source)
        .unwrap();
    assert!(structure.c2pa_item(&mut source).unwrap().is_some());
    assert_eq!(structure.c2pa_store(&mut source).unwrap(), Some(c2pa));
}

#[test]
fn bmff_original_and_update_read_as_one_c2pa_store() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    let Ok(fixture) = fs::read(path) else { return };
    let first = superbox(C2PA_STORE_TYPE, &[b"m1"]);
    let second = superbox(C2PA_STORE_TYPE, &[b"m1", b"m2"]);
    let none = write("heic", &fixture, &Updates::remove_all());
    let original = write("heic", &none, &Updates::new().set_jumbf(first.clone()));
    let updated = write(
        "heic",
        &original,
        &Updates::new().set_jumbf(second.clone()).as_update(),
    );

    let registry = HandlerRegistry::builtin();
    for (bytes, expected) in [
        (&none, None),
        (&original, Some(first)),
        (&updated, Some(second)),
    ] {
        let mut source = Cursor::new(bytes);
        let structure = registry.parse("heic", &mut source).unwrap();
        assert_eq!(structure.c2pa_store(&mut source).unwrap(), expected);
    }
    let mut source = Cursor::new(&updated);
    let structure = registry.parse("heic", &mut source).unwrap();
    let item = structure.c2pa_item(&mut source).unwrap().unwrap();
    assert_eq!(item.label(), Some(asset_io::labels::ORIGINAL));
}

/// A planned structure describes output that does not exist yet, so the C2PA store
/// must be identifiable without reading it: a reader with no bytes is enough.
#[test]
fn planned_stores_are_identified_without_reading_the_output() {
    let registry = HandlerRegistry::builtin();
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let mut inputs = vec![
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "png",
            b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec(),
        ),
        ("wav", b"RIFF\x04\0\0\0WAVE".to_vec()),
        (
            "svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#.to_vec(),
        ),
    ];
    let heic = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    if let Ok(fixture) = fs::read(heic) {
        inputs.push(("heic", write("heic", &fixture, &Updates::remove_all())));
    }
    for (format, input) in inputs {
        let signed = write(format, &input, &Updates::new().set_jumbf(c2pa.clone()));
        for (source, updates) in [
            (&input, Updates::new().set_jumbf(c2pa.clone())),
            // An existing store that is kept, or for BMFF, relabelled as the
            // original of an update, takes its type from the parsed source.
            (&signed, Updates::new()),
        ] {
            let mut file = Cursor::new(source);
            let structure = registry.parse(format, &mut file).unwrap();
            let planned = structure.plan(&mut file, &updates).unwrap();
            let item = planned.c2pa_item(&mut Cursor::new([])).unwrap();
            assert!(item.is_some(), "{format}");
        }
        if format == "heic" {
            let mut file = Cursor::new(&signed);
            let structure = registry.parse(format, &mut file).unwrap();
            let update = superbox(C2PA_STORE_TYPE, &[b"manifest", b"more"]);
            let planned = structure
                .plan(&mut file, &Updates::new().set_jumbf(update).as_update())
                .unwrap();
            let item = planned.c2pa_item(&mut Cursor::new([])).unwrap().unwrap();
            assert_eq!(item.label(), Some(asset_io::labels::ORIGINAL));
        }
    }
}
