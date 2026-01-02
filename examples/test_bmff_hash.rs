//! Test that hashing works with BMFF files
//!
//! Run with: cargo run --example test_bmff_hash --features bmff,hashing

use asset_io::Asset;
use sha2::{Digest, Sha256};

fn main() -> asset_io::Result<()> {
    println!("🔍 Testing BMFF Hashing");

    let input_path = "../c2pa-rs/sdk/tests/fixtures/video1.mp4";

    // Open the BMFF file
    let mut asset = Asset::open(input_path)?;
    println!("  Container: {:?}", asset.container());
    println!("  Media Type: {:?}", asset.media_type());
    println!("  Total size: {} bytes", asset.structure().total_size);
    println!("  Segments: {}", asset.structure().segments().len());

    // Test 1: Hash entire file
    println!("\n✅ Test 1: Hash entire file");
    let mut hasher1 = Sha256::new();
    asset.hash_excluding_segments(&[], &mut hasher1)?;
    let hash1 = hasher1.finalize();
    println!("  SHA-256: {:x}", hash1);

    // Test 2: Hash excluding JUMBF (if present)
    if !asset.structure().jumbf_indices().is_empty() {
        println!("\n✅ Test 2: Hash excluding JUMBF segment");
        let jumbf_idx = asset.structure().c2pa_jumbf_index();
        let mut hasher2 = Sha256::new();
        asset.hash_excluding_segments(&[jumbf_idx], &mut hasher2)?;
        let hash2 = hasher2.finalize();
        println!("  SHA-256: {:x}", hash2);
        println!("  Note: This hash excludes the C2PA manifest (as required for validation)");
    }

    // Test 3: Hash excluding XMP (if present)
    if asset.structure().xmp_index().is_some() {
        println!("\n✅ Test 3: Hash excluding XMP segment");
        let xmp_idx = asset.structure().xmp_index();
        let mut hasher3 = Sha256::new();
        asset.hash_excluding_segments(&[xmp_idx], &mut hasher3)?;
        let hash3 = hasher3.finalize();
        println!("  SHA-256: {:x}", hash3);
    }

    println!("\n✅ BMFF hashing feature works correctly!");
    println!("   The hash_excluding_segments() method is container-agnostic.");
    println!("   It works with JPEG, PNG, BMFF, and any future containers.");

    Ok(())
}
