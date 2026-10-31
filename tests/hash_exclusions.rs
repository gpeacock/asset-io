#![cfg(all(
    feature = "jpeg",
    feature = "png",
    feature = "riff",
    feature = "svg",
    feature = "bmff",
    feature = "c2pa"
))]

use asset_io::{ByteRange, Error, HandlerRegistry, ItemKind, NullSink, Updates};
use std::io::Cursor;

fn store(len: usize) -> Vec<u8> {
    let mut bytes = (len as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(b"jumb");
    bytes.extend_from_slice(&25u32.to_be_bytes());
    bytes.extend_from_slice(b"jumd");
    bytes.extend_from_slice(&[7; 16]);
    bytes.push(0);
    bytes.resize(len, 0);
    bytes
}

#[test]
fn handler_exclusions_match_planned_written_and_parsed_geometry() {
    let registry = HandlerRegistry::builtin();
    for (format, input) in [
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
    ] {
        for len in [33, 34, 160_001] {
            let mut source = Cursor::new(&input);
            let structure = registry.parse(format, &mut source).unwrap();
            let updates = Updates::new().set_jumbf(store(len));
            let planned = structure.plan(&mut source, &updates).unwrap();
            let mut output = Cursor::new(Vec::new());
            let written = structure
                .write(&mut source, &mut output, &updates, &mut NullSink)
                .unwrap();
            let parsed = registry.parse(format, &mut output).unwrap();
            let item = planned.require_item(ItemKind::Jumbf).unwrap();
            let expected = planned.c2pa_data_hash_exclusions(item).unwrap();
            assert!(!expected.is_empty());
            for candidate in [&written, &parsed] {
                assert_eq!(
                    candidate
                        .c2pa_data_hash_exclusions(candidate.require_item(ItemKind::Jumbf).unwrap())
                        .unwrap(),
                    expected,
                    "{format}, {len}"
                );
            }
            assert!(matches!(
                written.c2pa_data_hash_exclusions(item),
                Err(Error::InvalidFormat(_))
            ));
            match format {
                "jpg" => {
                    // Adjacent multipart APP11 segments normalize into one range.
                    assert_eq!(expected.len(), 1);
                    assert_eq!(expected[0].offset(), 2);
                    assert_eq!(expected[0].end(), planned.total_size() - 2);
                    if len > 65_535 {
                        assert!(item.extent_count() > 1);
                    }
                }
                "png" => {
                    let extent = item.extents().next().unwrap().range();
                    assert_eq!(
                        expected,
                        [ByteRange::new(extent.offset() - 8, len as u64 + 12).unwrap()]
                    );
                }
                "wav" => {
                    assert_eq!(expected, [ByteRange::new(12, len as u64 + 8).unwrap()]);
                    assert_eq!(planned.total_size() - expected[0].end(), (len % 2) as u64);
                }
                "svg" => {
                    let extent = item.extents().next().unwrap().range();
                    assert_eq!(expected, [extent]);
                    assert_eq!(extent.len(), (len as u64).div_ceil(3) * 4);
                    let bytes = output.get_ref();
                    assert!(bytes[..extent.offset() as usize].ends_with(b"<c2pa:manifest>"));
                    assert!(bytes[extent.end() as usize..].starts_with(b"</c2pa:manifest>"));
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn containers_with_other_bindings_refuse_rather_than_guess() {
    let registry = HandlerRegistry::builtin();
    let mut sidecar = Cursor::new(store(33));
    let structure = registry.parse("c2pa", &mut sidecar).unwrap();
    let item = structure.require_item(ItemKind::Jumbf).unwrap();
    assert!(matches!(
        structure.c2pa_data_hash_exclusions(item),
        Err(Error::UnsupportedFeature(_))
    ));

    let mut mp4 = Cursor::new(b"\0\0\0\x18ftypisom\0\0\0\0isommp42".to_vec());
    let structure = registry.parse("mp4", &mut mp4).unwrap();
    let planned = structure
        .plan(&mut mp4, &Updates::new().set_jumbf(store(33)))
        .unwrap();
    let item = planned.require_item(ItemKind::Jumbf).unwrap();
    assert!(matches!(
        planned.c2pa_data_hash_exclusions(item),
        Err(Error::UnsupportedFeature(_))
    ));
}

#[test]
fn each_container_names_its_c2pa_hard_binding() {
    use asset_io::C2paHash;
    let registry = HandlerRegistry::builtin();
    for (format, input, expected) in [
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9], Some(C2paHash::Box)),
        (
            "png",
            b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec(),
            Some(C2paHash::Box),
        ),
        ("wav", b"RIFF\x04\0\0\0WAVE".to_vec(), Some(C2paHash::Box)),
        (
            "svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#.to_vec(),
            Some(C2paHash::Data),
        ),
        (
            "mp4",
            b"\0\0\0\x18ftypisom\0\0\0\0isommp42".to_vec(),
            Some(C2paHash::Bmff),
        ),
        ("c2pa", store(33), None),
    ] {
        let structure = registry.parse(format, &mut Cursor::new(input)).unwrap();
        assert_eq!(structure.c2pa_hash(), expected, "{format}");
    }
}

#[test]
fn c2pa_boxes_tile_the_asset_with_one_store_box() {
    use asset_io::{AFTER_BOX, C2PA_BOX};
    let registry = HandlerRegistry::builtin();
    for (format, input) in [
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "png",
            b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec(),
        ),
        ("wav", b"RIFF\x04\0\0\0WAVE".to_vec()),
    ] {
        for len in [33, 160_001] {
            let mut source = Cursor::new(&input);
            let structure = registry.parse(format, &mut source).unwrap();
            let mut c2pa = store(len);
            c2pa[16..32].copy_from_slice(&asset_io::jumbf::C2PA_STORE_TYPE);
            let updates = Updates::new().set_jumbf(c2pa);
            let planned = structure.plan(&mut source, &updates).unwrap();
            let mut output = Cursor::new(Vec::new());
            structure
                .write(&mut source, &mut output, &updates, &mut NullSink)
                .unwrap();
            let mut bytes = output.into_inner();
            let parsed = registry.parse(format, &mut Cursor::new(&bytes)).unwrap();

            let boxes = parsed.c2pa_boxes(&mut Cursor::new(&bytes)).unwrap();
            assert_eq!(
                planned.c2pa_boxes(&mut Cursor::new([])).unwrap(),
                boxes,
                "{format}"
            );
            let mut end = 0;
            for b in &boxes {
                assert_eq!(b.range.offset(), end, "{format}: boxes are contiguous");
                end = b.range.end();
            }
            assert_eq!(end, parsed.total_size());
            let store_boxes: Vec<_> = boxes.iter().filter(|b| b.name == C2PA_BOX).collect();
            assert_eq!(store_boxes.len(), 1, "{format}, {len}");
            // The store box covers whatever its data-hash exclusion does.
            let item = parsed.require_item(ItemKind::Jumbf).unwrap();
            let excluded = parsed.c2pa_data_hash_exclusions(item).unwrap();
            assert!(store_boxes[0].range.offset() <= excluded[0].offset());
            assert!(store_boxes[0].range.end() >= excluded[excluded.len() - 1].end());

            // Bytes after the structure are one final box. RIFF does not accept
            // arbitrary bytes after its last chunk.
            if format == "wav" {
                continue;
            }
            bytes.extend_from_slice(b"trailing");
            let parsed = registry.parse(format, &mut Cursor::new(&bytes)).unwrap();
            let boxes = parsed.c2pa_boxes(&mut Cursor::new(&bytes)).unwrap();
            let last = boxes.last().unwrap();
            assert_eq!(
                (last.name.as_ref(), last.range.len()),
                (AFTER_BOX, 8),
                "{format}"
            );
        }
    }
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
    let structure = registry.parse("svg", &mut Cursor::new(svg)).unwrap();
    assert!(matches!(
        structure.c2pa_boxes(&mut Cursor::new(svg)),
        Err(Error::UnsupportedFeature(_))
    ));
}

/// Box names and ranges, as `(name, offset, len)`.
type Named = Vec<(String, u64, u64)>;

fn named(boxes: &[asset_io::C2paBox]) -> Named {
    boxes
        .iter()
        .map(|b| (b.name.to_string(), b.range.offset(), b.range.len()))
        .collect()
}

/// Write `input` with a C2PA store, then return the planned boxes (read in the
/// source, before the output exists) and the boxes of the written output.
fn planned_and_written(format: &str, input: &[u8]) -> (Named, Named, Vec<u8>) {
    let registry = HandlerRegistry::builtin();
    let mut c2pa = store(40);
    c2pa[16..32].copy_from_slice(&asset_io::jumbf::C2PA_STORE_TYPE);
    let updates = Updates::new().set_jumbf(c2pa);
    let mut source = Cursor::new(input);
    let structure = registry.parse(format, &mut source).unwrap();
    let planned = structure.plan(&mut source, &updates).unwrap();
    let planned_boxes = named(&planned.c2pa_boxes(&mut source).unwrap());
    let mut output = Cursor::new(Vec::new());
    structure
        .write(&mut source, &mut output, &updates, &mut NullSink)
        .unwrap();
    let bytes = output.into_inner();
    let written = registry.parse(format, &mut Cursor::new(&bytes)).unwrap();
    let written_boxes = named(&written.c2pa_boxes(&mut Cursor::new(&bytes)).unwrap());
    (planned_boxes, written_boxes, bytes)
}

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn riff(form: &[u8; 4], kids: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = kids.concat();
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    v.extend_from_slice(form);
    v.extend_from_slice(&body);
    v
}

/// C2PA 2.4, 18.7.3.5: a RIFF chunk is a 12-byte box named by its form, a LIST
/// chunk a box named by its list type that holds everything nested in it, and
/// padding belongs to the chunk before it.
#[test]
fn riff_boxes_are_named_by_form_and_list_type() {
    let wav = riff(
        b"WAVE",
        &[
            chunk(b"fmt ", &[0; 16]),
            chunk(
                b"LIST",
                &[b"INFO".as_slice(), &chunk(b"INAM", b"abc")].concat(),
            ),
            chunk(b"data", &[1, 2, 3]),
        ],
    );
    let (planned, written, _) = planned_and_written("wav", &wav);
    assert_eq!(planned, written);
    let names: Vec<_> = written.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(names, ["WAVE", "fmt ", "INFO", "data", "C2PA"]);
    assert_eq!(written[0].2, 12);
    // LIST: header(8) + type(4) + INAM chunk(8 + 3 + pad).
    assert_eq!(written[2].2, 8 + 4 + 12);
    // The odd-sized data chunk gained a pad byte when the store followed it.
    assert_eq!(written[3].2, 8 + 3 + 1);

    let avi = [
        riff(
            b"AVI ",
            &[chunk(
                b"LIST",
                &[b"hdrl".as_slice(), &chunk(b"avih", &[0; 4])].concat(),
            )],
        ),
        riff(b"AVIX", &[chunk(b"LIST", b"movi")]),
    ]
    .concat();
    let (planned, written, _) = planned_and_written("avi", &avi);
    assert_eq!(planned, written);
    let names: Vec<_> = written.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(names, ["AVI ", "hdrl", "C2PA", "AVIX", "movi"]);
}

/// C2PA 2.4, 18.7.3.1: SOS and each RSTn are boxes with their entropy-coded data.
#[test]
fn jpeg_restart_markers_are_their_own_boxes() {
    let jpeg = [
        &[0xff, 0xd8][..],
        &[0xff, 0xdd, 0x00, 0x04, 0x00, 0x01],
        &[0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00],
        &[0x12, 0x34, 0xff, 0xd0, 0x56, 0xff, 0x00, 0xff, 0xd1, 0x78],
        &[0xff, 0xd9],
    ]
    .concat();
    let (planned, written, bytes) = planned_and_written("jpg", &jpeg);
    assert_eq!(planned, written);
    let names: Vec<_> = written.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(names, ["SOI", "C2PA", "DRI", "SOS", "RST0", "RST1", "EOI"]);
    let rst0 = &written[4];
    assert_eq!(&bytes[rst0.1 as usize..][..2], &[0xff, 0xd0]);
    // RST0 holds its marker, a byte of entropy-coded data and a stuffed 0xFF00.
    assert_eq!(rst0.2, 2 + 1 + 2);
}

/// C2PA 2.4, 18.7.3.1: a Multi-Picture Format JPEG lists every image's boxes;
/// only unnamed bytes at the end are `c2pa.after`.
#[test]
fn every_image_of_a_multi_picture_jpeg_is_listed() {
    let image = [0xff, 0xd8, 0xff, 0xd9];
    let mpf = [image, image].concat();
    let (planned, written, _) = planned_and_written("jpg", &mpf);
    assert_eq!(planned, written);
    let names: Vec<_> = written.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(names, ["SOI", "C2PA", "EOI", "SOI", "EOI"]);

    let trailing = [&mpf[..], b"junk"].concat();
    let (_, written, _) = planned_and_written("jpg", &trailing);
    let last = written.last().unwrap();
    assert_eq!((last.0.as_str(), last.2), ("c2pa.after", 4));
}

#[test]
fn boxes_report_their_asset_metadata() {
    let registry = HandlerRegistry::builtin();
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec();
    let mut source = Cursor::new(&png);
    let structure = registry.parse("png", &mut source).unwrap();
    let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"/>"#.to_vec();
    let mut output = Cursor::new(Vec::new());
    structure
        .write(
            &mut source,
            &mut output,
            &Updates::new().set_xmp(xmp.clone()),
            &mut NullSink,
        )
        .unwrap();
    let bytes = output.into_inner();
    let parsed = registry.parse("png", &mut Cursor::new(&bytes)).unwrap();
    let boxes = parsed.c2pa_boxes(&mut Cursor::new(&bytes)).unwrap();
    let itxt = boxes.iter().find(|b| b.name == "iTXt").unwrap();
    assert_eq!(itxt.metadata.len(), 1);
    let m = itxt.metadata[0];
    assert_eq!(&bytes[m.offset() as usize..m.end() as usize], &xmp[..]);
    assert!(boxes
        .iter()
        .filter(|b| b.name != "iTXt")
        .all(|b| b.metadata.is_empty()));
}
