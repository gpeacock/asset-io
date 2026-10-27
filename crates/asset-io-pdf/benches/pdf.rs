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

//! Parsing a PDF reads what the catalog points at, not the payloads, so its cost does
//! not grow with their size: `cargo bench -p asset-io-pdf --bench pdf`.

use asset_io_core::{HandlerRegistry, NullSink, Updates};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::io::Cursor;

fn document(payload_size: usize) -> Vec<u8> {
    let mut data = b"%PDF-1.7\n".to_vec();
    let catalog = data.len();
    data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Metadata 2 0 R >>\nendobj\n");
    let metadata = data.len();
    data.extend_from_slice(
        format!("2 0 obj\n<< /Type /Metadata /Subtype /XML /Length {payload_size} >>\nstream\n")
            .as_bytes(),
    );
    data.resize(data.len() + payload_size, b'x');
    data.extend_from_slice(b"\nendstream\nendobj\n");
    let xref = data.len();
    data.extend_from_slice(format!(
        "xref\n0 3\n0000000000 65535 f \n{catalog:010} 00000 n \n{metadata:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ).as_bytes());
    data
}

fn parse(c: &mut Criterion) {
    let registry = HandlerRegistry::new().with_handler(asset_io_pdf::PdfHandler);
    let mut group = c.benchmark_group("pdf_parse");
    for payload_size in [1024, 1024 * 1024, 8 * 1024 * 1024] {
        let data = document(payload_size);
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let structure = registry
                        .parse("pdf", &mut Cursor::new(black_box(data.as_slice())))
                        .unwrap();
                    black_box(structure);
                });
            },
        );
    }
    group.finish();
}

/// Planning an edit is independent of the payload size too: the source is copied, not read.
fn plan(c: &mut Criterion) {
    let registry = HandlerRegistry::new().with_handler(asset_io_pdf::PdfHandler);
    let mut group = c.benchmark_group("pdf_plan");
    for payload_size in [1024, 1024 * 1024, 8 * 1024 * 1024] {
        let data = document(payload_size);
        let structure = registry.parse("pdf", &mut Cursor::new(&data)).unwrap();
        let updates = Updates::new().set_xmp(b"<x:xmpmeta/>".to_vec());
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &data,
            |b, data| {
                b.iter(|| {
                    black_box(
                        structure
                            .plan(&mut Cursor::new(black_box(data.as_slice())), &updates)
                            .unwrap(),
                    );
                });
            },
        );
    }
    group.finish();
}

fn write(c: &mut Criterion) {
    let registry = HandlerRegistry::new().with_handler(asset_io_pdf::PdfHandler);
    let mut group = c.benchmark_group("pdf_write");
    for payload_size in [1024 * 1024, 8 * 1024 * 1024] {
        let data = document(payload_size);
        let structure = registry.parse("pdf", &mut Cursor::new(&data)).unwrap();
        let updates = Updates::new().set_xmp(b"<x:xmpmeta/>".to_vec());
        group.throughput(criterion::Throughput::Bytes(data.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut out = Vec::with_capacity(data.len() + 1024);
                    structure
                        .write(
                            &mut Cursor::new(black_box(data.as_slice())),
                            &mut out,
                            &updates,
                            &mut NullSink,
                        )
                        .unwrap();
                    black_box(out);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, parse, plan, write);
criterion_main!(benches);
