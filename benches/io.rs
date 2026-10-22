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

//! Throughput benchmarks for parse, read and write.
//!
//! Run: `cargo bench --features all-formats` (add `memory-mapped` for the mmap group)
//!
//! Inputs are fixtures from `tests/fixtures` plus synthetic 32 MB JPEG and MP4
//! files. Set `BENCH_HUGE=1` to add `huge_test.png` (about 236 MB). Everything
//! except the mmap group works on in-memory bytes, so the numbers show CPU and
//! copy cost, not disk speed. `sha256` and `copy` are baselines: a read or write
//! cannot go faster than the work it feeds.

use asset_io::{Chunk, HandlerRegistry, NullSink, Result, Role, SegmentKind, Structure, Updates};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sha2::{Digest, Sha256};
use std::hint::black_box;
use std::io::Cursor;
use std::path::PathBuf;

const BIG: usize = 32 << 20;

struct Case {
    name: String,
    format: &'static str,
    bytes: Vec<u8>,
}

fn fixture(name: &str) -> Option<Vec<u8>> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    std::fs::read(p).ok()
}

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    v
}

/// Deterministic filler with no 0xFF bytes, so it is valid JPEG entropy data.
fn filler(n: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 255) as u8
        })
        .collect()
}

fn synthetic_jpeg(scan: usize) -> Vec<u8> {
    let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>";
    let sig = b"http://ns.adobe.com/xap/1.0/\0";
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE1];
    v.extend_from_slice(&((2 + sig.len() + xmp.len()) as u16).to_be_bytes());
    v.extend_from_slice(sig);
    v.extend_from_slice(xmp);
    v.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ]);
    v.extend_from_slice(&filler(scan));
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

/// ftyp, moov (with one stco entry that must follow the mdat when moov grows), mdat.
fn synthetic_mp4(mdat: usize) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"isom\0\0\0\0isomiso2");
    let stco = |off: u32| {
        let mut p = vec![0, 0, 0, 0, 0, 0, 0, 1];
        p.extend_from_slice(&off.to_be_bytes());
        bx(b"stco", &p)
    };
    let moov = |off: u32| {
        bx(
            b"moov",
            &bx(
                b"trak",
                &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stco(off)))),
            ),
        )
    };
    let moov_len = moov(0).len();
    let first_sample = (ftyp.len() + moov_len + 8) as u32;
    let mut v = ftyp;
    v.extend_from_slice(&moov(first_sample));
    v.extend_from_slice(&bx(b"mdat", &filler(mdat)));
    v
}

fn fake_jumbf() -> Vec<u8> {
    let mut jumd = vec![0u8; 16];
    jumd.push(3);
    jumd.extend_from_slice(b"c2pa\0");
    let mut payload = bx(b"jumd", &jumd);
    payload.extend_from_slice(&filler(4096));
    bx(b"jumb", &payload)
}

fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    let mut add = |name: &str, format: &'static str, bytes: Option<Vec<u8>>| {
        if let Some(bytes) = bytes {
            v.push(Case {
                name: name.into(),
                format,
                bytes,
            });
        }
    };
    add("jpeg_165K", "jpg", fixture("FireflyTrain.jpg"));
    add("png_2.4M", "png", fixture("GreenCat.png"));
    add("heic_293K", "heic", fixture("sample1.heic"));
    add("jpeg_32M", "jpg", Some(synthetic_jpeg(BIG)));
    add("mp4_32M", "mp4", Some(synthetic_mp4(BIG)));
    if std::env::var_os("BENCH_HUGE").is_some() {
        add("png_236M", "png", fixture("huge_test.png"));
    }
    v
}

fn parse(c: &Case) -> Structure {
    HandlerRegistry::builtin()
        .parse(c.format, &mut Cursor::new(&c.bytes))
        .unwrap_or_else(|e| panic!("{}: {e}", c.name))
}

fn group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(10)
        .warm_up_time(std::time::Duration::from_millis(500))
        .measurement_time(std::time::Duration::from_secs(2));
    g
}

fn bench_parse(c: &mut Criterion) {
    let mut g = group(c, "parse");
    for case in cases() {
        g.throughput(Throughput::Bytes(case.bytes.len() as u64));
        g.bench_function(BenchmarkId::from_parameter(&case.name), |b| {
            b.iter(|| black_box(parse(&case)))
        });
    }
    g.finish();
}

fn bench_read(c: &mut Criterion) {
    // Hashing is the cost that matters at scale; small files only repeat it.
    let mut g = group(c, "read_sha256");
    for case in cases().into_iter().filter(|c| c.bytes.len() >= BIG) {
        let s = parse(&case);
        g.throughput(Throughput::Bytes(case.bytes.len() as u64));

        g.bench_function(BenchmarkId::new("baseline", &case.name), |b| {
            b.iter(|| black_box(Sha256::digest(&case.bytes)))
        });
        g.bench_function(BenchmarkId::new("stream", &case.name), |b| {
            b.iter(|| {
                let mut h = Sha256::new();
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        h.update(c.data);
                        Ok(())
                    };
                    s.read(&mut Cursor::new(&case.bytes), &mut sink).unwrap();
                }
                black_box(h.finalize())
            })
        });
        g.bench_function(BenchmarkId::new("slice", &case.name), |b| {
            b.iter(|| {
                let mut h = Sha256::new();
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        h.update(c.data);
                        Ok(())
                    };
                    s.read_slice(&case.bytes, &mut sink).unwrap();
                }
                black_box(h.finalize())
            })
        });
    }
    g.finish();

    // Delivery cost without hashing, and the saving from skipping runs.
    let mut g = group(c, "read_count");
    for case in cases() {
        let s = parse(&case);
        g.throughput(Throughput::Bytes(case.bytes.len() as u64));
        g.bench_function(BenchmarkId::new("stream", &case.name), |b| {
            b.iter(|| {
                let mut n = 0u64;
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        n += c.data.len() as u64;
                        Ok(())
                    };
                    s.read(&mut Cursor::new(&case.bytes), &mut sink).unwrap();
                }
                black_box(n)
            })
        });
        g.bench_function(BenchmarkId::new("slice", &case.name), |b| {
            b.iter(|| {
                let mut n = 0u64;
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        n += c.data.len() as u64;
                        Ok(())
                    };
                    s.read_slice(&case.bytes, &mut sink).unwrap();
                }
                black_box(n)
            })
        });
        // Only metadata and framing: image data is never fetched.
        struct NoImage(u64);
        impl asset_io::Sink for NoImage {
            fn wants(&self, seg: &asset_io::Segment, _: Role) -> bool {
                seg.kind() != SegmentKind::ImageData && seg.extent().len() < (1 << 20)
            }
            fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
                self.0 += c.data.len() as u64;
                Ok(())
            }
        }
        g.bench_function(BenchmarkId::new("stream_skip_large", &case.name), |b| {
            b.iter(|| {
                let mut k = NoImage(0);
                s.read(&mut Cursor::new(&case.bytes), &mut k).unwrap();
                black_box(k.0)
            })
        });
    }
    g.finish();
}

fn bench_write(c: &mut Criterion) {
    let jumbf = fake_jumbf();
    let keep = Updates::new();
    let set = Updates::new().set_jumbf(jumbf);
    let mut g = group(c, "write");
    for case in cases() {
        let s = parse(&case);
        g.throughput(Throughput::Bytes(case.bytes.len() as u64));
        let mut out = Cursor::new(Vec::with_capacity(case.bytes.len() + (64 << 10)));

        g.bench_function(BenchmarkId::new("baseline_copy", &case.name), |b| {
            b.iter(|| {
                out.get_mut().clear();
                out.get_mut().extend_from_slice(&case.bytes);
                black_box(out.get_ref().len())
            })
        });
        for (label, updates) in [("keep", &keep), ("set_jumbf", &set)] {
            g.bench_function(BenchmarkId::new(label, &case.name), |b| {
                b.iter(|| {
                    out.get_mut().clear();
                    out.set_position(0);
                    let r = s
                        .write(
                            &mut Cursor::new(&case.bytes),
                            &mut out,
                            updates,
                            &mut NullSink,
                        )
                        .unwrap();
                    black_box(r.total_size())
                })
            });
        }
        // Writing while hashing the output, as a signer would.
        if case.bytes.len() < BIG {
            continue;
        }
        g.bench_function(BenchmarkId::new("set_jumbf_sha256", &case.name), |b| {
            b.iter(|| {
                out.get_mut().clear();
                out.set_position(0);
                let mut h = Sha256::new();
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        h.update(c.data);
                        Ok(())
                    };
                    s.write(&mut Cursor::new(&case.bytes), &mut out, &set, &mut sink)
                        .unwrap();
                }
                black_box(h.finalize())
            })
        });
    }
    g.finish();
}

fn bench_items(c: &mut Criterion) {
    let Some(bytes) = fixture("FireflyTrain.jpg") else {
        return;
    };
    let s = HandlerRegistry::builtin()
        .parse("jpg", &mut Cursor::new(&bytes))
        .unwrap();
    let Some(e) = s.items(SegmentKind::Xmp).next() else {
        return;
    };
    let mut g = group(c, "item");
    g.bench_function("read_item", |b| {
        b.iter(|| black_box(s.read_item(&mut Cursor::new(&bytes), e).unwrap()))
    });
    g.bench_function("slice_item", |b| {
        b.iter(|| black_box(s.slice_item(&bytes, e).unwrap()))
    });
    g.finish();
}

#[cfg(feature = "memory-mapped")]
fn bench_mmap(c: &mut Criterion) {
    use std::io::Write;
    let mut g = group(c, "mmap_sha256");
    for case in cases().into_iter().filter(|c| c.bytes.len() >= BIG) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&case.bytes).unwrap();
        f.flush().unwrap();
        g.throughput(Throughput::Bytes(case.bytes.len() as u64));

        g.bench_function(BenchmarkId::new("file_stream", &case.name), |b| {
            b.iter(|| {
                let mut file = std::fs::File::open(f.path()).unwrap();
                let s = HandlerRegistry::builtin()
                    .parse(case.format, &mut file)
                    .unwrap();
                let mut h = Sha256::new();
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        h.update(c.data);
                        Ok(())
                    };
                    s.read(&mut file, &mut sink).unwrap();
                }
                black_box(h.finalize())
            })
        });
        g.bench_function(BenchmarkId::new("mmap_slice", &case.name), |b| {
            b.iter(|| {
                let file = std::fs::File::open(f.path()).unwrap();
                // SAFETY: the temp file is not touched while mapped.
                let map = unsafe { asset_io::MappedFile::map(&file).unwrap() };
                let s = map.parse(&HandlerRegistry::builtin(), case.format).unwrap();
                let mut h = Sha256::new();
                {
                    let mut sink = |c: Chunk<'_>| -> Result<()> {
                        h.update(c.data);
                        Ok(())
                    };
                    s.read_slice(&map, &mut sink).unwrap();
                }
                black_box(h.finalize())
            })
        });
    }
    g.finish();
}

#[cfg(not(feature = "memory-mapped"))]
fn bench_mmap(_: &mut Criterion) {}

criterion_group!(
    benches,
    bench_parse,
    bench_read,
    bench_write,
    bench_items,
    bench_mmap
);
criterion_main!(benches);
