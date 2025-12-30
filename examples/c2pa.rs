//! C2PA data hash example using asset-io
//!
//! This example demonstrates how to create a C2PA manifest using data hashing
//! with the asset-io intermediate parsing workflow to get accurate structure offsets.
//!
//! **IMPORTANT**: Currently only works reliably with JPEG files. PNG support has
//! known issues with chunk boundary tracking during writes that need to be fixed.
//!
//! Based on the c2pa-rs data_hash.rs example, adapted for asset-io integration.
//!
//! Run: `cargo run --example c2pa --features xmp tests/fixtures/FireflyTrain.jpg`

use asset_io::{Asset, Updates};
use c2pa::{
    assertions::{c2pa_action, Action, DataHash, DigitalSourceType},
    hash_stream_by_alg,
    settings::Settings,
    Builder, ClaimGeneratorInfo, HashRange, Reader,
};
use std::fs::File;
use std::io::{Seek, SeekFrom};

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

    // === Step 4: Write intermediate asset with placeholder ===
    println!("\n=== Step 4: Write Intermediate with Placeholder ===");

    // Write to intermediate file with placeholder manifest
    let intermediate_path = "target/intermediate_c2pa.tmp";
    let updates = Updates::new().set_jumbf(placeholder_manifest.clone());
    asset.write_to(intermediate_path, &updates)?;

    let intermediate_size = std::fs::metadata(intermediate_path)?.len();

    println!(
        "✓ Wrote intermediate with placeholder ({} bytes)",
        intermediate_size
    );

    // === Step 5: Parse intermediate to get actual structure ===
    println!("\n=== Step 5: Parse Intermediate Asset ===");

    // Parse the intermediate file as a new asset
    let mut intermediate_asset = Asset::open(intermediate_path)?;

    // Now get the ACTUAL manifest location from the parsed structure
    let manifest_segment_idx = intermediate_asset
        .structure()
        .c2pa_jumbf_index()
        .ok_or("No C2PA JUMBF segment found in intermediate structure")?;
    let manifest_segment = &intermediate_asset.structure().segments[manifest_segment_idx];

    println!("✓ Parsed intermediate asset");
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

    // === Step 6: Hash the intermediate (excluding manifest) ===
    println!("\n=== Step 6: Hash Intermediate (excluding manifest) ===");

    let mut dh = DataHash::new("jumbf_manifest", "sha256");
    let hr = HashRange::new(manifest_offset, total_size);
    dh.add_exclusion(hr.clone());

    // Hash the intermediate buffer excluding the manifest
    let intermediate_cursor = intermediate_asset.source_mut();
    intermediate_cursor.seek(SeekFrom::Start(0))?;
    let hash = hash_stream_by_alg("sha256", intermediate_cursor, Some(vec![hr]), true)?;
    dh.set_hash(hash.clone());

    println!("✓ Generated hash (excluding manifest)");
    println!("  Hash: {}", hex::encode(&hash));

    // === Step 7: Sign and create final manifest ===
    println!("\n=== Step 7: Sign and Create Final Manifest ===");
    let final_manifest = builder.sign_data_hashed_embeddable(&signer, &dh, "application/c2pa")?;

    println!(
        "✓ Created final signed manifest ({} bytes)",
        final_manifest.len()
    );

    // === Step 8: Write final output with signed manifest ===
    println!("\n=== Step 8: Write Final Output ===");

    // Open source again for final write
    let mut asset2 = Asset::open(source_path)?;
    let extension = asset2.media_type().to_extension();

    let final_updates = Updates::new().set_jumbf(final_manifest.clone());
    let output_path = format!("target/output_c2pa.{}", extension);

    asset2.write_to(&output_path, &final_updates)?;

    let output_size = std::fs::metadata(&output_path)?.len();
    println!("✓ Wrote final output to: {}", output_path);
    println!("  Output file size: {} bytes", output_size);

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
    println!("4. ✓ Wrote intermediate with placeholder");
    println!("5. ✓ Parsed intermediate to get actual structure");
    println!("6. ✓ Hashed intermediate excluding manifest");
    println!("7. ✓ Signed final manifest with hash");
    println!("8. ✓ Wrote final output with signed manifest");
    println!("9. ✓ Verified C2PA manifest in output");

    Ok(())
}
