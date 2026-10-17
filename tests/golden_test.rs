//! Golden fingerprint tests.
//!
//! Captures parse results, metadata reads, read-side processing event streams, and
//! write outputs (bytes + processing event streams) for every fixture so that
//! refactors can be verified to be byte-for-byte and event-for-event identical.
//!
//! Regenerate with: `UPDATE_GOLDEN=1 cargo test --all-features --test golden_test`

#![cfg(all(
    feature = "jpeg",
    feature = "png",
    feature = "bmff",
    feature = "riff",
    feature = "pdf"
))]

use asset_io::{Asset, ExclusionMode, ProcessChunk, SegmentKind, Updates};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;

const FIXTURES: &[&str] = &[
    "Designer.jpeg",
    "FireflyTrain.jpg",
    "P1000708.jpg",
    "GreenCat.png",
    "sample1.png",
    "sample1.avif",
    "sample1.heic",
    "sample1.heif",
    "sample1.m4a",
    "sample1.webp",
    "sample1.wav",
    "pdf_metadata.pdf",
    "gen:plain.pdf",
];

/// Minimal classic-xref PDF without metadata, so writes are supported.
fn plain_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (id, body) in [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
    ] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    for o in offsets {
        bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn fixture(name: &str) -> Vec<u8> {
    if name == "gen:plain.pdf" {
        return plain_pdf();
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    std::fs::read(p).unwrap()
}

fn golden_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/golden/fingerprints.txt");
    p
}

/// Fingerprint of a parsed structure that is independent of enabled cargo features
/// (`memory-mapped` adds an always-`None` field to the `Debug` output).
fn structure_fp(s: &asset_io::Structure) -> String {
    hex(format!("{:?}", s).replace(", mmap: None", "").as_bytes())
}

fn hex(data: &[u8]) -> String {
    let h = Sha256::digest(data);
    h.iter().take(12).map(|b| format!("{:02x}", b)).collect()
}

fn result_str<T>(r: asset_io::Result<T>, f: impl FnOnce(T) -> String) -> String {
    match r {
        Ok(v) => f(v),
        Err(e) => format!("ERR({})", e),
    }
}

fn opt_hex(v: Option<Vec<u8>>) -> String {
    match v {
        Some(d) => format!("{}:{}", d.len(), hex(&d)),
        None => "none".into(),
    }
}

/// Fingerprint of a write-side processing event stream.
#[derive(Default)]
struct EventLog {
    hasher: Sha256,
    events: usize,
    bytes: u64,
}

impl EventLog {
    fn record(&mut self, chunk: &dyn ProcessChunk) {
        self.events += 1;
        if let Some(seg) = chunk.segment() {
            self.hasher
                .update(format!("B:{:?}:{:?}:{:?}|", seg.kind, seg.path, seg.ranges).as_bytes());
            return;
        }
        if let Some(id) = chunk.id() {
            self.hasher
                .update(format!("M:{}:{:?}|", id, chunk.large_size()).as_bytes());
        }
        self.hasher
            .update(format!("D:{}|", chunk.data().len()).as_bytes());
        self.hasher.update(chunk.data());
        self.bytes += chunk.data().len() as u64;
    }

    fn finish(self) -> String {
        let h = self.hasher.finalize();
        let h: String = h.iter().take(12).map(|b| format!("{:02x}", b)).collect();
        format!("ev={} bytes={} {}", self.events, self.bytes, h)
    }
}

fn scenarios() -> Vec<(&'static str, Updates)> {
    let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/></x:xmpmeta>"#.to_vec();
    let jumbf: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    vec![
        ("keep", Updates::new()),
        ("set_xmp", Updates::new().set_xmp(xmp.clone())),
        ("set_jumbf", Updates::new().set_jumbf(jumbf.clone())),
        ("remove_all", Updates::remove_all()),
        (
            "set_both_excl_data",
            Updates::new()
                .set_xmp(xmp.clone())
                .set_jumbf(jumbf.clone())
                .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly),
        ),
        (
            "set_jumbf_excl_entire",
            Updates::new()
                .set_jumbf(jumbf)
                .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::EntireSegment)
                .with_chunk_size(4096),
        ),
    ]
}

fn fingerprint_fixture(name: &str, out: &mut BTreeMap<String, String>) {
    let data = fixture(name);
    let key = |s: &str| format!("{}::{}", name, s);

    let mut asset = match Asset::from_source(Cursor::new(data.clone())) {
        Ok(a) => a,
        Err(e) => {
            out.insert(key("parse"), format!("ERR({})", e));
            return;
        }
    };
    out.insert(key("parse"), structure_fp(asset.structure()));
    out.insert(key("xmp"), result_str(asset.xmp(), opt_hex));
    out.insert(key("jumbf"), result_str(asset.jumbf(), opt_hex));

    for (sname, updates) in scenarios() {
        let mut h = Sha256::new();
        let mut n = 0usize;
        let mut total = 0u64;
        let r = asset.read_with_processing(&updates, &mut |c: &[u8]| {
            n += 1;
            total += c.len() as u64;
            h.update((c.len() as u64).to_be_bytes());
            h.update(c);
            Ok(())
        });
        let hs: String = h
            .finalize()
            .iter()
            .take(12)
            .map(|b| format!("{:02x}", b))
            .collect();
        out.insert(
            key(&format!("read_process/{}", sname)),
            result_str(r, |_| format!("ev={} bytes={} {}", n, total, hs)),
        );

        let mut w = Cursor::new(Vec::new());
        let r = asset.write(&mut w, &updates);
        out.insert(
            key(&format!("write/{}", sname)),
            result_str(r, |s| {
                format!(
                    "{}:{} dest={}",
                    w.get_ref().len(),
                    hex(w.get_ref()),
                    structure_fp(&s)
                )
            }),
        );

        let mut w = Cursor::new(Vec::new());
        let mut log = EventLog::default();
        let r = asset.write_with_processing(&mut w, &updates, &mut |c: &dyn ProcessChunk| {
            log.record(c);
            Ok(())
        });
        out.insert(
            key(&format!("write_process/{}", sname)),
            result_str(r, |_| {
                format!(
                    "{}:{} {}",
                    w.get_ref().len(),
                    hex(w.get_ref()),
                    log.finish()
                )
            }),
        );
    }
}

fn compute_all() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in FIXTURES {
        fingerprint_fixture(name, &mut out);
    }
    out
}

fn render(map: &BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(k, v)| format!("{} = {}\n", k, v))
        .collect()
}

#[test]
fn golden_fingerprints() {
    let actual = render(&compute_all());
    let path = golden_path();
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .expect("golden file missing; run with UPDATE_GOLDEN=1 to create");
    if expected != actual {
        let exp: BTreeMap<_, _> = expected
            .lines()
            .filter_map(|l| l.split_once(" = "))
            .collect();
        let act: BTreeMap<_, _> = actual.lines().filter_map(|l| l.split_once(" = ")).collect();
        let mut diffs = Vec::new();
        for (k, v) in &act {
            if exp.get(k) != Some(v) {
                diffs.push(format!(
                    "{}\n  expected: {:?}\n  actual:   {}",
                    k,
                    exp.get(k),
                    v
                ));
            }
        }
        for k in exp.keys() {
            if !act.contains_key(k) {
                diffs.push(format!("{} missing", k));
            }
        }
        panic!("{} golden mismatches:\n{}", diffs.len(), diffs.join("\n"));
    }
}
