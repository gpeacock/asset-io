// Copyright 2026 Adobe. All rights reserved.
// Licensed under the Apache License, Version 2.0 or the MIT license, at your option.

#![cfg(feature = "tiff")]

use asset_io::containers::tiff::{self, layout, ByteOrder, C2PA_TAG, XMP_TAG};
use asset_io::{
    Action, Error, HandlerRegistry, ItemKind, NullSink, Progress, Structure, Task, Updates,
};
use std::io::{self, Cursor, Read, Seek, SeekFrom};

#[derive(Clone, Copy)]
struct Format {
    little: bool,
    big: bool,
}
impl Format {
    fn width(self) -> usize {
        if self.big {
            8
        } else {
            4
        }
    }
    fn count_width(self) -> usize {
        if self.big {
            8
        } else {
            2
        }
    }
    fn entry_width(self) -> usize {
        if self.big {
            20
        } else {
            12
        }
    }
    fn header_len(self) -> usize {
        if self.big {
            16
        } else {
            8
        }
    }
    fn number(self, value: u64, width: usize) -> Vec<u8> {
        if self.little {
            value.to_le_bytes()[..width].to_vec()
        } else {
            value.to_be_bytes()[8 - width..].to_vec()
        }
    }
}
const FORMATS: [Format; 4] = [
    Format {
        little: true,
        big: false,
    },
    Format {
        little: false,
        big: false,
    },
    Format {
        little: true,
        big: true,
    },
    Format {
        little: false,
        big: true,
    },
];
type Tag = (u16, u16, Vec<u8>);

fn fixture(f: Format, pages: usize, extra: &[Tag]) -> Vec<u8> {
    let mut directories = Vec::new();
    for _ in 0..pages {
        let short = |tag, v| (tag, 3, f.number(v, 2));
        let long = |tag, v| (tag, 4, f.number(v, 4));
        let mut tags = vec![
            long(256, 2),
            long(257, 2),
            short(258, 8),
            short(259, 1),
            short(262, 1),
            long(273, 0),
            short(277, 1),
            long(278, 2),
            long(279, 4),
            (65000, 65000, vec![0x5a; f.width()]),
        ];
        if directories.is_empty() {
            tags.extend(extra.iter().cloned());
        }
        tags.sort_by_key(|t| t.0);
        directories.push(tags);
    }
    let mut offsets = Vec::new();
    let mut at = f.header_len();
    for tags in &directories {
        offsets.push(at);
        at += f.count_width() + tags.len() * f.entry_width() + f.width();
    }
    let mut bytes = vec![0; at];
    bytes[..2].copy_from_slice(if f.little { b"II" } else { b"MM" });
    bytes[2..4].copy_from_slice(&f.number(if f.big { 43 } else { 42 }, 2));
    if f.big {
        bytes[4..6].copy_from_slice(&f.number(8, 2));
    }
    let pointer = if f.big { 8 } else { 4 };
    bytes[pointer..pointer + f.width()]
        .copy_from_slice(&f.number(f.header_len() as u64, f.width()));
    for (page, tags) in directories.iter().enumerate() {
        let pixels = bytes.len() as u64;
        bytes.extend([0, 85, 170, 255]);
        let mut table = f.number(tags.len() as u64, f.count_width());
        for (tag, ty, data) in tags {
            let data = if *tag == 273 {
                f.number(pixels, 4)
            } else {
                data.clone()
            };
            let unit = match ty {
                3 => 2,
                4 | 13 => 4,
                16 | 18 => 8,
                _ => 1,
            };
            let count = if *ty == 65000 {
                1
            } else {
                data.len() as u64 / unit
            };
            table.extend(f.number(*tag as u64, 2));
            table.extend(f.number(*ty as u64, 2));
            table.extend(f.number(count, f.width()));
            if data.len() <= f.width() {
                table.extend(&data);
                table.resize(table.len() + f.width() - data.len(), 0);
            } else {
                if !bytes.len().is_multiple_of(8) {
                    bytes.resize(bytes.len().div_ceil(8) * 8, 0);
                }
                table.extend(f.number(bytes.len() as u64, f.width()));
                bytes.extend(data);
            }
        }
        table.extend(f.number(
            offsets.get(page + 1).copied().unwrap_or(0) as u64,
            f.width(),
        ));
        bytes[offsets[page]..offsets[page] + table.len()].copy_from_slice(&table);
    }
    bytes
}

fn parse(data: &[u8]) -> Structure {
    HandlerRegistry::builtin()
        .parse_detect(&mut Cursor::new(data))
        .unwrap()
}
fn drive<T>(mut task: Task<'_, T>, bytes: &[u8]) -> T {
    loop {
        match task.resume().unwrap() {
            Progress::Read(mut request) => {
                let at = request.offset() as usize;
                let end = (at + request.window_len()).min(bytes.len());
                let buffer = request.buffer();
                buffer.clear();
                buffer.extend_from_slice(&bytes[at..end]);
            }
            Progress::Done(result) => return result,
            _ => unreachable!(),
        }
    }
}
fn metadata(s: &Structure, bytes: &[u8], kind: ItemKind) -> Option<Vec<u8>> {
    s.item(kind)
        .unwrap()
        .map(|e| s.read_item(&mut Cursor::new(bytes), e).unwrap())
}
fn shape(s: &Structure) -> String {
    format!(
        "{:?} {:?} {}",
        s.segments(),
        s.items().collect::<Vec<_>>(),
        s.media_type()
    )
}
fn rewrite(bytes: &[u8], updates: &Updates) -> (Vec<u8>, Structure) {
    let source = parse(bytes);
    let planned = source.plan(&mut Cursor::new(bytes), updates).unwrap();
    let plan = drive(source.write_plan_task(updates), bytes);
    asset_io::plan::validate(&plan, bytes.len() as u64).unwrap();
    let mut out = Cursor::new(Vec::new());
    let written = source
        .write(&mut Cursor::new(bytes), &mut out, updates, &mut NullSink)
        .unwrap();
    let output = out.into_inner();
    let reparsed = parse(&output);
    for other in [&planned, plan.structure(), &written] {
        assert_eq!(shape(other), shape(&reparsed));
    }
    let mut stepping = plan.start().with_buffer_len(4096);
    let mut stepped = Vec::new();
    loop {
        match stepping.step(&mut NullSink).unwrap() {
            Action::Read { offset, buf } => {
                buf.copy_from_slice(&bytes[offset as usize..offset as usize + buf.len()])
            }
            Action::Write(buf) => stepped.extend_from_slice(buf),
            Action::Done => break,
            _ => unreachable!(),
        }
    }
    assert_eq!(stepped, output);
    (output, written)
}

#[test]
fn both_byte_orders_and_widths_read_write_and_conform() {
    for f in FORMATS {
        for pages in [1, 2, 3] {
            let bytes = fixture(f, pages, &[]);
            let source = parse(&bytes);
            let parsed = drive(
                HandlerRegistry::builtin().parse_detect_task(bytes.len() as u64),
                &bytes,
            );
            assert_eq!(shape(&source), shape(&parsed));
            assert_eq!(layout(&source).unwrap().is_big_tiff(), f.big);
            assert_eq!(
                layout(&source).unwrap().byte_order(),
                if f.little {
                    ByteOrder::Little
                } else {
                    ByteOrder::Big
                }
            );
            let (unchanged, _) = rewrite(&bytes, &Updates::new());
            assert_eq!(unchanged, bytes);
            let (signed, state) = rewrite(
                &bytes,
                &Updates::new()
                    .set_xmp(b"<xmp>metadata</xmp>".to_vec())
                    .set_jumbf(vec![0x5a; 100]),
            );
            assert_eq!(
                metadata(&state, &signed, ItemKind::Jumbf),
                Some(vec![0x5a; 100])
            );
            assert_eq!(
                metadata(&state, &signed, ItemKind::Xmp),
                Some(b"<xmp>metadata</xmp>".to_vec())
            );
            let map = layout(&state).unwrap();
            assert_eq!(map.pages().count(), pages + usize::from(pages > 1));
            let entry = map.manifest_entry().unwrap();
            assert_eq!(entry.field_type(), 7);
            assert_eq!(entry.count_range().len(), f.width() as u64);
            assert_eq!(entry.value_range().unwrap().end(), signed.len() as u64);
            let ptr = if f.big { 8 } else { 4 };
            assert_eq!(&signed[..ptr], &bytes[..ptr]);
            assert_eq!(
                &signed[ptr + f.width()..bytes.len()],
                &bytes[ptr + f.width()..]
            );
            let mut original_pages = layout(&source).unwrap().pages();
            for page in map.pages().take(pages) {
                let original = original_pages.next().unwrap();
                for tag in [273, 279, 65000] {
                    let a = original.entry(tag).unwrap().range();
                    let b = page.entry(tag).unwrap().range();
                    assert_eq!(
                        &bytes[a.offset() as usize..a.end() as usize],
                        &signed[b.offset() as usize..b.end() as usize]
                    );
                }
            }
            let (removed, state) = rewrite(&signed, &Updates::remove_all());
            assert!(metadata(&state, &removed, ItemKind::Jumbf).is_none());
            assert!(metadata(&state, &removed, ItemKind::Xmp).is_none());
            assert_eq!(layout(&state).unwrap().pages().count(), pages);
            asset_io::testing::Conformance::new(tiff::handler())
                .jumbf(Some(vec![0; 128]))
                .check("synthetic TIFF", &bytes);
        }
    }
}

#[test]
fn inline_metadata_and_same_size_placeholder_patching() {
    for f in FORMATS {
        for length in [0, 1, f.width(), f.width() + 1, 100] {
            let bytes = fixture(f, 1, &[]);
            let (signed, state) = rewrite(
                &bytes,
                &Updates::new()
                    .set_xmp(vec![b'X'; length])
                    .set_jumbf(vec![0; length]),
            );
            let range = layout(&state)
                .unwrap()
                .manifest_entry()
                .unwrap()
                .value_range()
                .unwrap();
            let (patched, updated) =
                rewrite(&signed, &Updates::new().set_jumbf(vec![0x5a; length]));
            assert_eq!(patched.len(), signed.len());
            assert_eq!(
                metadata(&updated, &patched, ItemKind::Jumbf),
                Some(vec![0x5a; length])
            );
            let mut expected = signed.clone();
            expected[range.offset() as usize..range.end() as usize].fill(0x5a);
            assert_eq!(patched, expected);
            let (moved, updated) = rewrite(&patched, &Updates::new().set_xmp(vec![b'Y'; 200]));
            assert_eq!(
                metadata(&updated, &moved, ItemKind::Jumbf),
                Some(vec![0x5a; length])
            );
            assert_eq!(
                metadata(&updated, &moved, ItemKind::Xmp),
                Some(vec![b'Y'; 200])
            );
            let item = updated.item(ItemKind::Jumbf).unwrap().unwrap();
            let mut cursor = Cursor::new(moved);
            updated
                .patch_exact(&mut cursor, item, &vec![0x33; length])
                .unwrap();
            let reparsed = parse(cursor.get_ref());
            assert_eq!(
                metadata(&reparsed, cursor.get_ref(), ItemKind::Jumbf),
                Some(vec![0x33; length])
            );
        }
    }
}

#[test]
fn legacy_first_ifd_manifest_in_multipage_images_is_migrated_and_removable() {
    for f in FORMATS {
        let bytes = fixture(
            f,
            2,
            &[
                (C2PA_TAG, 7, vec![0x31; 30]),
                (XMP_TAG, 1, b"<old/>".to_vec()),
            ],
        );
        assert_eq!(
            metadata(&parse(&bytes), &bytes, ItemKind::Jumbf),
            Some(vec![0x31; 30])
        );
        let (output, state) = rewrite(&bytes, &Updates::new().set_jumbf(vec![0x32; 45]));
        let map = layout(&state).unwrap();
        assert_eq!(map.pages().count(), 3);
        assert!(map.pages().next().unwrap().entry(C2PA_TAG).is_none());
        assert_eq!(map.pages().last().unwrap().entries().count(), 1);
        let (output, state) = rewrite(&output, &Updates::new().remove_jumbf());
        assert!(metadata(&state, &output, ItemKind::Jumbf).is_none());
        let (output, state) = rewrite(&bytes, &Updates::new().remove_jumbf());
        assert!(metadata(&state, &output, ItemKind::Jumbf).is_none());
    }
}

fn set_number(bytes: &mut [u8], f: Format, at: u64, value: u64, width: usize) {
    bytes[at as usize..at as usize + width].copy_from_slice(&f.number(value, width));
}

#[test]
fn dng_subifds_exif_gps_and_bigtable_offsets_are_preserved() {
    for f in FORMATS {
        let mut bytes = fixture(
            f,
            1,
            &[
                (50706, 1, vec![1, 6, 0, 0]),
                (330, if f.big { 18 } else { 13 }, f.number(0, f.width())),
                (34665, 4, f.number(0, 4)),
                (34853, 4, f.number(0, 4)),
                (52540, 7, vec![0x42; 16]),
                (52541, 4, f.number(0, 4)),
                (52542, 4, f.number(9, 4)),
            ],
        );
        let original = parse(&bytes);
        let bigtable = bytes.len();
        bytes.extend(b"BIGTABLE!");
        let child = bytes.len();
        bytes.extend(f.number(0, f.count_width()));
        bytes.extend(f.number(0, f.width()));
        for tag in [330, 34665, 34853, 52541] {
            let entry = layout(&original)
                .unwrap()
                .pages()
                .next()
                .unwrap()
                .entry(tag)
                .unwrap();
            let at = entry.value_range().unwrap().offset();
            set_number(
                &mut bytes,
                f,
                at,
                if tag == 52541 { bigtable } else { child } as u64,
                if tag == 330 { f.width() } else { 4 },
            );
        }
        let source = parse(&bytes);
        assert_eq!(source.media_type(), "image/x-adobe-dng");
        assert_eq!(layout(&source).unwrap().directories().count(), 2);
        let (output, state) = rewrite(&bytes, &Updates::new().set_jumbf(vec![0x5a; 100]));
        assert_eq!(&output[bigtable..bigtable + 9], b"BIGTABLE!");
        assert_eq!(&output[child..bytes.len()], &bytes[child..]);
        assert_eq!(layout(&state).unwrap().directories().count(), 2);
        let first = layout(&state).unwrap().pages().next().unwrap();
        for tag in [330, 34665, 34853, 52541] {
            let a = layout(&source)
                .unwrap()
                .pages()
                .next()
                .unwrap()
                .entry(tag)
                .unwrap()
                .value_range()
                .unwrap();
            let b = first.entry(tag).unwrap().value_range().unwrap();
            assert_eq!(
                &bytes[a.offset() as usize..a.end() as usize],
                &output[b.offset() as usize..b.end() as usize]
            );
        }
    }
}

#[test]
fn registry_formats_and_composed_metadata_match_upstream() {
    let registry = HandlerRegistry::builtin();
    for format in [
        "tif",
        "tiff",
        "image/tiff",
        "dng",
        "image/dng",
        "image/x-adobe-dng",
    ] {
        assert_eq!(registry.container_for(format), Some("tiff"));
        assert_eq!(
            registry.compose(format, ItemKind::Jumbf, b"STORE").unwrap(),
            b"STORE"
        );
        assert_eq!(
            registry.compose(format, ItemKind::Xmp, b"<xmp/>").unwrap(),
            b"<xmp/>"
        );
    }
    assert_eq!(registry.media_type_for("dng"), Some("image/x-adobe-dng"));
    assert_eq!(registry.extension_for("image/tiff"), Some("tif"));
    assert!(matches!(
        registry.compose("tif", ItemKind::Exif, b""),
        Err(Error::UnsupportedFormat)
    ));
}

#[test]
fn malformed_headers_cycles_counts_and_aliases_are_errors() {
    for f in FORMATS {
        let bytes = fixture(f, 1, &[]);
        for cut in 0..bytes.len() {
            assert!(tiff::parse(&mut Cursor::new(&bytes[..cut])).is_err());
        }
        let mut cyclic = bytes.clone();
        let dir = layout(&parse(&bytes))
            .unwrap()
            .pages()
            .next()
            .unwrap()
            .range();
        set_number(
            &mut cyclic,
            f,
            dir.end() - f.width() as u64,
            dir.offset(),
            f.width(),
        );
        assert!(tiff::parse(&mut Cursor::new(cyclic)).is_err());
        let mut subcycle = fixture(
            f,
            1,
            &[(330, if f.big { 18 } else { 13 }, f.number(0, f.width()))],
        );
        let structure = parse(&subcycle);
        let dir = layout(&structure).unwrap().pages().next().unwrap();
        set_number(
            &mut subcycle,
            f,
            dir.entry(330).unwrap().value_range().unwrap().offset(),
            dir.range().offset(),
            f.width(),
        );
        assert!(tiff::parse(&mut Cursor::new(subcycle)).is_err());
        let mut oversized = bytes.clone();
        set_number(
            &mut oversized,
            f,
            dir.range().offset(),
            65535,
            f.count_width(),
        );
        assert!(tiff::parse(&mut Cursor::new(oversized)).is_err());
        let mut alias = fixture(f, 1, &[(C2PA_TAG, 7, vec![0; 20])]);
        let state = parse(&alias);
        let dir = layout(&state).unwrap().pages().next().unwrap();
        let entry = dir.entry(C2PA_TAG).unwrap();
        set_number(
            &mut alias,
            f,
            entry.range().offset() + 4 + f.width() as u64,
            dir.range().offset(),
            f.width(),
        );
        assert!(tiff::parse(&mut Cursor::new(alias)).is_err());
        let wrong_type = fixture(f, 1, &[(C2PA_TAG, 1, vec![0; 20])]);
        assert!(tiff::parse(&mut Cursor::new(wrong_type)).is_err());
    }
}

#[test]
fn bigtiff_counts_are_bounded_before_allocating() {
    for little in [true, false] {
        let f = Format { little, big: true };
        let mut bytes = fixture(f, 1, &[]);
        set_number(&mut bytes, f, 16, u64::MAX, 8);
        assert!(matches!(
            tiff::parse(&mut Cursor::new(bytes)),
            Err(Error::LimitExceeded { .. })
        ));
    }
}

#[test]
fn c2pa_rs_multipage_fixture_preserves_media_and_manifest_lifecycle() {
    let bytes = include_bytes!("fixtures/tiff_multipage.tif").as_slice();
    let original = parse(bytes);
    let count = layout(&original).unwrap().pages().count();
    assert!(count > 1);
    let (signed, _) = rewrite(
        bytes,
        &Updates::new().set_jumbf(b"some manifest data".to_vec()),
    );
    let (with_xmp, state) = rewrite(
        &signed,
        &Updates::new().set_xmp(b"<xmp>new metadata</xmp>".to_vec()),
    );
    assert_eq!(
        metadata(&state, &with_xmp, ItemKind::Jumbf),
        Some(b"some manifest data".to_vec())
    );
    assert_eq!(layout(&state).unwrap().pages().count(), count + 1);
    let (replaced, state) = rewrite(&with_xmp, &Updates::new().set_jumbf(b"short".to_vec()));
    assert_eq!(
        metadata(&state, &replaced, ItemKind::Jumbf),
        Some(b"short".to_vec())
    );
    let (removed, state) = rewrite(&replaced, &Updates::new().remove_jumbf());
    assert_eq!(layout(&state).unwrap().pages().count(), count);
    assert!(metadata(&state, &removed, ItemKind::Jumbf).is_none());
    asset_io::testing::Conformance::new(tiff::handler())
        .jumbf(Some(vec![0; 128]))
        .mutations(false)
        .check("c2pa-rs MultiPage.tif", bytes);
}

struct Sparse {
    data: Vec<u8>,
    position: u64,
    length: u64,
}

#[test]
fn shared_subifds_can_reference_a_main_page_without_losing_geometry() {
    for f in FORMATS {
        let mut bytes = fixture(
            f,
            2,
            &[(330, if f.big { 18 } else { 13 }, f.number(0, f.width()))],
        );
        let parsed = parse(&bytes);
        let mut pages = layout(&parsed).unwrap().pages();
        let first = pages.next().unwrap();
        let second = pages.next().unwrap();
        set_number(
            &mut bytes,
            f,
            first.entry(330).unwrap().value_range().unwrap().offset(),
            second.range().offset(),
            f.width(),
        );
        let (output, state) = rewrite(&bytes, &Updates::new().set_xmp(b"<new metadata/>".to_vec()));
        assert_eq!(layout(&state).unwrap().directories().count(), 3);
        assert_eq!(
            metadata(&state, &output, ItemKind::Xmp),
            Some(b"<new metadata/>".to_vec())
        );
    }
}

impl Read for Sparse {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = (self.length.saturating_sub(self.position)).min(out.len() as u64) as usize;
        out[..n].fill(0);
        if self.position < self.data.len() as u64 {
            let take = n.min(self.data.len() - self.position as usize);
            out[..take]
                .copy_from_slice(&self.data[self.position as usize..self.position as usize + take]);
        }
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let at = match from {
            SeekFrom::Start(at) => at as i128,
            SeekFrom::Current(delta) => self.position as i128 + delta as i128,
            SeekFrom::End(delta) => self.length as i128 + delta as i128,
        };
        self.position = u64::try_from(at).map_err(|_| io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

#[test]
fn classic_tiff_refuses_64bit_ifd_offsets_while_bigtiff_plans_them() {
    for f in FORMATS {
        let mut source = Sparse {
            data: fixture(f, 1, &[]),
            position: 0,
            length: (1u64 << 32) + 32,
        };
        let structure = tiff::parse(&mut source).unwrap();
        let updates = Updates::new().set_jumbf(vec![0; 100]);
        let planned = structure.write_plan(&mut source, &updates);
        if f.big {
            let plan = planned.unwrap();
            asset_io::plan::validate(&plan, source.length).unwrap();
            assert!(
                layout(plan.structure())
                    .unwrap()
                    .pages()
                    .next()
                    .unwrap()
                    .range()
                    .offset()
                    > u32::MAX as u64
            );
        } else {
            assert!(planned.is_err());
        }
    }
}
