//! C2PA data hash example using asset-io
//!
//! Demonstrates creating a C2PA manifest using data hashing with a single output file.
//!
//! ## Workflow
//!
//! 1. Open source asset
//! 2. Create C2PA builder with actions/assertions
//! 3. Generate placeholder manifest (reserves space)
//! 4. Write output with placeholder
//! 5. Hash output (zero-copy mmap, excluding manifest)
//! 6. Sign final manifest with hash
//! 7. Overwrite manifest bytes in-place
//! 8. Verify output
//!
//! Run: `cargo run --example c2pa --features xmp,png tests/fixtures/sample1.png`

use asset_io::{Asset, Updates};
use c2pa::{
    assertions::{c2pa_action, Action, DataHash, DigitalSourceType},
    hash_stream_by_alg,
    settings::Settings,
    Builder, ClaimGeneratorInfo, HashRange, Reader,
};
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

/// Generate a DataHash for an asset by hashing it while excluding the C2PA manifest.
///
/// This function:
/// 1. Finds the C2PA JUMBF segment in the asset
/// 2. Creates exclusion ranges for all the manifest's byte ranges
/// 3. Hashes the asset excluding those ranges
/// 4. Returns a DataHash ready to be used in C2PA signing
///
/// # Arguments
/// * `asset` - The asset to hash (must have a C2PA JUMBF segment)
/// * `algorithm` - Hash algorithm name (e.g., "sha256")
///
/// # Returns
/// A DataHash containing the hash and exclusion information
fn generate_data_hash_for_asset<R: Read + Seek>(
    asset: &mut Asset<R>,
    algorithm: &str,
) -> Result<DataHash, Box<dyn std::error::Error>> {
    // Find the C2PA JUMBF segment
    let manifest_segment_idx = asset
        .structure()
        .c2pa_jumbf_index()
        .ok_or("No C2PA JUMBF segment found in asset")?;
    let manifest_segment = &asset.structure().segments[manifest_segment_idx];

    // Calculate manifest location and total size across all ranges
    let manifest_offset = manifest_segment.ranges[0].offset;
    let total_size: u64 = manifest_segment.ranges.iter().map(|r| r.size).sum();

    // Create DataHash with exclusion
    let mut dh = DataHash::new("jumbf_manifest", algorithm);
    let hr = HashRange::new(manifest_offset, total_size);
    dh.add_exclusion(hr.clone());

    // Hash the asset excluding the manifest
    let source = asset.source_mut();
    source.seek(SeekFrom::Start(0))?;
    let hash = hash_stream_by_alg(algorithm, source, Some(vec![hr]), true)?;
    dh.set_hash(hash);

    Ok(dh)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <image_file>", args[0]);
        return Ok(());
    }

    let source_path = &args[1];

    // Load settings and signer
    let settings_str = std::fs::read_to_string("tests/fixtures/test_settings.json")?;
    Settings::from_string(&settings_str, "json")?;
    let signer = Settings::signer()?;

    // Open source asset
    let mut asset = Asset::open(source_path)?;
    let mime_type = asset.media_type().to_mime();
    let extension = asset.media_type().to_extension();
    let output_path = format!("target/output_c2pa.{}", extension);

    // Create C2PA builder
    let mut builder = Builder::default();
    let mut claim_generator = ClaimGeneratorInfo::new("asset-io-example".to_string());
    claim_generator.set_version("0.1");
    builder
        .set_claim_generator_info(claim_generator)
        .add_action(Action::new(c2pa_action::CREATED).set_source_type(DigitalSourceType::Empty))?;

    // Create placeholder manifest and write output
    let placeholder_manifest =
        builder.data_hashed_placeholder(signer.reserve_size(), "application/c2pa")?;
    let updates = Updates::new().set_jumbf(placeholder_manifest.clone());
    asset.write_to(&output_path, &updates)?;

    // Open output for hashing
    let mut output_asset = Asset::open(&output_path)?;
    let manifest_segment_idx = output_asset
        .structure()
        .c2pa_jumbf_index()
        .ok_or("No C2PA JUMBF segment found in output structure")?;
    let manifest_ranges = output_asset.structure().segments[manifest_segment_idx].ranges.clone();

    // Hash output (excluding manifest)
    let dh = generate_data_hash_for_asset(&mut output_asset, "sha256")?;

    // Sign and create final manifest
    let mut final_manifest = builder.sign_data_hashed_embeddable(&signer, &dh, "application/c2pa")?;

    // Validate and pad final manifest
    if final_manifest.len() > placeholder_manifest.len() {
        return Err(format!(
            "Final manifest ({} bytes) is larger than placeholder ({} bytes)",
            final_manifest.len(),
            placeholder_manifest.len()
        )
        .into());
    }
    if final_manifest.len() < placeholder_manifest.len() {
        final_manifest.resize(placeholder_manifest.len(), 0);
    }

    // Overwrite manifest bytes in-place
    let mut output_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&output_path)?;

    let mut bytes_written = 0usize;
    for range in manifest_ranges.iter() {
        output_file.seek(SeekFrom::Start(range.offset))?;
        let remaining = final_manifest.len() - bytes_written;
        let to_write = remaining.min(range.size as usize);
        output_file.write_all(&final_manifest[bytes_written..bytes_written + to_write])?;
        bytes_written += to_write;
        if bytes_written >= final_manifest.len() {
            break;
        }
    }
    output_file.flush()?;

    // Verify output
    let _verify_asset = Asset::open(&output_path)?;
    let mut verify_file = File::open(&output_path)?;
    let _reader = Reader::from_stream(mime_type, &mut verify_file)?;

    Ok(())
}
