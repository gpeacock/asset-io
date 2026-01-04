//! Test BMFF hash computation by comparing write-time hash with post-write hash

use std::io::Write;
use asset_io::{Asset, Updates, SegmentKind, ExclusionMode};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source_path = "tests/fixtures/sample1.heic";
    let output_path = "target/test_bmff_hash.heif";
    
    // Create dummy JUMBF data
    let dummy_jumbf = vec![0u8; 1000];
    
    // === STEP 1: Write with hash computed during write ===
    println!("Step 1: Writing file and computing hash during write...");
    let mut asset = Asset::open(source_path)?;
    
    let updates = Updates::new()
        .set_jumbf(dummy_jumbf.clone())
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
    
    let mut output_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_path)?;
    
    let mut hasher1 = Sha256::new();
    let structure = asset.write_with_processing(
        &mut output_file,
        &updates,
        &mut |chunk| hasher1.update(chunk),
    )?;
    
    let hash1 = hasher1.finalize();
    println!("Hash during write: {:02x?}", &hash1[..16]);
    output_file.flush()?;
    drop(output_file);
    
    // === STEP 2: Read file back and compute hash ===
    println!("\nStep 2: Reading file back and computing hash...");
    let mut verify_asset = Asset::open(output_path)?;
    
    // Check segment ranges
    if let Some(idx) = verify_asset.structure().c2pa_jumbf_index() {
        let seg = &verify_asset.structure().segments()[idx];
        println!("  JUMBF segment has {} range(s)", seg.ranges.len());
        for (i, range) in seg.ranges.iter().enumerate() {
            println!("    Range {}: offset={}, size={}", i, range.offset, range.size);
        }
    }
    
    let updates2 = Updates::new()
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
    
    let mut hasher2 = Sha256::new();
    verify_asset.read_with_processing(&updates2, &mut |chunk| hasher2.update(chunk))?;
    
    let hash2 = hasher2.finalize();
    println!("Hash after read:   {:02x?}", &hash2[..16]);
    
    // === STEP 3: Compare ===
    if hash1 == hash2 {
        println!("\n✅ Hashes match! BMFF write_with_processor is working correctly.");
    } else {
        println!("\n❌ Hashes DON'T match!");
        println!("Write hash: {:02x?}", &hash1[..]);
        println!("Read hash:  {:02x?}", &hash2[..]);
        return Err("Hash mismatch".into());
    }
    
    // === STEP 4: Show structure ===
    println!("\nStructure info:");
    println!("  Total size: {} bytes", structure.total_size);
    println!("  Segments: {}", structure.segments().len());
    if let Some(idx) = structure.c2pa_jumbf_index() {
        let seg = &structure.segments()[idx];
        let loc = seg.location();
        println!("  JUMBF at offset {}, size {}", loc.offset, loc.size);
    }
    
    Ok(())
}
