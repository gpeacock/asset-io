// Copyright 2026 Adobe. All rights reserved.
// This file is licensed under the Apache License, Version 2.0 or the MIT license,
// at your option.

#![cfg(all(feature = "bmff", feature = "jpeg"))]

use asset_io::{
    ByteRange, Error, HandlerRegistry, Name, NullSink, ScanLimits, SegmentKind, StructureBuilder,
    Updates,
};
use std::io::{Cursor, Read, Seek, SeekFrom};

const URI: &[u8] = b"tag:apple.com,2025:photo:metadata:provenance:processedimage\0";

fn boxed(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = ((data.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    out
}

fn full(ty: &[u8; 4], version: u8, data: &[u8]) -> Vec<u8> {
    boxed(ty, &[&[version, 0, 0, 0], data].concat())
}

fn packet(en: u16, z: u32, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, 0xeb];
    out.extend_from_slice(&((data.len() + 10) as u16).to_be_bytes());
    out.extend_from_slice(b"JP");
    out.extend_from_slice(&en.to_be_bytes());
    out.extend_from_slice(&z.to_be_bytes());
    out.extend_from_slice(data);
    out
}

fn uuid_box(value: u8, len: usize) -> Vec<u8> {
    boxed(b"uuid", &vec![value; len.max(16)])
}

fn jpeg() -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut boxes = vec![uuid_box(1, 30), uuid_box(2, 70_000), uuid_box(3, 40)];
    // Continuation data resembling a superbox header is still UUID box data.
    boxes[1][30_004..30_008].copy_from_slice(b"jumb");
    let mut jpeg = vec![0xff, 0xd8];
    // Interleaved box instances and out-of-order continuation packets.
    jpeg.extend(packet(1, 1, &boxes[0]));
    jpeg.extend(packet(2, 1, &boxes[1][..30_000]));
    jpeg.extend(packet(3, 1, &boxes[2]));
    jpeg.extend(packet(2, 3, &boxes[1][60_000..]));
    jpeg.extend(packet(2, 2, &boxes[1][30_000..60_000]));
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    (jpeg, boxes)
}

/// One item split over two extents in reverse physical order, with unrelated
/// bytes between them. Its logical stream must not include that padding.
fn heif(data: &[u8], ty: &[u8; 4], extra: &[u8]) -> Vec<u8> {
    let ftyp = boxed(b"ftyp", b"heic\0\0\0\0heicmif1");
    let split = data.len() / 2;
    let info = full(
        b"infe",
        2,
        &[&[0, 1, 0, 0], ty.as_slice(), &[0], extra].concat(),
    );
    let iinf = full(b"iinf", 0, &[&[0, 1], info.as_slice()].concat());
    let meta = |base: u32| {
        let mut loc = vec![0x44, 0, 0, 1, 0, 1, 0, 0, 0, 2];
        loc.extend_from_slice(&(base + (data.len() - split) as u32 + 7).to_be_bytes());
        loc.extend_from_slice(&(split as u32).to_be_bytes());
        loc.extend_from_slice(&base.to_be_bytes());
        loc.extend_from_slice(&((data.len() - split) as u32).to_be_bytes());
        full(b"meta", 0, &[iinf.clone(), full(b"iloc", 0, &loc)].concat())
    };
    let base = (ftyp.len() + meta(0).len() + 8) as u32;
    [
        ftyp,
        meta(base),
        boxed(
            b"mdat",
            &[&data[split..], &[0xfe; 7], &data[..split]].concat(),
        ),
    ]
    .concat()
}

#[test]
fn discovers_uri_image_and_reads_raw_boxes_through_fragmented_extents() {
    let (jpeg, expected) = jpeg();
    let file = heif(&jpeg, b"uri ", URI);
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(&file);
    let root = registry.parse_detect(&mut src).unwrap();
    assert!(root.entities(SegmentKind::Thumbnail).next().is_none());
    let entity = root.embedded_assets().next().unwrap();
    assert_eq!(entity.kind(), SegmentKind::Embedded);
    assert_eq!(entity.media_type(), None);
    assert_eq!(
        entity.label(),
        Some(std::str::from_utf8(&URI[..URI.len() - 1]).unwrap())
    );
    assert_eq!(root.read_entity(&mut src, entity).unwrap(), jpeg);

    let assets = root
        .scan_embedded(&mut src, &registry, ScanLimits::default())
        .unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!((assets[0].parent, assets[0].depth), (None, 1));
    assert_eq!(assets[0].ranges().len(), 2);
    let child = assets[0].structure.as_ref().unwrap();
    assert_eq!(child.media_type(), "image/jpeg");
    let entities: Vec<_> = child.entities(SegmentKind::Jumbf).collect();
    assert_eq!(entities.len(), 3);
    for (entity, expected) in entities.iter().zip(expected) {
        assert_eq!(entity.jumbf_type(), None);
        assert_eq!(
            child
                .read_entity(&mut assets[0].reader(&mut src).unwrap(), entity)
                .unwrap(),
            expected
        );
    }
    assert_eq!(entities[1].part_count(), 3);
}

#[test]
fn recursively_maps_nested_fragmented_items_to_original_source() {
    let (jpeg, expected) = jpeg();
    let inner = heif(&jpeg, b"jpeg", b"");
    let file = heif(&inner, b"mime", b"image/heic\0");
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(&file);
    let root = registry.parse_detect(&mut src).unwrap();
    let assets = root
        .scan_embedded(&mut src, &registry, ScanLimits::default())
        .unwrap();
    assert_eq!(assets.len(), 2);
    assert_eq!((assets[1].parent, assets[1].depth), (Some(0), 2));
    let child = assets[1].structure.as_ref().unwrap();
    let mut reader = assets[1].reader(&mut src).unwrap();
    for (entity, expected) in child.entities(SegmentKind::Jumbf).zip(expected) {
        assert_eq!(child.read_entity(&mut reader, entity).unwrap(), expected);
    }
    reader.seek(SeekFrom::End(-2)).unwrap();
    let mut end = [0; 2];
    reader.read_exact(&mut end).unwrap();
    assert_eq!(end, [0xff, 0xd9]);
    assert_eq!(reader.read(&mut end).unwrap(), 0);
    reader.seek(SeekFrom::End(10)).unwrap();
    assert_eq!(reader.read(&mut end).unwrap(), 0);
    assert!(reader.seek(SeekFrom::Start(0)).is_ok());
    assert!(reader.seek(SeekFrom::Current(-1)).is_err());
}

#[test]
fn embedded_entities_and_contents_survive_metadata_relocation() {
    let (jpeg, _) = jpeg();
    for (ty, extra) in [
        (b"uri ", URI),
        (b"jpeg", b"".as_slice()),
        (b"mime", b"image/jpeg\0".as_slice()),
    ] {
        let file = heif(&jpeg, ty, extra);
        let registry = HandlerRegistry::builtin();
        let mut src = Cursor::new(&file);
        let root = registry.parse_detect(&mut src).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = root
            .write(
                &mut src,
                &mut out,
                &Updates::new().set_jumbf(vec![0; 100]),
                &mut NullSink,
            )
            .unwrap();
        let reparsed = registry.parse_detect(&mut out).unwrap();
        for structure in [&dest, &reparsed] {
            let entity = structure.embedded_assets().next().unwrap();
            assert_eq!(structure.read_entity(&mut out, entity).unwrap(), jpeg);
            assert_eq!(
                entity.label(),
                root.embedded_assets().next().unwrap().label()
            );
            assert!(structure
                .scan_embedded(&mut out, &registry, ScanLimits::default())
                .unwrap()[0]
                .structure
                .is_ok());
        }
    }
}

#[test]
fn scan_enforces_exact_depth_count_and_cumulative_length_budgets() {
    let (jpeg, _) = jpeg();
    let inner = heif(&jpeg, b"jpeg", b"");
    let file = heif(&inner, b"mime", b"image/heic\0");
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(&file);
    let root = registry.parse_detect(&mut src).unwrap();
    let limits = ScanLimits {
        max_depth: 2,
        max_assets: 2,
        max_total_bytes: (inner.len() + jpeg.len()) as u64,
    };
    assert_eq!(
        root.scan_embedded(&mut src, &registry, limits)
            .unwrap()
            .len(),
        2
    );
    for limits in [
        ScanLimits {
            max_depth: 1,
            ..limits
        },
        ScanLimits {
            max_assets: 1,
            ..limits
        },
        ScanLimits {
            max_total_bytes: limits.max_total_bytes - 1,
            ..limits
        },
    ] {
        assert!(matches!(
            root.scan_embedded(&mut src, &registry, limits),
            Err(Error::LimitExceeded { .. })
        ));
    }
}

#[test]
fn unsupported_and_malformed_children_remain_readable_and_report_errors() {
    for (data, unsupported) in [
        (b"not an image".as_slice(), true),
        (&[0xff, 0xd8, 0xff, 0xe1], false),
    ] {
        let file = heif(data, b"uri ", URI);
        let registry = HandlerRegistry::builtin();
        let mut src = Cursor::new(&file);
        let root = registry.parse_detect(&mut src).unwrap();
        let assets = root
            .scan_embedded(&mut src, &registry, ScanLimits::default())
            .unwrap();
        assert_eq!(assets.len(), 1);
        assert!(assets[0].structure.is_err());
        if unsupported {
            assert!(matches!(assets[0].structure, Err(Error::UnsupportedFormat)));
        }
        let mut raw = Vec::new();
        assets[0]
            .reader(&mut src)
            .unwrap()
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(raw, data);
    }
}

#[test]
fn uuid_transport_survives_unrelated_jpeg_metadata_edits() {
    let (jpeg, expected) = jpeg();
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(&jpeg);
    let root = registry.parse_detect(&mut src).unwrap();
    let mut out = Cursor::new(Vec::new());
    let dest = root
        .write(
            &mut src,
            &mut out,
            &Updates::new().set_xmp(b"<x/>".to_vec()),
            &mut NullSink,
        )
        .unwrap();
    let reparsed = registry.parse_detect(&mut out).unwrap();
    for structure in [&dest, &reparsed] {
        let actual: Vec<_> = structure
            .entities(SegmentKind::Jumbf)
            .map(|e| structure.read_entity(&mut out, e).unwrap())
            .collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn rejects_duplicate_uuid_packets_and_inconsistent_box_length() {
    let data = uuid_box(1, 40);
    for packets in [
        [packet(1, 1, &data), packet(1, 1, &data)].concat(),
        packet(1, 1, &data[..30]),
    ] {
        let jpeg = [&[0xff, 0xd8], packets.as_slice(), &[0xff, 0xd9]].concat();
        assert!(HandlerRegistry::builtin()
            .parse_detect(&mut Cursor::new(jpeg))
            .is_err());
    }
}

#[test]
fn uuid_transport_accepts_zero_based_sequences() {
    let data = uuid_box(1, 100);
    let jpeg = [
        vec![0xff, 0xd8],
        packet(1, 0, &data[..40]),
        packet(1, 1, &data[40..]),
        vec![0xff, 0xd9],
    ]
    .concat();
    let mut src = Cursor::new(jpeg);
    let structure = HandlerRegistry::builtin().parse_detect(&mut src).unwrap();
    let entity = structure.require_entity(SegmentKind::Jumbf).unwrap();
    assert_eq!(structure.read_entity(&mut src, entity).unwrap(), data);
}

#[test]
fn generic_handler_entities_scan_siblings_after_an_unsupported_item() {
    let (jpeg, _) = jpeg();
    let bytes = [b"not an image".as_slice(), &jpeg].concat();
    let registry = HandlerRegistry::builtin();
    let mut builder = StructureBuilder::new(
        registry.handler("jpg").unwrap().clone(),
        "image/jpeg",
        bytes.len() as u64,
    );
    let id = builder
        .push_span(SegmentKind::Other, Name::None, 0, bytes.len() as u64, 0)
        .unwrap();
    for (at, len) in [(0, 12), (12, jpeg.len() as u64)] {
        builder
            .entity(
                SegmentKind::Embedded,
                &[(id, ByteRange::new(at, len).unwrap())],
            )
            .unwrap();
    }
    let root = builder.finish().unwrap();
    let mut src = Cursor::new(&bytes);
    let assets = root
        .scan_embedded(&mut src, &registry, ScanLimits::default())
        .unwrap();
    assert_eq!(assets.len(), 2);
    assert!(matches!(assets[0].structure, Err(Error::UnsupportedFormat)));
    assert_eq!(
        assets[1].structure.as_ref().unwrap().media_type(),
        "image/jpeg"
    );
    let mut raw = Vec::new();
    assets[1]
        .reader(&mut src)
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(raw, jpeg);
    // A truncated underlying source must be an error, not success-shaped EOF.
    let mut short = Cursor::new(&bytes[..bytes.len() - 1]);
    let mut raw = Vec::new();
    assert!(assets[1]
        .reader(&mut short)
        .unwrap()
        .read_to_end(&mut raw)
        .is_err());
}
