// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

// Unless required by applicable law or agreed to in writing,
// this software is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR REPRESENTATIONS OF ANY KIND, either express or
// implied. See the LICENSE-MIT and LICENSE-APACHE files for the
// specific language governing permissions and limitations under
// each license.

//! Runs the layout model on the real files in tests/fixtures.

#![cfg(all(feature = "jpeg", feature = "png"))]

use asset_io::{containers, NullSink, Role};
use asset_io::{SegmentKind, Updates};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn fixtures() -> Vec<(PathBuf, bool)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => v.push((p, true)),
            "png" => v.push((p, false)),
            _ => {}
        }
    }
    v.sort();
    v
}

fn parse(bytes: &[u8], jpeg: bool) -> asset_io::Result<asset_io::Structure> {
    let mut c = Cursor::new(bytes.to_vec());
    if jpeg {
        containers::jpeg::parse(&mut c)
    } else {
        containers::png::parse(&mut c)
    }
}

fn write(
    s: &asset_io::Structure,
    bytes: &[u8],
    jpeg: bool,
    u: &Updates,
) -> asset_io::Result<(Vec<u8>, asset_io::Structure)> {
    let mut out = Cursor::new(Vec::new());
    let mut src = Cursor::new(bytes.to_vec());
    let dest = if jpeg {
        containers::jpeg::write(s, &mut src, &mut out, u, &mut NullSink)?
    } else {
        containers::png::write(s, &mut src, &mut out, u, &mut NullSink)?
    };
    Ok((out.into_inner(), dest))
}

fn read_jumbf(s: &asset_io::Structure, bytes: &[u8], jpeg: bool) -> Option<Vec<u8>> {
    let mut c = Cursor::new(bytes.to_vec());
    match s.entity(SegmentKind::Jumbf).unwrap() {
        Some(e) => Some(s.read_entity(&mut c, e).unwrap()),
        None => {
            let _ = jpeg;
            None
        }
    }
}

fn manifest(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
    v[4..8].copy_from_slice(b"jumb");
    for (i, b) in v[8..].iter_mut().enumerate() {
        *b = ((i * 13) % 255) as u8 + 1;
    }
    v
}

#[test]
fn real_files_parse_and_tile() {
    let files = fixtures();
    assert!(!files.is_empty(), "no fixtures found");
    for (path, jpeg) in files {
        let bytes = fs::read(&path).unwrap();
        let s = parse(&bytes, jpeg).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(s.total_size(), bytes.len() as u64, "{}", path.display());
        let mut next = 0;
        for r in s.regions() {
            assert_eq!(r.range.offset(), next, "{}", path.display());
            next = r.range.end();
        }
        assert_eq!(next, bytes.len() as u64, "{}", path.display());

        // The element traversal (restart intervals expanded) tiles the file too.
        let mut next = 0;
        let mut count = 0usize;
        s.elements(&mut Cursor::new(bytes.clone()), |_, e| {
            assert_eq!(e.extent().offset(), next, "{}: element gap", path.display());
            next = e.extent().end();
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            next,
            bytes.len() as u64,
            "{}: element coverage",
            path.display()
        );
        assert!(count >= s.segments().len(), "{}", path.display());
        println!(
            "{}: {} segments, jumbf={}, xmp={}",
            path.file_name().unwrap().to_string_lossy(),
            s.segments().len(),
            s.entities(SegmentKind::Jumbf).count(),
            s.entities(SegmentKind::Xmp).count()
        );
    }
}

#[test]
fn real_files_keep_is_byte_identical() {
    for (path, jpeg) in fixtures() {
        let bytes = fs::read(&path).unwrap();
        let s = parse(&bytes, jpeg).unwrap();
        let (out, dest) = write(&s, &bytes, jpeg, &Updates::new()).unwrap();
        assert!(out == bytes, "{}: Keep changed the file", path.display());
        assert_eq!(dest.total_size(), bytes.len() as u64);
    }
}

#[test]
fn real_files_set_patch_and_reopen() {
    for (path, jpeg) in fixtures() {
        let bytes = fs::read(&path).unwrap();
        let s = parse(&bytes, jpeg).unwrap();

        // Sizes chosen to cover one segment and several (JPEG splits at ~64 KB).
        for size in [4_000usize, 150_000] {
            let placeholder = {
                let mut p = vec![0u8; size];
                p[0..4].copy_from_slice(&(size as u32).to_be_bytes());
                p[4..8].copy_from_slice(b"jumb");
                p
            };
            let (mut out, dest) = write(&s, &bytes, jpeg, &Updates::new().set_jumbf(placeholder))
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));

            // New layout must describe the output exactly.
            let reparsed = parse(&out, jpeg).unwrap();
            assert_eq!(reparsed.total_size(), dest.total_size());
            assert_eq!(reparsed.segments(), dest.segments(), "{}", path.display());

            // Patch a "signed" manifest in place.
            let e = dest.require_entity(SegmentKind::Jumbf).unwrap().clone();
            let signed = manifest(size - 100);
            let mut cur = Cursor::new(std::mem::take(&mut out));
            dest.patch(&mut cur, &e, &signed).unwrap();
            let out = cur.into_inner();

            // New parser reads it back.
            let re = parse(&out, jpeg).unwrap();
            let got = read_jumbf(&re, &out, jpeg).unwrap();
            assert_eq!(&got[..signed.len()], &signed[..], "{}", path.display());

            // Image data is untouched.
            let img = |st: &asset_io::Structure, b: &[u8]| -> Vec<u8> {
                let mut v = Vec::new();
                for g in st.segments() {
                    if g.kind() == SegmentKind::ImageData {
                        let r = g.extent();
                        v.extend_from_slice(&b[r.offset() as usize..r.end() as usize]);
                    }
                }
                v
            };
            assert!(
                img(&s, &bytes) == img(&re, &out),
                "{}: image data changed",
                path.display()
            );
        }
    }
}

fn walk(dir: &std::path::Path, out: &mut Vec<(PathBuf, bool)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
            continue;
        }
        match p
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("jpg" | "jpeg") => out.push((p, true)),
            Some("png") => out.push((p, false)),
            _ => {}
        }
    }
}

/// Sweep over arbitrary directories (recursive). Set `ASSET_IO_SWEEP` to a
/// path-separated list, e.g.
///   ASSET_IO_SWEEP=/Users/me/dev/c2pa-rs cargo test --test container_fixtures \
///     --features png,jpeg sweep -- --nocapture
/// Reports every disagreement instead of stopping at the first.
#[test]
fn sweep_external_directories() {
    let Some(dirs) = std::env::var_os("ASSET_IO_SWEEP") else {
        eprintln!("ASSET_IO_SWEEP not set; skipping");
        return;
    };
    let mut files = Vec::new();
    for d in std::env::split_paths(&dirs) {
        walk(&d, &mut files);
    }
    files.sort();

    let (mut ok, mut rejected, mut problems) = (0usize, 0usize, Vec::<String>::new());
    let mut census = [0usize; 6];
    for (path, jpeg) in &files {
        let name = path.display().to_string();
        let Ok(bytes) = fs::read(path) else { continue };
        let s = match parse(&bytes, *jpeg) {
            Ok(s) => s,
            Err(_) => {
                rejected += 1;
                continue;
            }
        };

        // Tiling.
        let mut next = 0;
        let mut tiled = true;
        for r in s.regions() {
            tiled &= r.range.offset() == next;
            next = r.range.end();
        }
        if !tiled || next != bytes.len() as u64 {
            problems.push(format!("{name}: does not tile"));
            continue;
        }

        // Keep is byte-identical.
        match write(&s, &bytes, *jpeg, &Updates::new()) {
            Ok((out, _)) if out == bytes => {}
            Ok(_) => {
                problems.push(format!("{name}: Keep changed bytes"));
                continue;
            }
            Err(e) => {
                problems.push(format!("{name}: Keep failed: {e}"));
                continue;
            }
        }

        // Metadata is readable. Ambiguity is reported, not treated as a mismatch.
        let mut c = Cursor::new(bytes.clone());
        match s.entity(SegmentKind::Jumbf) {
            Ok(Some(e)) => {
                if let Err(e) = s.read_entity(&mut c, e) {
                    problems.push(format!("{name}: JUMBF unreadable: {e}"));
                    continue;
                }
            }
            Ok(None) => {}
            Err(e) => {
                problems.push(format!("{name}: jumbf: {e}"));
                continue;
            }
        }
        if *jpeg {
            if let Err(e) = containers::jpeg::read_xmp(&s, &mut c) {
                problems.push(format!("{name}: XMP unreadable: {e}"));
                continue;
            }
        }

        // Replace the manifest (or add one) and check the output reparses to
        // the planned layout.
        match write(
            &s,
            &bytes,
            *jpeg,
            &Updates::new().set_jumbf(manifest(5_000)),
        ) {
            Ok((out, dest)) => match parse(&out, *jpeg) {
                Ok(re) if re.segments() == dest.segments() => {}
                Ok(_) => problems.push(format!("{name}: Set layout != reparsed layout")),
                Err(e) => problems.push(format!("{name}: Set output unparsable: {e}")),
            },
            Err(e) => problems.push(format!("{name}: Set failed: {e}")),
        }
        ok += 1;

        census[0] += 1;
        if s.entities(SegmentKind::Jumbf).next().is_some() {
            census[1] += 1;
        }
        if s.entities(SegmentKind::Jumbf).any(|e| e.part_count() > 1) {
            census[2] += 1;
        }
        if s.entities(SegmentKind::Xmp).next().is_some() {
            census[3] += 1;
        }
        if s.entities(SegmentKind::Xmp).any(|e| e.part_count() > 1) {
            census[4] += 1;
        }
        if s.entities(SegmentKind::Exif).next().is_some() {
            census[5] += 1;
        }
    }

    eprintln!(
        "swept {} files: {} ok, {} rejected, {} problems",
        files.len(),
        ok,
        rejected,
        problems.len()
    );
    eprintln!(
        "census: files={} jumbf={} multi-part jumbf={} xmp={} multi-part xmp={} exif={}",
        census[0], census[1], census[2], census[3], census[4], census[5]
    );
    for p in &problems {
        eprintln!("  {p}");
    }
    assert!(problems.is_empty(), "{} problem(s)", problems.len());
}

/// Tiny deterministic PRNG so failures are reproducible.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Hostile input: whatever the parser accepts must tile and survive a Keep and a
/// Set; whatever it rejects must be an `Err`, not a panic. Panics fail the test.
fn check_hostile(bytes: &[u8], jpeg: bool, what: &str) {
    let Ok(s) = parse(bytes, jpeg) else { return };
    assert_eq!(s.total_size(), bytes.len() as u64, "{what}: total");
    let mut next = 0;
    for r in s.regions() {
        assert_eq!(r.range.offset(), next, "{what}: gap");
        next = r.range.end();
    }
    assert_eq!(next, bytes.len() as u64, "{what}: coverage");
    // Reads of whatever it found must not panic (errors are fine).
    for kind in [SegmentKind::Jumbf, SegmentKind::Xmp, SegmentKind::Exif] {
        for e in s.entities(kind) {
            let _ = s.read_entity(&mut Cursor::new(bytes.to_vec()), e);
        }
    }
    if let Ok((out, dest)) = write(&s, bytes, jpeg, &Updates::new()) {
        assert!(out == bytes, "{what}: Keep changed bytes");
        assert_eq!(dest.total_size(), bytes.len() as u64);
    }
    // Set may legitimately fail (e.g. Ambiguous); if it succeeds it must be coherent.
    if let Ok((out, dest)) = write(&s, bytes, jpeg, &Updates::new().set_jumbf(manifest(3_000))) {
        assert_eq!(dest.total_size(), out.len() as u64, "{what}: Set size");
    }
}

#[test]
fn hostile_truncation_and_mutation() {
    let mut files = fixtures();
    if let Some(dirs) = std::env::var_os("ASSET_IO_SWEEP") {
        for d in std::env::split_paths(&dirs) {
            walk(&d, &mut files);
        }
    }
    files.sort();
    files.dedup();

    let mut rng = Lcg(0x5eed);
    for (path, jpeg) in files {
        let bytes = fs::read(&path).unwrap();
        let name = path.display().to_string();
        // Large files: only the first 256 KiB are mutated/truncated; headers and
        // length fields live there, and the cost stays bounded.
        let head = bytes.len().min(256 * 1024);

        // Truncation: every offset in the first 600 bytes, then random ones.
        for cut in (0..head.min(600)).chain((0..200).map(|_| (rng.next() as usize) % head)) {
            check_hostile(&bytes[..cut], jpeg, &format!("{name} cut {cut}"));
        }
        // Single and multi-byte corruption, biased toward the start where
        // lengths and signatures sit.
        for i in 0..400 {
            let mut m = bytes[..head].to_vec();
            for _ in 0..1 + (i % 4) {
                let pos = if rng.next().is_multiple_of(2) {
                    (rng.next() as usize) % m.len().min(512)
                } else {
                    (rng.next() as usize) % m.len()
                };
                m[pos] = match rng.next() % 3 {
                    0 => 0xFF,
                    1 => 0x00,
                    _ => rng.next() as u8,
                };
            }
            check_hostile(&m, jpeg, &format!("{name} mutation {i}"));
        }
    }
}

#[test]
fn real_files_sink_sees_exactly_the_output() {
    for (path, jpeg) in fixtures() {
        let bytes = fs::read(&path).unwrap();
        let s = parse(&bytes, jpeg).unwrap();
        let mut seen = Vec::new();
        let mut roles = [0u64; 3];
        let mut sink = |c: asset_io::Chunk<'_>| -> asset_io::Result<()> {
            assert_eq!(c.offset, seen.len() as u64);
            roles[match c.role {
                Role::Header => 0,
                Role::Payload => 1,
                Role::Trailer => 2,
            }] += c.data.len() as u64;
            seen.extend_from_slice(c.data);
            Ok(())
        };
        let mut out = Cursor::new(Vec::new());
        let mut src = Cursor::new(bytes.clone());
        let u = Updates::new().set_jumbf(manifest(20_000));
        if jpeg {
            containers::jpeg::write(&s, &mut src, &mut out, &u, &mut sink).unwrap();
        } else {
            containers::png::write(&s, &mut src, &mut out, &u, &mut sink).unwrap();
        }
        assert!(seen == out.into_inner(), "{}", path.display());
    }
}
