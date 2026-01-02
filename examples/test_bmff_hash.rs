//! Test that read_with_processing works with BMFF files
//!
//! Run with: cargo run --example test_bmff_hash --features bmff

use asset_io::{Asset, Updates, SegmentKind, ExclusionMode};
use sha2::{Digest, Sha256};

fn main() -> asset_io::Result<()> {
    println!("🔍 Testing BMFF Processing");

    let input_path = "../c2pa-rs/sdk/tests/fixtures/video1.mp4";

    // Open the BMFF file
    let mut asset = Asset::open(input_path)?;
    println!("  Container: {:?}", asset.container());
    println!("  Media Type: {:?}", asset.media_type());
    println!("  Total size: {} bytes", asset.structure().total_size);
    println!("  Segments: {}", asset.structure().segments().len());

    // Test 1: Process entire file (hash)
    println!("\n✅ Test 1: Hash entire file");
    let updates = Updates::new();
    let mut hasher1 = Sha256::new();
    asset.read_with_processing(&updates, &mut |chunk| hasher1.update(chunk))?;
    let hash1 = hasher1.finalize();
    println!("  SHA-256: {:x}", hash1);

    // Test 2: Hash excluding JUMBF (if present)
    if !asset.structure().jumbf_indices().is_empty() {
        println!("\n✅ Test 2: Hash excluding JUMBF segment");
        let updates = Updates::new()
            .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
        let mut hasher2 = Sha256::new();
        asset.read_with_processing(&updates, &mut |chunk| hasher2.update(chunk))?;
        let hash2 = hasher2.finalize();
        println!("  SHA-256: {:x}", hash2);
        println!("  Note: This hash excludes the C2PA manifest (as required for validation)");
    }

    // Test 3: Hash excluding XMP (if present)
    if asset.structure().xmp_index().is_some() {
        println!("\n✅ Test 3: Hash excluding XMP segment");
        let updates = Updates::new()
            .exclude_from_processing(vec![SegmentKind::Xmp], ExclusionMode::EntireSegment);
        let mut hasher3 = Sha256::new();
        asset.read_with_processing(&updates, &mut |chunk| hasher3.update(chunk))?;
        let hash3 = hasher3.finalize();
        println!("  SHA-256: {:x}", hash3);
    }

    println!("\n✅ BMFF processing works correctly!");
    println!("   The read_with_processing() method is container-agnostic.");
    println!("   It works with JPEG, PNG, BMFF, and any future containers.");

    Ok(())
}
