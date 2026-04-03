//! Basic benchmarks for reading and signing various file formats.
//!
//! Run with: cargo bench --features all-formats,xmp

use asset_io::{Asset, ExclusionMode, ProcessChunk, SegmentKind, Updates};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures");
    path.push(name);
    path
}

fn bench_read(c: &mut Criterion, name: &str, fixture: &str) {
    let path = fixture_path(fixture);
    if !path.exists() {
        return;
    }
    let path = path.to_string_lossy().to_string();

    c.bench_function(&format!("read_{}", name), |b| {
        b.iter(|| {
            let mut asset = Asset::open(&path).unwrap();
            black_box(asset.structure());
            let _ = black_box(asset.xmp());
            let _ = black_box(asset.jumbf());
        })
    });
}

fn bench_sign(c: &mut Criterion, name: &str, fixture: &str) {
    #[cfg(not(feature = "xmp"))]
    return;

    let path = fixture_path(fixture);
    if !path.exists() {
        return;
    }
    let input_path = path.to_string_lossy().to_string();

    c.bench_function(&format!("sign_{}", name), |b| {
        b.iter(|| {
            use c2pa::{
                assertions::{c2pa_action, Action, BmffHash, DataHash},
                Builder, ClaimGeneratorInfo, HashRange, Settings,
            };
            use sha2::{Digest, Sha256};
            use std::io::Write;

            let settings_str = std::fs::read_to_string(fixture_path("test_settings.json")).unwrap();
            let settings = Settings::from_string(&settings_str, "json").unwrap();

            let context = c2pa::Context::new()
                .with_settings(settings)
                .unwrap()
                .into_shared();
            let mut builder = Builder::from_shared_context(&context);

            let mut claim_generator = ClaimGeneratorInfo::new("asset-io-bench".to_string());
            claim_generator.set_version("0.1.0");
            builder.set_claim_generator_info(claim_generator);
            builder
                .add_action(
                    Action::new(c2pa_action::CREATED)
                        .set_parameter("identifier", &input_path)
                        .unwrap(),
                )
                .unwrap();

            let mut asset = Asset::open(&input_path).unwrap();
            let native_format = asset.media_type().to_mime();
            let is_bmff = matches!(asset.structure().container, asset_io::ContainerKind::Bmff);

            let signer = Settings::signer().unwrap();

            if builder.needs_placeholder(&native_format) {
                let (structure, mut output_file, signed_jumbf) = if is_bmff {
                    let mut placeholder_bmff = BmffHash::new("jumbf manifest", "sha256", None);
                    placeholder_bmff.set_default_exclusions();
                    placeholder_bmff.add_place_holder_hash().unwrap();
                    builder
                        .add_assertion(&format!("{}.v3", BmffHash::LABEL), &placeholder_bmff)
                        .unwrap();

                    let mut placeholder_jumbf = builder.placeholder("application/c2pa").unwrap();
                    placeholder_jumbf.extend(std::iter::repeat(0u8).take(256 * 1024));

                    let updates = Updates::new()
                        .set_jumbf(placeholder_jumbf.clone())
                        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

                    let output_file = tempfile::NamedTempFile::new().unwrap();
                    let output_path = output_file.path().to_path_buf();
                    let mut output_file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&output_path)
                        .unwrap();

                    let mut processor = |chunk: &dyn ProcessChunk| {
                        if let Some(id) = chunk.id() {
                            builder
                                .hash_bmff_mdat_bytes(
                                    id,
                                    chunk.data(),
                                    chunk.large_size().unwrap_or(false),
                                )
                                .map_err(|e| asset_io::Error::InvalidFormat(e.to_string()))?;
                        }
                        Ok(())
                    };
                    let structure = asset
                        .write_with_processing(&mut output_file, &updates, &mut processor)
                        .unwrap();
                    output_file.flush().unwrap();
                    let signed_jumbf = builder.sign_embeddable("application/c2pa").unwrap();
                    (structure, output_file, signed_jumbf)
                } else {
                    let placeholder_jumbf = builder.placeholder("application/c2pa").unwrap();

                    let updates = Updates::new()
                        .set_jumbf(placeholder_jumbf.clone())
                        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

                    let output_file = tempfile::NamedTempFile::new().unwrap();
                    let output_path = output_file.path().to_path_buf();
                    let mut output_file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&output_path)
                        .unwrap();

                    let mut hasher = Sha256::new();
                    let mut processor = |chunk: &dyn ProcessChunk| {
                        hasher.update(chunk.data());
                        Ok(())
                    };
                    let structure = asset
                        .write_with_processing(&mut output_file, &updates, &mut processor)
                        .unwrap();
                    output_file.flush().unwrap();

                    let (exclusion_offset, exclusion_size) = structure
                        .exclusion_range_for_segment(SegmentKind::Jumbf)
                        .unwrap();

                    let mut data_hash = DataHash::new("jumbf_manifest", "sha256");
                    data_hash.add_exclusion(HashRange::new(exclusion_offset, exclusion_size));
                    data_hash.set_hash(hasher.finalize().to_vec());

                    let signed_jumbf = builder
                        .sign_data_hashed_embeddable(
                            signer.as_ref(),
                            &data_hash,
                            "application/c2pa",
                        )
                        .unwrap();
                    (structure, output_file, signed_jumbf)
                };

                structure
                    .update_segment(&mut output_file, SegmentKind::Jumbf, signed_jumbf)
                    .unwrap();
                output_file.flush().unwrap();
            }
        })
    });
}

fn io_benches(c: &mut Criterion) {
    #[cfg(feature = "jpeg")]
    {
        bench_read(c, "jpeg", "FireflyTrain.jpg");
        bench_sign(c, "jpeg", "FireflyTrain.jpg");
    }

    #[cfg(feature = "png")]
    {
        bench_read(c, "png", "sample1.png");
        bench_sign(c, "png", "sample1.png");
    }

    #[cfg(feature = "bmff")]
    {
        bench_read(c, "heic", "sample1.heic");
        bench_sign(c, "heic", "sample1.heic");
    }

    #[cfg(feature = "riff")]
    {
        bench_read(c, "webp", "sample1.webp");
    }
}

criterion_group!(benches, io_benches);
criterion_main!(benches);
