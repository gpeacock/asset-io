//! Protocol-level tests for the sans-IO machines.
//!
//! Every machine is driven by a hand-written loop that serves source bytes from memory using
//! deliberately awkward read sizes (1 byte, pseudo-random short reads). Results must be
//! identical to the blocking [`Asset`] API, which is itself built on the same machines driven
//! with full reads.

#![cfg(all(
    feature = "jpeg",
    feature = "png",
    feature = "bmff",
    feature = "riff",
    feature = "pdf"
))]

use asset_io::sans_io::{self, Machine, Step};
use asset_io::{Asset, Error, ExclusionMode, ProcessChunk, SegmentKind, Updates};
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
];

fn fixture(name: &str) -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    std::fs::read(p).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Reads {
    /// Fill every read request completely.
    Full,
    /// Deliver at most one byte per read.
    OneByte,
    /// Deliver a pseudo-random number of bytes (1..=max) per read.
    Random { seed: u64, max: usize },
}

#[derive(Default)]
struct Trace {
    out: Vec<u8>,
    /// Concatenated processed bytes and a compact event log.
    processed: Vec<u8>,
    events: Vec<String>,
    /// (offset, len) of each satisfied read.
    reads: Vec<(u64, usize)>,
}

/// Drive `machine` against an in-memory source/destination.
fn run<T>(mut machine: Machine<'_, T>, src: &[u8], mut dest: Vec<u8>, reads: Reads) -> (T, Trace) {
    let mut rng = match reads {
        Reads::Random { seed, .. } => seed | 1,
        _ => 1,
    };
    let mut trace = Trace::default();
    loop {
        match machine.step().unwrap() {
            Step::Read { offset, buf } => {
                let start = (offset as usize).min(src.len());
                let avail = src.len() - start;
                let want = match reads {
                    Reads::Full => buf.len(),
                    Reads::OneByte => 1,
                    Reads::Random { max, .. } => {
                        rng ^= rng << 13;
                        rng ^= rng >> 7;
                        rng ^= rng << 17;
                        1 + (rng as usize % max)
                    }
                };
                let n = want.min(buf.len()).min(avail);
                buf[..n].copy_from_slice(&src[start..start + n]);
                trace.reads.push((offset, n));
                machine.filled(n).unwrap();
            }
            Step::Write { offset, data } => {
                let end = offset as usize + data.len();
                if dest.len() < end {
                    dest.resize(end, 0);
                }
                dest[offset as usize..end].copy_from_slice(data);
            }
            Step::Process(chunk) => {
                trace.processed.extend_from_slice(chunk.data());
                trace.events.push(match (chunk.id(), chunk.segment()) {
                    (Some(id), _) => format!("mdat{id}:{}", chunk.data().len()),
                    (_, Some(s)) => format!("seg:{:?}@{}", s.kind, s.location().offset),
                    _ => format!("d:{}", chunk.data().len()),
                });
            }
            Step::Done => break,
        }
    }
    trace.out = dest;
    (machine.finish().unwrap(), trace)
}

/// Same event log, but merging adjacent data events so that only content and boundaries
/// are compared (event granularity is checked separately with full reads).
fn merged(events: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        if let (Some(n), Some(last)) = (e.strip_prefix("d:"), out.last_mut()) {
            if let Some(m) = last.strip_prefix("d:") {
                *last = format!(
                    "d:{}",
                    m.parse::<usize>().unwrap() + n.parse::<usize>().unwrap()
                );
                continue;
            }
        }
        out.push(e.clone());
    }
    out
}

fn scenarios() -> Vec<(&'static str, Updates)> {
    vec![
        ("keep", Updates::new()),
        (
            "jumbf_excl",
            Updates::new()
                .set_jumbf(vec![0x5a; 3000])
                .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly),
        ),
        (
            "xmp",
            Updates::new()
                .set_xmp(b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF/></x:xmpmeta>".to_vec()),
        ),
    ]
}

/// Reference results via the blocking `Asset` API.
struct Expected {
    structure: String,
    xmp: Option<Vec<u8>>,
    jumbf: Option<Vec<u8>>,
    hash_input: Vec<u8>,
}

fn expected(data: &[u8]) -> Expected {
    let mut asset = Asset::from_source(Cursor::new(data.to_vec())).unwrap();
    let mut hash_input = Vec::new();
    asset
        .read_with_processing(&Updates::new(), &mut |d: &[u8]| {
            hash_input.extend_from_slice(d);
            Ok(())
        })
        .unwrap();
    Expected {
        structure: format!("{:?}", asset.structure()),
        xmp: asset.xmp().unwrap(),
        jumbf: asset.jumbf().unwrap(),
        hash_input,
    }
}

fn check_reads(name: &str, data: &[u8], reads: Reads) {
    let exp = expected(data);
    let len = data.len() as u64;

    let (structure, _) = run(sans_io::parse(len), data, Vec::new(), reads);
    assert_eq!(
        format!("{structure:?}"),
        exp.structure,
        "{name} parse {reads:?}"
    );

    let (xmp, _) = run(sans_io::read_xmp(&structure), data, Vec::new(), reads);
    assert_eq!(xmp, exp.xmp, "{name} xmp {reads:?}");

    let (jumbf, _) = run(sans_io::read_jumbf(&structure), data, Vec::new(), reads);
    assert_eq!(jumbf, exp.jumbf, "{name} jumbf {reads:?}");

    let updates = Updates::new();
    let (_, trace) = run(
        sans_io::read_process(&structure, &updates),
        data,
        Vec::new(),
        reads,
    );
    assert_eq!(
        trace.processed, exp.hash_input,
        "{name} read_process {reads:?}"
    );

    // Hashing an existing asset is strictly forward-only.
    let mut end = 0;
    for &(offset, n) in &trace.reads {
        assert!(offset >= end, "{name}: read_process went backwards");
        end = offset + n as u64;
    }
}

fn check_writes(name: &str, data: &[u8], reads: Reads) {
    let len = data.len() as u64;
    let (structure, _) = run(sans_io::parse(len), data, Vec::new(), Reads::Full);
    for (scenario, updates) in scenarios() {
        let mut asset = Asset::from_source(Cursor::new(data.to_vec())).unwrap();
        let mut events = Vec::new();
        let mut expected_out = Cursor::new(Vec::new());
        let expected_dest = asset.write_with_processing(
            &mut expected_out,
            &updates,
            &mut |c: &dyn ProcessChunk| {
                events.push(c.data().to_vec());
                Ok(())
            },
        );
        let machine = sans_io::write(&structure, &updates, true);
        let mut m = Some(machine);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(m.take().unwrap(), data, Vec::new(), reads)
        }));
        match (expected_dest, result) {
            (Ok(exp_dest), Ok((dest, trace))) => {
                assert_eq!(
                    trace.out,
                    expected_out.get_ref().as_slice(),
                    "{name}/{scenario} bytes {reads:?}"
                );
                assert_eq!(
                    trace.processed,
                    events.concat(),
                    "{name}/{scenario} processed {reads:?}"
                );
                assert_eq!(
                    format!("{dest:?}"),
                    format!("{exp_dest:?}"),
                    "{name}/{scenario} dest structure"
                );
                if matches!(reads, Reads::Full) {
                    let sizes: Vec<usize> = events.iter().map(Vec::len).collect();
                    let got: Vec<usize> = trace
                        .events
                        .iter()
                        .filter_map(|e| {
                            e.strip_prefix("d:")
                                .or_else(|| {
                                    e.split_once(':')
                                        .filter(|p| p.0.starts_with("mdat"))
                                        .map(|p| p.1)
                                })
                                .map(|n| n.parse().unwrap())
                        })
                        .chain(
                            trace
                                .events
                                .iter()
                                .filter(|e| e.starts_with("seg:"))
                                .map(|_| 0),
                        )
                        .collect();
                    assert_eq!(got.iter().sum::<usize>(), sizes.iter().sum::<usize>());
                }
                // Plain writes produce the same bytes.
                let (_, plain) = run(
                    sans_io::write(&structure, &updates, false),
                    data,
                    Vec::new(),
                    reads,
                );
                assert_eq!(plain.out, trace.out, "{name}/{scenario} plain write");
                assert!(plain.events.is_empty());
                assert_eq!(merged(&plain.events), Vec::<String>::new());
            }
            (Err(_), Err(_)) => {}
            (Err(e), Ok(_)) => {
                panic!("{name}/{scenario}: Asset failed ({e}) but machine succeeded")
            }
            (Ok(_), Err(_)) => panic!("{name}/{scenario}: machine failed but Asset succeeded"),
        }
    }
}

#[test]
fn reads_are_independent_of_read_sizes() {
    for name in FIXTURES {
        let data = fixture(name);
        check_reads(name, &data, Reads::Full);
        check_reads(
            name,
            &data,
            Reads::Random {
                seed: 0x9e37_79b9,
                max: 997,
            },
        );
        if data.len() < 300_000 {
            check_reads(name, &data, Reads::OneByte);
        }
    }
}

#[test]
fn writes_are_independent_of_read_sizes() {
    for name in FIXTURES {
        let data = fixture(name);
        check_writes(name, &data, Reads::Full);
        check_writes(
            name,
            &data,
            Reads::Random {
                seed: 0xdead_beef,
                max: 4093,
            },
        );
    }
}

#[test]
fn update_segment_matches_structure_update_segment() {
    for name in [
        "P1000708.jpg",
        "sample1.png",
        "sample1.heic",
        "sample1.webp",
    ] {
        let data = fixture(name);
        let updates = Updates::new().set_jumbf(vec![0u8; 4000]);
        let mut asset = Asset::from_source(Cursor::new(data.clone())).unwrap();
        let mut out = Cursor::new(Vec::new());
        let dest = asset.write(&mut out, &updates).unwrap();
        let written = out.into_inner();

        let manifest = vec![0xab; 3000];
        let mut expected = Cursor::new(written.clone());
        let n = dest
            .update_segment(&mut expected, SegmentKind::Jumbf, manifest.clone())
            .unwrap();

        let (m, trace) = run(
            sans_io::update_segment(&dest, SegmentKind::Jumbf, manifest),
            &[],
            written,
            Reads::Full,
        );
        assert_eq!(m, n, "{name}");
        assert!(
            trace.reads.is_empty(),
            "{name}: in-place update must not read"
        );
        assert_eq!(trace.out, expected.into_inner(), "{name}");
    }
}

#[test]
fn progress_reports_totals() {
    let data = fixture("sample1.png");
    let (structure, _) = run(
        sans_io::parse(data.len() as u64),
        &data,
        Vec::new(),
        Reads::Full,
    );
    let updates = Updates::new();
    let mut m = sans_io::read_process(&structure, &updates);
    let mut last = 0;
    loop {
        match m.step().unwrap() {
            Step::Read { offset, buf } => {
                let n = buf.len().min(data.len() - offset as usize);
                buf[..n].copy_from_slice(&data[offset as usize..offset as usize + n]);
                m.filled(n).unwrap();
            }
            Step::Process(_) => {}
            Step::Done => break,
            Step::Write { .. } => unreachable!(),
        }
        let p = m.progress();
        assert!(p.bytes_processed >= last);
        last = p.bytes_processed;
    }
    let p = m.progress();
    assert_eq!(p.total, Some(data.len() as u64));
    assert_eq!(p.bytes_processed, data.len() as u64);
    m.finish().unwrap();
}

#[test]
fn dropping_a_machine_cancels_it() {
    let data = fixture("sample1.m4a");
    let (structure, _) = run(
        sans_io::parse(data.len() as u64),
        &data,
        Vec::new(),
        Reads::Full,
    );
    let updates = Updates::new().set_xmp(b"<x/>".to_vec());
    let mut m = sans_io::write(&structure, &updates, true);
    for _ in 0..10 {
        match m.step().unwrap() {
            Step::Read { offset, buf } => {
                let n = buf.len().min(data.len() - offset as usize);
                buf[..n].copy_from_slice(&data[offset as usize..offset as usize + n]);
                m.filled(n).unwrap();
            }
            Step::Done => panic!("finished too early"),
            _ => {}
        }
    }
    assert!(!m.is_done());
    drop(m);
}

#[test]
fn protocol_misuse_is_an_error() {
    let data = fixture("sample1.png");
    let mut m = sans_io::parse(data.len() as u64);
    assert!(matches!(m.step().unwrap(), Step::Read { .. }));
    assert!(matches!(m.step(), Err(Error::Protocol(_))));

    let mut m = sans_io::parse(data.len() as u64);
    let len = match m.step().unwrap() {
        Step::Read { buf, .. } => buf.len(),
        _ => unreachable!(),
    };
    assert!(matches!(m.filled(len + 1), Err(Error::Protocol(_))));

    let m = sans_io::parse(data.len() as u64);
    assert!(m.finish().is_err());
}

#[test]
fn processor_errors_stop_blocking_writes() {
    let data = fixture("P1000708.jpg");
    let mut asset = Asset::from_source(Cursor::new(data)).unwrap();
    let mut calls = 0;
    let err = asset
        .write_with_processing(
            &mut Cursor::new(Vec::new()),
            &Updates::new(),
            &mut |_: &dyn ProcessChunk| {
                calls += 1;
                if calls == 3 {
                    Err(Error::UserCanceled)
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::UserCanceled));
    assert_eq!(calls, 3);
}

#[test]
fn early_eof_is_reported() {
    let data = fixture("P1000708.jpg");
    // Claim the source is longer than the bytes we can deliver.
    let mut m = sans_io::parse(data.len() as u64 + 1000);
    let truncated = &data[..data.len() / 2];
    let err = loop {
        match m.step() {
            Ok(Step::Read { offset, buf }) => {
                let start = (offset as usize).min(truncated.len());
                let n = buf.len().min(truncated.len() - start);
                buf[..n].copy_from_slice(&truncated[start..start + n]);
                m.filled(n).unwrap();
            }
            Ok(Step::Done) => break m.finish().err(),
            Ok(_) => {}
            Err(e) => break Some(e),
        }
    };
    assert!(err.is_some(), "truncated source must fail");
}
