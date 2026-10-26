// Copyright 2026 Adobe. All rights reserved.
// This file is licensed under the Apache License, Version 2.0 or the MIT license,
// at your option.

#![cfg(all(feature = "bmff", feature = "jpeg"))]

use asset_io::{
    ByteRange, Encoding, Error, HandlerRegistry, ItemInfo, ItemKind, Name, NullSink, ScanLimits,
    SegmentId, SegmentKind, Structure, StructureBuilder, TransferEncoding, Updates,
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
fn discovers_uri_image_and_reads_raw_boxes_through_multi_extent_items() {
    let (jpeg, expected) = jpeg();
    let file = heif(&jpeg, b"uri ", URI);
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(&file);
    let root = registry.parse_detect(&mut src).unwrap();
    assert!(root.embedded_thumbnail(&mut src).unwrap().is_none());
    // A URI declares no media type, so the item is opaque, not an embedded asset.
    assert_eq!(root.embedded_assets().count(), 0);
    let item = root.require_item(ItemKind::Other).unwrap();
    assert_eq!(item.encoding(), Encoding::Unknown);
    assert_eq!(*item.transfer(), TransferEncoding::Identity);
    assert_eq!(item.media_type(), None);
    assert_eq!(item.label(), None);
    assert_eq!(item.native_type(), Some(Name::FourCC(*b"uri ")));
    assert_eq!(item.native_id(), Some(1));
    assert_eq!(
        item.uri(),
        Some(std::str::from_utf8(&URI[..URI.len() - 1]).unwrap())
    );
    assert_eq!(root.read_item(&mut src, item).unwrap(), jpeg);

    let assets = root
        .scan_embedded(&mut src, &registry, ScanLimits::default())
        .unwrap();
    // The scan detects that the opaque item is a JPEG.
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].item.kind(), ItemKind::Other);
    assert_eq!((assets[0].parent, assets[0].depth), (None, 1));
    assert_eq!(assets[0].ranges().len(), 2);
    let child = assets[0].structure.as_ref().unwrap();
    assert_eq!(child.media_type(), "image/jpeg");
    // Apple's APP11 UUID boxes are plain ISO BMFF boxes, not JUMBF.
    assert_eq!(child.items_of(ItemKind::Jumbf).count(), 0);
    let items: Vec<_> = child.items_of(ItemKind::Other).collect();
    assert_eq!(items.len(), 3);
    for (item, expected) in items.iter().zip(expected) {
        assert_eq!(item.encoding(), Encoding::IsoBmff);
        assert_eq!(item.jumbf_type(), None);
        assert_eq!(
            child
                .read_item(&mut assets[0].reader(&mut src).unwrap(), item)
                .unwrap(),
            expected
        );
    }
    assert_eq!(items[1].extent_count(), 3);
    assert_eq!(assets[0].item.uri(), item.uri());
}

#[test]
fn recursively_maps_nested_multi_extent_items_to_original_source() {
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
    let mut n = 0;
    for (item, expected) in child.items_of(ItemKind::Other).zip(expected) {
        assert_eq!(child.read_item(&mut reader, item).unwrap(), expected);
        n += 1;
    }
    assert_eq!(n, 3);
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
fn embedded_items_and_contents_survive_metadata_relocation() {
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
        // The first HEIF item, embedded asset or opaque.
        let first = |s: &Structure| s.items().find(|e| e.native_id().is_some()).cloned();
        for structure in [&dest, &reparsed] {
            let item = first(structure).unwrap();
            assert_eq!(structure.read_item(&mut out, &item).unwrap(), jpeg);
            assert_eq!(item.info(), first(&root).unwrap().info());
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
        // A declared embedded asset is reported whether or not it parses.
        let file = heif(data, b"mime", b"image/png\0");
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
        assert_eq!(other_items(structure, &mut out), expected);
    }
}

/// The bytes of every `Other` item, which must be ISO BMFF boxes, in order.
fn other_items(s: &Structure, src: &mut Cursor<Vec<u8>>) -> Vec<Vec<u8>> {
    s.items_of(ItemKind::Other)
        .map(|e| {
            assert_eq!(e.encoding(), Encoding::IsoBmff);
            assert_eq!(e.jumbf_type(), None);
            s.read_item(src, e).unwrap()
        })
        .collect()
}

/// A JUMBF superbox of type `ty`, as a C2PA store would be.
fn superbox(ty: [u8; 16]) -> Vec<u8> {
    let mut jumd = ty.to_vec();
    jumd.push(0);
    boxed(b"jumb", &boxed(b"jumd", &jumd))
}

#[test]
fn uuid_boxes_are_not_jumbf_and_survive_jumbf_updates() {
    let (jpeg, expected) = jpeg();
    let store = superbox([7; 16]);
    let registry = HandlerRegistry::builtin();
    let root = registry.parse_detect(&mut Cursor::new(&jpeg)).unwrap();
    assert!(root.item(ItemKind::Jumbf).unwrap().is_none());
    let with_store = {
        let mut out = Cursor::new(Vec::new());
        root.write(
            &mut Cursor::new(&jpeg),
            &mut out,
            &Updates::new().set_jumbf(store.clone()),
            &mut NullSink,
        )
        .unwrap();
        out.into_inner()
    };
    for (input, updates, stores) in [
        (&jpeg, Updates::new().set_jumbf(store.clone()), 1),
        (&jpeg, Updates::new().remove_jumbf(), 0),
        (&with_store, Updates::new().set_jumbf(store.clone()), 1),
        (&with_store, Updates::new().remove_jumbf(), 0),
        (&with_store, Updates::new().remove_jumbf_of([7; 16]), 0),
        (&with_store, Updates::new().remove_jumbf_of([8; 16]), 1),
    ] {
        let src = registry.parse_detect(&mut Cursor::new(input)).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = src
            .write(&mut Cursor::new(input), &mut out, &updates, &mut NullSink)
            .unwrap();
        let reparsed = registry.parse_detect(&mut out).unwrap();
        for structure in [&dest, &reparsed] {
            assert_eq!(other_items(structure, &mut out), expected);
            assert_eq!(structure.items_of(ItemKind::Jumbf).count(), stores);
        }
        // Byte for byte: every UUID packet is still in the output.
        let bytes = out.into_inner();
        for (i, seg) in src.segments().iter().enumerate() {
            let id = SegmentId::from_index(i).unwrap();
            if src.items_in(id).any(|e| e.kind() == ItemKind::Other) {
                let r = seg.range();
                let packet = &input[r.offset() as usize..r.end() as usize];
                assert!(bytes.windows(packet.len()).any(|w| w == packet));
            }
        }
    }
}

#[test]
fn opaque_items_that_are_not_files_are_skipped() {
    for (data, ty, extra) in [
        (b"not an image".as_slice(), b"uri ", URI),
        (
            b"not an image".as_slice(),
            b"mime",
            b"nonsense\0".as_slice(),
        ),
        (&[0xff, 0xd8, 0xff][..], b"uri ", URI),
    ] {
        let file = heif(data, ty, extra);
        let registry = HandlerRegistry::builtin();
        let mut src = Cursor::new(&file);
        let root = registry.parse_detect(&mut src).unwrap();
        let item = root.require_item(ItemKind::Other).unwrap();
        assert_eq!(item.native_type(), Some(Name::FourCC(*ty)));
        let detected = data.starts_with(&[0xff, 0xd8, 0xff]);
        // Skipped items count against no limit.
        let limits = ScanLimits {
            max_assets: usize::from(detected),
            max_total_bytes: if detected { data.len() as u64 } else { 0 },
            ..ScanLimits::default()
        };
        let assets = root.scan_embedded(&mut src, &registry, limits).unwrap();
        assert_eq!(assets.len(), usize::from(detected));
        if detected {
            // Recognised, and then found malformed: the error is kept.
            assert!(assets[0].structure.is_err());
        }
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
    let item = structure.require_item(ItemKind::Other).unwrap();
    assert_eq!(structure.read_item(&mut src, item).unwrap(), data);
}

#[test]
fn generic_handler_items_scan_siblings_after_an_unsupported_item() {
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
            .item(
                ItemInfo::new(ItemKind::EmbeddedAsset, Encoding::Asset),
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

#[test]
fn transfer_encoded_items_are_not_parsed_or_detected() {
    let (jpeg, _) = jpeg();
    let registry = HandlerRegistry::builtin();
    // A declared asset stored gzip-compressed: reported, not parsed. (The bytes are
    // a plain JPEG here, which shows nothing tried to parse them.)
    let file = heif(&jpeg, b"mime", b"image/jpeg\0gzip\0");
    let mut src = Cursor::new(&file);
    let root = registry.parse_detect(&mut src).unwrap();
    let item = root.embedded_assets().next().unwrap();
    assert_eq!(*item.transfer(), TransferEncoding::Gzip);
    let assets = root
        .scan_embedded(&mut src, &registry, ScanLimits::default())
        .unwrap();
    assert_eq!(assets.len(), 1);
    assert!(matches!(
        assets[0].structure,
        Err(Error::UnsupportedFeature(_))
    ));
    let mut raw = Vec::new();
    assets[0]
        .reader(&mut src)
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(raw, jpeg, "the reader gives the stored bytes");

    // An opaque item with a transfer encoding is not a detection candidate.
    let file = heif(&jpeg, b"mime", b"nonsense\0gzip\0");
    let mut src = Cursor::new(&file);
    let root = registry.parse_detect(&mut src).unwrap();
    assert_eq!(
        *root.require_item(ItemKind::Other).unwrap().transfer(),
        TransferEncoding::Gzip
    );
    assert!(root
        .scan_embedded(&mut src, &registry, ScanLimits::default())
        .unwrap()
        .is_empty());
}
