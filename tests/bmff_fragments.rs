// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

#![cfg(feature = "bmff")]

use asset_io::containers::bmff::{self, fragment_layout, BmffUpdates};
use asset_io::{
    ByteRange, Error, HandlerRegistry, ItemKind, NullSink, Progress, Structure, Task, Updates,
};
use std::io::Cursor;

fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(ty);
    out.extend_from_slice(body);
    out
}

fn full(ty: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut data = flags.to_be_bytes();
    data[0] = version;
    boxed(ty, &[data.as_slice(), body].concat())
}

fn type_box(ty: &[u8; 4]) -> Vec<u8> {
    boxed(ty, b"iso6\0\0\0\0iso6dash")
}

fn xmp(data: &[u8]) -> Vec<u8> {
    HandlerRegistry::builtin()
        .compose("mp4", ItemKind::Xmp, data)
        .unwrap()
}

fn moof(base: Option<u64>, offset: i32, saio: Option<(u8, u64)>) -> Vec<u8> {
    let mut header = 1u32.to_be_bytes().to_vec();
    if let Some(base) = base {
        header.extend_from_slice(&base.to_be_bytes());
    }
    let mut traf = full(
        b"tfhd",
        0,
        if base.is_some() { 1 } else { 0x020000 },
        &header,
    );
    traf.extend(full(b"tfdt", 1, 0, &123456789u64.to_be_bytes()));
    let mut run = 1u32.to_be_bytes().to_vec();
    run.extend(offset.to_be_bytes());
    run.extend(5u32.to_be_bytes());
    traf.extend(full(b"trun", 0, 0x201, &run));
    if let Some((version, offset)) = saio {
        let mut entries = 1u32.to_be_bytes().to_vec();
        if version == 0 {
            entries.extend((offset as u32).to_be_bytes());
        } else {
            entries.extend(offset.to_be_bytes());
        }
        traf.extend(full(b"saio", version, 0, &entries));
    }
    boxed(
        b"moof",
        &[
            full(b"mfhd", 0, 0, &1u32.to_be_bytes()),
            boxed(b"traf", &traf),
        ]
        .concat(),
    )
}

fn media_segment(prefix: &[u8]) -> Vec<u8> {
    let header = moof(None, 0, None);
    [
        prefix,
        &moof(None, (header.len() + 8) as i32, None),
        &boxed(b"mdat", b"MEDIA"),
    ]
    .concat()
}

fn parse(data: &[u8]) -> Structure {
    HandlerRegistry::builtin()
        .parse_detect(&mut Cursor::new(data))
        .unwrap()
}

fn drive<T>(mut task: Task<'_, T>, source: &[u8]) -> T {
    loop {
        match task.resume().unwrap() {
            Progress::Read(mut request) => {
                let start = request.offset() as usize;
                let end = (start + request.window_len()).min(source.len());
                let buffer = request.buffer();
                buffer.clear();
                buffer.extend_from_slice(&source[start..end]);
            }
            Progress::Done(result) => return result,
            _ => unreachable!(),
        }
    }
}

fn rewrite(source: &[u8], updates: &Updates) -> (Vec<u8>, Structure) {
    let structure = parse(source);
    let planned = structure.plan(&mut Cursor::new(source), updates).unwrap();
    let plan = drive(structure.write_plan_task(updates), source);
    asset_io::plan::validate(&plan, source.len() as u64).unwrap();
    let mut output = Cursor::new(Vec::new());
    let written = structure
        .write(
            &mut Cursor::new(source),
            &mut output,
            updates,
            &mut NullSink,
        )
        .unwrap();
    let bytes = output.into_inner();
    let reparsed = parse(&bytes);
    for candidate in [&planned, plan.structure(), &written] {
        assert_eq!(candidate.segments(), reparsed.segments());
        assert_eq!(
            fragment_layout(candidate).unwrap(),
            fragment_layout(&reparsed).unwrap()
        );
        assert_eq!(candidate.media_type(), reparsed.media_type());
    }
    let mut stepped = Vec::new();
    plan.execute(&mut Cursor::new(source), &mut stepped, &mut NullSink)
        .unwrap();
    assert_eq!(stepped, bytes);
    (bytes, written)
}

fn fields(data: &[u8], path: &str, skip: u64, width: usize) -> Vec<u64> {
    let structure = parse(data);
    let mut values = Vec::new();
    structure
        .walk(&mut Cursor::new(data), |node| {
            if node.path == path {
                let at = (node.range.offset() + node.header_len as u64 + 4 + skip) as usize;
                values.push(
                    data[at..at + width]
                        .iter()
                        .fold(0, |a, b| a << 8 | *b as u64),
                );
            }
            Ok(())
        })
        .unwrap();
    values
}

#[test]
fn recognizes_and_roundtrips_init_and_media_streams() {
    let registry = HandlerRegistry::builtin();
    for format in ["m4s", "cmfv", "cmfa", "video/iso.segment"] {
        assert_eq!(registry.container_for(format), Some("bmff"));
    }
    let init = [type_box(b"ftyp"), boxed(b"moov", &boxed(b"mvex", &[]))].concat();
    let layout = fragment_layout(&parse(&init)).unwrap();
    assert!(layout.fragments().is_empty());
    assert_eq!(
        layout.prefix(),
        Some(ByteRange::new(0, init.len() as u64).unwrap())
    );
    let (signed_init, _) = rewrite(&init, &Updates::new().set_jumbf(b"INIT".to_vec()));
    assert!(fragment_layout(&parse(&signed_init))
        .unwrap()
        .fragments()
        .is_empty());

    let segment_len = media_segment(&[]).len() as u32;
    for prefix in [
        Vec::new(),
        type_box(b"styp"),
        sidx(0, 0, &[segment_len]),
        full(b"emsg", 0, 0, b"event\0value\0\0\0\0\0"),
        full(b"prft", 1, 0, &[0; 20]),
    ] {
        let data = media_segment(&prefix);
        let detected = drive(registry.parse_detect_task(data.len() as u64), &data);
        assert_eq!(detected.media_type(), "video/iso.segment");
        let layout = fragment_layout(&detected).unwrap();
        assert_eq!(layout.fragments().len(), 1);
        assert_eq!(layout.fragments()[0].range().offset(), prefix.len() as u64);
        let updates = Updates::new()
            .set_jumbf(b"STORE".to_vec())
            .set_xmp(b"XMP".to_vec());
        let (output, written) = rewrite(&data, &updates);
        let item = written.item(ItemKind::Jumbf).unwrap().unwrap();
        assert_eq!(
            written.read_item(&mut Cursor::new(&output), item).unwrap(),
            b"STORE"
        );
        let layout = fragment_layout(&written).unwrap();
        let fragment = &layout.fragments()[0];
        let offset = fields(&output, "/moof/traf/trun", 4, 4)[0];
        assert_eq!(
            &output[(fragment.moof().offset() + offset) as usize..][..5],
            b"MEDIA"
        );
        let (removed, _) = rewrite(&output, &Updates::remove_all());
        assert_eq!(removed, data);
        asset_io::testing::Conformance::new(bmff::handler())
            .jumbf(Some(b"STORE".to_vec()))
            .mutations(false)
            .check("DASH media segment", &data);
    }
}

fn random_access(version: u8, offsets: &[u64]) -> Vec<u8> {
    let mut body = 1u32.to_be_bytes().to_vec();
    body.extend(6u32.to_be_bytes()); // variable-width traf/trun/sample numbers
    body.extend((offsets.len() as u32).to_be_bytes());
    for (i, offset) in offsets.iter().enumerate() {
        if version == 0 {
            body.extend((i as u32).to_be_bytes());
            body.extend((*offset as u32).to_be_bytes());
        } else {
            body.extend((i as u64).to_be_bytes());
            body.extend(offset.to_be_bytes());
        }
        body.extend([1, 0, 1, 0, 0, 1]);
    }
    let tfra = full(b"tfra", version, 0, &body);
    let mfro = full(b"mfro", 0, 0, &((8 + tfra.len() + 16) as u32).to_be_bytes());
    boxed(b"mfra", &[tfra, mfro].concat())
}

#[test]
fn internal_fragments_and_each_random_access_entry_follow_the_write() {
    let init = [type_box(b"ftyp"), boxed(b"moov", &boxed(b"mvex", &[]))].concat();
    let fragment = media_segment(&[]);
    let starts = [init.len() as u64, (init.len() + fragment.len()) as u64];
    for version in [0, 1] {
        let source = [
            init.clone(),
            fragment.clone(),
            fragment.clone(),
            random_access(version, &starts),
        ]
        .concat();
        let original = fragment_layout(&parse(&source)).unwrap();
        assert_eq!(original.fragments().len(), 2);
        assert_eq!(original.prefix().unwrap().len(), init.len() as u64);
        assert_eq!(original.fragments()[0].range().len(), fragment.len() as u64);
        assert_eq!(original.fragments()[1].range().end(), source.len() as u64);
        let (output, written) = rewrite(&source, &Updates::new().set_jumbf(b"STORE".to_vec()));
        let layout = fragment_layout(&written).unwrap();
        let width = if version == 0 { 4 } else { 8 };
        let entry_start = 12 + width;
        let stride = 2 * width + 6;
        for (i, start) in starts.iter().enumerate() {
            assert_eq!(layout.fragments()[i].range().offset(), start + 50);
            assert_eq!(
                fields(
                    &output,
                    "/mfra/tfra",
                    entry_start + i as u64 * stride,
                    width as usize
                )[0],
                layout.fragments()[i].moof().offset()
            );
        }
        assert_eq!(
            fields(&source, "/mfra/mfro", 0, 4),
            fields(&output, "/mfra/mfro", 0, 4)
        );
        assert_eq!(
            fields(&source, "/moof/traf/tfdt", 0, 8),
            fields(&output, "/moof/traf/tfdt", 0, 8)
        );
    }
}

fn sidx(version: u8, first: u64, sizes: &[u32]) -> Vec<u8> {
    let mut body = 1u32.to_be_bytes().to_vec();
    body.extend(1000u32.to_be_bytes());
    if version == 0 {
        body.extend(0u32.to_be_bytes());
        body.extend((first as u32).to_be_bytes());
    } else {
        body.extend(0u64.to_be_bytes());
        body.extend(first.to_be_bytes());
    }
    body.extend([0, 0]);
    body.extend((sizes.len() as u16).to_be_bytes());
    for size in sizes {
        body.extend(size.to_be_bytes());
        body.extend(1000u32.to_be_bytes());
        body.extend(0x9000_0000u32.to_be_bytes());
    }
    full(b"sidx", version, 0, &body)
}

#[test]
fn indexes_runs_and_auxiliary_offsets_survive_nonuniform_growth_and_shrinkage() {
    for version in [0, 1] {
        for explicit in [false, true] {
            let metadata = xmp(b"OLD");
            let spacer = boxed(b"free", b"SPACER");
            let header_len = moof(explicit.then_some(0), 0, Some((version, 0))).len();
            let media = boxed(b"mdat", b"MEDIA");
            let second = media_segment(&[]);
            let sizes = [
                (header_len + metadata.len() + media.len()) as u32,
                second.len() as u32,
            ];
            let index = sidx(version, spacer.len() as u64, &sizes);
            let prefix = [type_box(b"styp"), index, spacer].concat();
            let base = prefix.len() as u64;
            let offset = (header_len + metadata.len() + 8) as i32;
            let header = moof(
                explicit.then_some(base),
                offset,
                Some((version, offset as u64)),
            );
            let source = [prefix, header, metadata, media, second].concat();
            for updates in [
                Updates::new()
                    .set_xmp(vec![b'X'; 200])
                    .set_jumbf(b"STORE".to_vec()),
                Updates::new().remove_xmp(),
            ] {
                let (output, written) = rewrite(&source, &updates);
                let layout = fragment_layout(&written).unwrap();
                let first = &layout.fragments()[0];
                let trun = fields(&output, "/moof/traf/trun", 4, 4)[0];
                let saio = fields(
                    &output,
                    "/moof/traf/saio",
                    4,
                    if version == 0 { 4 } else { 8 },
                )[0];
                assert_eq!(trun, saio);
                assert_eq!(
                    &output[(first.moof().offset() + trun) as usize..][..5],
                    b"MEDIA"
                );
                if explicit {
                    assert_eq!(
                        fields(&output, "/moof/traf/tfhd", 4, 8)[0],
                        first.moof().offset()
                    );
                }
                let width = if version == 0 { 4 } else { 8 };
                let offset = fields(&output, "/sidx", 8 + width, width as usize)[0];
                let sidx_box = written
                    .segments()
                    .iter()
                    .find(|s| s.name() == asset_io::Name::FourCC(*b"sidx"))
                    .unwrap();
                assert_eq!(sidx_box.range().end() + offset, first.moof().offset());
                let size = fields(&output, "/sidx", 12 + 2 * width, 4)[0];
                assert_eq!(size, first.range().len());
                assert_eq!(
                    fields(&output, "/sidx", 12 + 2 * width + 12, 4)[0],
                    sizes[1] as u64
                );
            }
        }
    }
}

#[test]
fn signed_track_run_offsets_can_point_before_the_moof() {
    let prefix = type_box(b"styp");
    let target = prefix.len() as u64 + 8;
    let before = [prefix, boxed(b"mdat", b"MEDIA"), xmp(b"OLD")].concat();
    let offset = (target as i64 - before.len() as i64) as i32;
    let source = [before, moof(None, offset, None)].concat();
    let (output, written) = rewrite(&source, &Updates::new().set_xmp(vec![b'X'; 100]));
    let adjusted = fields(&output, "/moof/traf/trun", 4, 4)[0] as u32 as i32;
    assert!(adjusted < offset);
    let start = fragment_layout(&written).unwrap().fragments()[0]
        .moof()
        .offset();
    assert_eq!((start as i64 + adjusted as i64) as u64, target);
}

#[test]
fn index_first_offset_changes_when_metadata_before_the_first_fragment_changes() {
    for version in [0, 1] {
        let fragment = media_segment(&[]);
        let metadata = xmp(b"OLD");
        let source = [
            type_box(b"styp"),
            sidx(version, metadata.len() as u64, &[fragment.len() as u32]),
            metadata,
            fragment,
        ]
        .concat();
        let (output, written) = rewrite(&source, &Updates::new().remove_xmp());
        let width = if version == 0 { 4 } else { 8 };
        assert_eq!(fields(&output, "/sidx", 8 + width, width as usize)[0], 0);
        assert_eq!(fragment_layout(&written).unwrap().fragments().len(), 1);
    }
}

#[test]
fn standalone_open_ended_media_can_take_merkle_boxes() {
    let mut source = media_segment(&[]);
    let mdat = source.len() - 13;
    source[mdat..mdat + 4].fill(0);
    let (output, written) = rewrite(
        &source,
        &Updates::new()
            .set_jumbf(b"STORE".to_vec())
            .set_merkle(vec![b"PROOF".to_vec()]),
    );
    assert_eq!(
        written
            .items_labelled(ItemKind::Other, bmff::MERKLE)
            .count(),
        1
    );
    assert_eq!(
        fields(&source, "/moof/traf/trun", 4, 4),
        fields(&output, "/moof/traf/trun", 4, 4)
    );
    assert_eq!(
        written
            .segments()
            .iter()
            .find(|s| s.name() == asset_io::Name::FourCC(*b"mdat"))
            .unwrap()
            .range()
            .len(),
        13
    );
}

#[test]
fn malformed_fragment_offsets_fail_before_writing() {
    let invalid = [
        full(b"tfhd", 0, 1, &1u32.to_be_bytes()),
        full(b"trun", 0, 1, &1u32.to_be_bytes()),
        full(b"saio", 0, 0, &10u32.to_be_bytes()),
    ];
    for bad in invalid {
        let mut traf = full(b"tfhd", 0, 0x020000, &1u32.to_be_bytes());
        if &bad[4..8] == b"tfhd" {
            traf.clear();
        }
        traf.extend(bad);
        let source = [
            type_box(b"styp"),
            boxed(b"moof", &boxed(b"traf", &traf)),
            boxed(b"mdat", b"MEDIA"),
        ]
        .concat();
        let structure = parse(&source);
        assert!(structure
            .write_plan(
                &mut Cursor::new(&source),
                &Updates::new().set_jumbf(b"STORE".to_vec())
            )
            .is_err());
    }
    for index in [
        full(b"sidx", 2, 0, &[0; 24]),
        full(b"sidx", 0, 0, &[0; 4]),
        sidx(0, 0, &[0x7fff_ffff]),
    ] {
        let source = [type_box(b"styp"), index, media_segment(&[])].concat();
        let structure = parse(&source);
        assert!(structure
            .write_plan(
                &mut Cursor::new(&source),
                &Updates::new().set_jumbf(b"STORE".to_vec())
            )
            .is_err());
    }
}

#[test]
fn inherited_data_bases_reject_nonuniform_relocation() {
    let first = moof(None, 0, None);
    let mut second = full(b"tfhd", 0, 0, &2u32.to_be_bytes());
    second.extend(full(b"trun", 0, 1, &[1, 0, 0, 0, 0, 0, 0, 0]));
    let mut header = first[8..].to_vec();
    header.extend(boxed(b"traf", &second));
    let source = [
        type_box(b"styp"),
        boxed(b"moof", &header),
        xmp(b"OLD"),
        boxed(b"mdat", b"MEDIA"),
    ]
    .concat();
    let structure = parse(&source);
    let error = structure
        .write_plan(&mut Cursor::new(&source), &Updates::new().remove_xmp())
        .unwrap_err();
    assert!(matches!(error, Error::UnsupportedFeature(_)));
}

#[test]
fn extended_uuid_prefixes_are_detected_and_can_be_removed() {
    let metadata = HandlerRegistry::builtin()
        .compose("m4s", ItemKind::Jumbf, b"STORE")
        .unwrap();
    let mut extended = 1u32.to_be_bytes().to_vec();
    extended.extend(b"uuid");
    extended.extend((metadata.len() as u64 + 8).to_be_bytes());
    extended.extend(&metadata[8..]);
    let segment = media_segment(&[]);
    let source = [extended, segment.clone()].concat();
    assert_eq!(parse(&source).media_type(), "video/iso.segment");
    let (output, _) = rewrite(&source, &Updates::new().remove_jumbf());
    assert_eq!(output, segment);
}

#[test]
fn ssix_ranges_allow_uniform_moves_but_reject_unresolved_size_changes() {
    let metadata = xmp(b"OLD");
    let header = moof(
        None,
        (moof(None, 0, None).len() + metadata.len() + 8) as i32,
        None,
    );
    let fragment = [header, metadata, boxed(b"mdat", b"MEDIA")].concat();
    let mut ranges = 1u32.to_be_bytes().to_vec();
    ranges.extend(1u32.to_be_bytes());
    ranges.extend((fragment.len() as u32).to_be_bytes());
    let ssix = full(b"ssix", 0, 0, &ranges);
    let source = [
        type_box(b"styp"),
        sidx(0, ssix.len() as u64, &[fragment.len() as u32]),
        ssix,
        fragment,
    ]
    .concat();
    rewrite(&source, &Updates::new().set_jumbf(b"STORE".to_vec()));
    assert!(matches!(
        parse(&source).write_plan(&mut Cursor::new(&source), &Updates::new().remove_xmp()),
        Err(Error::UnsupportedFeature(_))
    ));
}

#[test]
fn unsigned_relative_maps_cancel_edits_before_the_base() {
    let mut map = asset_io::plan::OffsetMap::new();
    map.add(10, 20);
    map.add(30, -5);
    map.add(90, 8);
    for base in [0, 10, 20, 50, 100] {
        let relative = map.relative_to(base);
        for offset in [0, 1, 25, 100] {
            assert_eq!(
                relative.apply(offset).unwrap(),
                map.apply(base + offset).unwrap() - map.apply(base).unwrap()
            );
        }
    }
}
