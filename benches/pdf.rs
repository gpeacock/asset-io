//! Range-only PDF parsing benchmark: cargo bench --features pdf --bench pdf

use asset_io::Asset;
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
    let mut group = c.benchmark_group("pdf_parse");
    for payload_size in [1024, 1024 * 1024, 8 * 1024 * 1024] {
        let data = document(payload_size);
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let asset =
                        Asset::from_source(Cursor::new(black_box(data.as_slice()))).unwrap();
                    black_box(asset.structure());
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, parse);
criterion_main!(benches);
