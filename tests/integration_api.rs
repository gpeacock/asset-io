//! The pieces an embedding SDK needs beyond read and write: planning without
//! writing, the store as a reader sees it, and media types by format string.
#![cfg(all(feature = "jpeg", feature = "png", feature = "bmff", feature = "riff"))]

use asset_io::{HandlerRegistry, NullSink, SegmentKind, Structure, Updates};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            [
                "jpg", "jpeg", "png", "webp", "wav", "heic", "heif", "avif", "m4a", "mp4", "mov",
            ]
            .contains(&ext.as_str())
            .then(|| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap(),
                )
            })
        })
        .collect();
    v.sort();
    v
}

fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

/// A JUMBF superbox of type `uuid` holding one child per entry of `children`.
fn store(uuid: u8, children: &[&[u8]]) -> Vec<u8> {
    let mut d = [uuid; 16].to_vec();
    d.push(3);
    d.extend_from_slice(b"c2pa\0");
    let mut body = boxed(b"jumd", &d);
    for c in children {
        body.extend_from_slice(&boxed(b"json", c));
    }
    boxed(b"jumb", &body)
}

/// Everything about a layout a caller can observe.
fn shape(
    s: &Structure,
) -> (
    u64,
    Vec<asset_io::Segment>,
    Vec<(SegmentKind, Option<String>, Vec<(usize, u64, u64)>)>,
) {
    let mut entities = Vec::new();
    for kind in [
        SegmentKind::Jumbf,
        SegmentKind::Xmp,
        SegmentKind::Exif,
        SegmentKind::Thumbnail,
        SegmentKind::Other,
    ] {
        for e in s.entities(kind) {
            let parts = e
                .parts()
                .map(|p| (p.segment().index(), p.range().offset(), p.range().len()))
                .collect();
            entities.push((kind, e.label().map(str::to_owned), parts));
        }
    }
    (s.total_size(), s.segments().to_vec(), entities)
}

#[test]
fn plan_is_what_write_returns() {
    let reg = HandlerRegistry::builtin();
    let updates = [
        Updates::new(),
        Updates::new().set_jumbf(store(1, &[b"one"])),
        Updates::new()
            .set_jumbf(store(1, &[b"one"]))
            .set_xmp(b"<x:xmpmeta/>\n".to_vec()),
        Updates::remove_all(),
        Updates::new().remove_jumbf().remove_xmp(),
    ];
    let mut ran = 0;
    for (name, bytes) in fixtures() {
        let Ok(s) = reg.parse_detect(&mut Cursor::new(&bytes)) else {
            continue;
        };
        for (i, u) in updates.iter().enumerate() {
            let planned = s.plan(&mut Cursor::new(&bytes), u);
            let mut out = Cursor::new(Vec::new());
            let written = s.write(&mut Cursor::new(&bytes), &mut out, u, &mut NullSink);
            match (planned, written) {
                (Ok(p), Ok(w)) => {
                    assert_eq!(shape(&p), shape(&w), "{name} update {i}");
                    assert_eq!(
                        p.total_size(),
                        out.get_ref().len() as u64,
                        "{name} update {i}"
                    );
                }
                (Err(_), Err(_)) => {}
                (p, w) => panic!(
                    "{name} update {i}: plan {:?}, write {:?}",
                    p.is_ok(),
                    w.is_ok()
                ),
            }
        }
        ran += 1;
    }
    assert!(ran > 0, "no fixtures");
}

#[test]
fn plan_places_a_manifest_that_is_not_there_yet() {
    let reg = HandlerRegistry::builtin();
    let u = Updates::new().set_jumbf(store(1, &[b"one"]));
    let mut ran = 0;
    for (name, bytes) in fixtures() {
        let Ok(s) = reg.parse_detect(&mut Cursor::new(&bytes)) else {
            continue;
        };
        if s.entity(SegmentKind::Jumbf).ok().flatten().is_some() {
            continue;
        }
        let Ok(dest) = s.plan(&mut Cursor::new(&bytes), &u) else {
            continue;
        };
        let e = dest.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.len(), store(1, &[b"one"]).len() as u64, "{name}");
        assert!(dest.total_size() > s.total_size(), "{name}");
        ran += 1;
    }
    assert!(ran > 0, "no fixture without a manifest");
}

#[test]
fn a_reader_sees_original_and_update_as_one_store() {
    let reg = HandlerRegistry::builtin();
    let bytes =
        fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic"));
    let Ok(bytes) = bytes else { return };

    let first = store(1, &[b"m1"]);
    let second = store(1, &[b"m1", b"m2"]);

    // Start from a file with no store, whatever the fixture holds.
    let bytes = {
        let s = reg.parse_detect(&mut Cursor::new(&bytes)).unwrap();
        let mut out = Cursor::new(Vec::new());
        s.write(
            &mut Cursor::new(&bytes),
            &mut out,
            &Updates::remove_all(),
            &mut NullSink,
        )
        .unwrap();
        out.into_inner()
    };
    let s0 = reg.parse_detect(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(s0.store(&mut Cursor::new(&bytes)).unwrap(), None);

    let mut a = Cursor::new(Vec::new());
    s0.write(
        &mut Cursor::new(&bytes),
        &mut a,
        &Updates::new().set_jumbf(first.clone()),
        &mut NullSink,
    )
    .unwrap();
    let a = a.into_inner();
    let s1 = reg.parse_detect(&mut Cursor::new(&a)).unwrap();
    assert_eq!(s1.store(&mut Cursor::new(&a)).unwrap(), Some(first));

    let mut b = Cursor::new(Vec::new());
    s1.write(
        &mut Cursor::new(&a),
        &mut b,
        &Updates::new().set_jumbf(second.clone()).as_update(),
        &mut NullSink,
    )
    .unwrap();
    let b = b.into_inner();
    let s2 = reg.parse_detect(&mut Cursor::new(&b)).unwrap();
    // Two stores are in the file, and a reader sees them as the one that was set.
    assert_eq!(s2.entities(SegmentKind::Jumbf).count(), 2);
    assert_eq!(s2.store(&mut Cursor::new(&b)).unwrap(), Some(second));
}

/// The bytes of the segments an entity lives in, in order.
fn entity_segments(s: &Structure, out: &[u8], kind: SegmentKind) -> Vec<u8> {
    // A JPEG that already holds a store of another type keeps it, so choose the
    // one just written, which these tests give the type `[1; 16]`.
    let e = s
        .entities(kind)
        .find(|e| e.jumbf_type() == Some([1; 16]))
        .or_else(|| s.entities(kind).next())
        .unwrap();
    let mut ids: Vec<_> = e.parts().map(|p| p.segment()).collect();
    ids.dedup();
    let mut v = Vec::new();
    for id in ids {
        let x = s.segment(id).unwrap().extent();
        v.extend_from_slice(&out[x.offset() as usize..x.end() as usize]);
    }
    v
}

#[test]
fn compose_is_what_write_inserts() {
    let reg = HandlerRegistry::builtin();
    let jumbf = store(1, &[b"one"]);
    let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_vec();
    let mut checked = std::collections::HashSet::new();
    for (name, bytes) in fixtures() {
        let Ok(s) = reg.parse_detect(&mut Cursor::new(&bytes)) else {
            continue;
        };
        for (kind, data) in [(SegmentKind::Jumbf, &jumbf), (SegmentKind::Xmp, &xmp)] {
            let composed = match reg.compose(s.container_id(), kind, data) {
                Ok(c) => c,
                Err(asset_io::Error::UnsupportedFormat) => continue,
                Err(e) => panic!("{name}: {e}"),
            };
            let updates = match kind {
                SegmentKind::Jumbf => Updates::new().set_jumbf(data.clone()),
                _ => Updates::new().set_xmp(data.clone()),
            };
            let mut out = Cursor::new(Vec::new());
            let dest = s
                .write(&mut Cursor::new(&bytes), &mut out, &updates, &mut NullSink)
                .unwrap();
            let out = out.into_inner();
            assert_eq!(
                composed,
                entity_segments(&dest, &out, kind),
                "{name} {kind:?}"
            );
            checked.insert((s.container_id().to_owned(), kind));
        }
    }
    // JPEG, PNG and BMFF, each for both kinds.
    assert_eq!(checked.len(), 6, "{checked:?}");
}

#[test]
fn compose_splits_a_large_jpeg_store_like_write() {
    let reg = HandlerRegistry::builtin();
    let children = vec![0x5Au8; 200_000];
    let jumbf = store(1, &[&children]);
    let composed = reg.compose("jpg", SegmentKind::Jumbf, &jumbf).unwrap();
    // Four APP11 segments, each with its own marker.
    let markers = composed
        .windows(2)
        .filter(|w| w[0] == 0xFF && w[1] == 0xEB)
        .count();
    assert!(markers >= 4, "{markers} APP11 markers");

    let Some((_, bytes)) = fixtures().into_iter().find(|(n, _)| n.ends_with(".jpg")) else {
        return;
    };
    let s = reg.parse("jpg", &mut Cursor::new(&bytes)).unwrap();
    let mut out = Cursor::new(Vec::new());
    let dest = s
        .write(
            &mut Cursor::new(&bytes),
            &mut out,
            &Updates::new().set_jumbf(jumbf),
            &mut NullSink,
        )
        .unwrap();
    assert_eq!(
        composed,
        entity_segments(&dest, &out.into_inner(), SegmentKind::Jumbf)
    );
}

#[test]
fn compose_rejects_what_it_cannot_encode() {
    let reg = HandlerRegistry::builtin();
    assert!(reg
        .compose("jpg", SegmentKind::Jumbf, b"not a superbox")
        .is_err());
    assert!(reg.compose("jpg", SegmentKind::Exif, b"x").is_err());
    assert!(reg.compose("png", SegmentKind::Thumbnail, b"x").is_err());
    assert!(reg.compose("xyz", SegmentKind::Jumbf, b"x").is_err());
}

#[test]
fn media_types_by_format_string() {
    let reg = HandlerRegistry::builtin();
    for (format, mime) in [
        ("jpg", "image/jpeg"),
        ("JPEG", "image/jpeg"),
        ("mpo", "image/jpeg"),
        ("image/jpeg", "image/jpeg"),
        (" .png ", "image/png"),
        ("heic", "image/heic"),
        ("avif", "image/avif"),
        ("m4a", "audio/mp4"),
        ("mov", "video/quicktime"),
        ("video/mp4", "video/mp4"),
        ("webp", "image/webp"),
        ("wav", "audio/wav"),
        ("avi", "video/avi"),
    ] {
        assert_eq!(reg.media_type_for(format), Some(mime), "{format}");
    }
    assert_eq!(reg.media_type_for("xyz"), None);
    // Same container family, different media types.
    assert_eq!(reg.container_for("heic"), reg.container_for("mp4"));
    assert_eq!(reg.container_for("wav"), reg.container_for("webp"));

    // A caller's registry extends the built-ins and still answers for them.
    let ext = HandlerRegistry::extending_builtin();
    assert_eq!(ext.media_type_for("heic"), Some("image/heic"));
}
