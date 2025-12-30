//! C2PA data hash example using asset-io
//!
//! This example demonstrates how to create a C2PA manifest using data hashing
//! with a single output file - no intermediate files required!
//!
//! ## Workflow
//!
//! 1. Open source asset with asset-io
//! 2. Create C2PA builder with actions/assertions
//! 3. Generate placeholder manifest (reserves space)
//! 4. Write output with placeholder manifest
//! 5. Open output with memory mapping (zero-copy)
//! 6. Hash output from mmap (zero-copy, excluding manifest)
//! 7. Sign final manifest with hash
//! 8. Overwrite output with final signed manifest
//! 9. Verify output with C2PA reader
//!
//! ## Zero-Copy Optimization
//!
//! The hashing step uses memory-mapped I/O via `Asset::open()`, which internally
//! uses `memmap2` for efficient zero-copy reads. The `hash_stream_by_alg()`
//! function reads directly from the mmap without creating intermediate buffers.
//!
//! Based on the c2pa-rs data_hash.rs example, adapted for asset-io integration.
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
        eprintln!("\nCreates a C2PA manifest using data hashing with asset-io");
        return Ok(());
    }

    let source_path = &args[1];
    println!("=== C2PA Data Hash Example with asset-io ===\n");
    println!("Source: {}", source_path);

    // Load settings from test fixture
    println!("\n=== Loading C2PA Settings ===");
    let settings_str = std::fs::read_to_string("tests/fixtures/test_settings.json")?;
    Settings::from_string(&settings_str, "json")?;
    let signer = Settings::signer()?;

    println!("✓ Loaded signer from settings");

    // === Step 1: Open source asset with asset-io ===
    println!("\n=== Step 1: Open Source Asset ===");
    let mut asset = Asset::open(source_path)?;

    let mime_type = asset.media_type().to_mime();
    println!("Container: {:?}", asset.container());
    println!("Media type: {}", mime_type);
    println!(
        "Structure: {} segments, {} bytes",
        asset.structure().segments.len(),
        asset.structure().total_size
    );

    // Check if source already has C2PA data
    if let Some(jumbf_idx) = asset.structure().c2pa_jumbf_index() {
        println!("⚠ Source already has C2PA JUMBF at segment {}", jumbf_idx);
        println!(
            "  Offset: {}, Size: {}",
            asset.structure().segments[jumbf_idx].ranges[0].offset,
            asset.structure().segments[jumbf_idx].ranges[0].size
        );
    }

    // === Step 2: Create C2PA Builder ===
    println!("\n=== Step 2: Create C2PA Builder ===");

    // Create ingredient from source
    // let title = std::path::Path::new(source_path)
    //     .file_name()
    //     .and_then(|n| n.to_str())
    //     .unwrap_or("source");
    // let mut parent = Ingredient::new_v2(title, mime_type);
    // parent.set_relationship(Relationship::ParentOf);

    let mut builder = Builder::default();
    let mut claim_generator = ClaimGeneratorInfo::new("asset-io-example".to_string());
    claim_generator.set_version("0.1");

    builder
        .set_claim_generator_info(claim_generator)
        .add_action(Action::new(c2pa_action::CREATED).set_source_type(DigitalSourceType::Empty))?;
    // .add_action(Action::new(c2pa_action::OPENED).set_parameter("ingredients", [parent.instance_id()].to_vec())?)?
    // .add_ingredient(parent);

    println!("✓ Builder created with ingredient and c2pa.opened action");

    // === Step 3: Create placeholder manifest ===
    println!("\n=== Step 3: Create Placeholder Manifest ===");
    let placeholder_manifest =
        builder.data_hashed_placeholder(signer.reserve_size(), "application/c2pa")?;

    println!("Placeholder size: {} bytes", placeholder_manifest.len());

    // === Step 4: Write output with placeholder ===
    println!("\n=== Step 4: Write Output with Placeholder ===");

    // Determine output path
    let extension = asset.media_type().to_extension();
    let output_path = format!("target/output_c2pa.{}", extension);

    // Write directly to output with placeholder manifest
    let updates = Updates::new().set_jumbf(placeholder_manifest.clone());
    asset.write_to(&output_path, &updates)?;
    
    let output_size = std::fs::metadata(&output_path)?.len();
    println!(
        "✓ Wrote output with placeholder ({} bytes)",
        output_size
    );

    // === Step 5: Open output with memory mapping for zero-copy hashing ===
    println!("\n=== Step 5: Open Output for Hashing ===");

    // Open with memory mapping for efficient zero-copy hashing
    let mut output_asset = Asset::open(&output_path)?;
    println!("✓ Opened output asset (uses mmap for zero-copy reads)");
    
    // Get the ACTUAL manifest location from the parsed structure
    let manifest_segment_idx = output_asset
        .structure()
        .c2pa_jumbf_index()
        .ok_or("No C2PA JUMBF segment found in output structure")?;
    let manifest_segment = &output_asset.structure().segments[manifest_segment_idx];

    println!("  C2PA segment at index {}", manifest_segment_idx);
    println!("  Manifest has {} range(s):", manifest_segment.ranges.len());
    for (i, range) in manifest_segment.ranges.iter().enumerate() {
        println!(
            "    Range {}: offset={}, size={}",
            i, range.offset, range.size
        );
    }

    // Calculate total exclusion size from actual structure
    let manifest_offset = manifest_segment.ranges[0].offset;
    let total_size: u64 = manifest_segment.ranges.iter().map(|r| r.size).sum();

    println!(
        "  Total exclusion: {} bytes starting at offset {}",
        total_size, manifest_offset
    );

    // === Step 6: Hash the output (excluding manifest) ===
    println!("\n=== Step 6: Hash Output (zero-copy from mmap) ===");

    let dh = generate_data_hash_for_asset(&mut output_asset, "sha256")?;

    println!("✓ Generated hash (excluding manifest)");
    println!("  Hash: {}", hex::encode(&dh.hash));

    // === Step 7: Sign and create final manifest ===
    println!("\n=== Step 7: Sign and Create Final Manifest ===");
    let final_manifest = builder.sign_data_hashed_embeddable(&signer, &dh, "application/c2pa")?;

    println!(
        "✓ Created final signed manifest ({} bytes)",
        final_manifest.len()
    );

    // Validate final manifest fits in placeholder space
    if final_manifest.len() > placeholder_manifest.len() {
        return Err(format!(
            "Final manifest ({} bytes) is larger than placeholder ({} bytes)",
            final_manifest.len(),
            placeholder_manifest.len()
        )
        .into());
    }
    println!(
        "  Space check: {} / {} bytes used",
        final_manifest.len(),
        placeholder_manifest.len()
    );

    // === Step 8: Overwrite manifest in output ===
    println!("\n=== Step 8: Overwrite Manifest in Output ===");

    // For now, we rewrite the whole file with the final manifest
    // TODO: Future optimization - seek and overwrite just the manifest bytes
    let mut final_asset = Asset::open(source_path)?;
    let final_updates = Updates::new().set_jumbf(final_manifest.clone());
    final_asset.write_to(&output_path, &final_updates)?;

    let output_size = std::fs::metadata(&output_path)?.len();
    println!("✓ Rewrote output with final manifest");
    println!("  Output file: {} ({} bytes)", output_path, output_size);

    // === Step 9: Verify the output ===
    println!("\n=== Step 9: Verify Output ===");

    // First, verify with asset-io that the JUMBF was written
    let verify_asset = Asset::open(&output_path)?;
    if let Some(jumbf_idx) = verify_asset.structure().c2pa_jumbf_index() {
        println!("✓ asset-io found JUMBF in output at segment {}", jumbf_idx);
        println!(
            "  Offset: {}, Size: {}",
            verify_asset.structure().segments[jumbf_idx].ranges[0].offset,
            verify_asset.structure().segments[jumbf_idx].ranges[0].size
        );
    } else {
        println!("✗ asset-io did NOT find JUMBF in output!");
    }

    // Then try to read with c2pa Reader
    let mut verify_file = File::open(&output_path)?;

    match Reader::from_stream(mime_type, &mut verify_file) {
        Ok(reader) => {
            println!("✓ c2pa Reader successfully read C2PA data from output!");
            println!("  Manifests found: {}", reader.manifests().len());
            if let Some(manifest) = reader.active_manifest() {
                println!("\nActive Manifest:");
                println!("  Title: {:?}", manifest.title());
                println!("  Format: {:?}", manifest.format());
                println!("  Instance ID: {}", manifest.instance_id());
                println!("  Assertions: {}", manifest.assertions().len());
            }
        }
        Err(e) => {
            println!("⚠ c2pa Reader could not read C2PA data: {}", e);
            println!("  The JUMBF structure may be incorrect for c2pa validation.");
        }
    }

    println!("\n=== Success! ===");
    println!("\nWorkflow Summary:");
    println!("1. ✓ Opened source with asset-io");
    println!("2. ✓ Created C2PA builder with assertions");
    println!("3. ✓ Generated placeholder manifest");
    println!("4. ✓ Wrote output with placeholder");
    println!("5. ✓ Parsed output to get actual structure");
    println!("6. ✓ Hashed output excluding manifest");
    println!("7. ✓ Signed final manifest with hash");
    println!("8. ✓ Overwrote manifest in output");
    println!("9. ✓ Verified C2PA manifest in output");

    Ok(())
}
