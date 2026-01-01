//! Demonstrates the efficient streaming write-hash-update workflow for C2PA
//!
//! This example shows how to:
//! 1. Write a file with placeholder JUMBF
//! 2. Hash the data during the write (single pass!)
//! 3. Generate the final C2PA manifest with the hash
//! 4. Update the JUMBF in-place before closing
//!
//! This is much more efficient than the traditional approach of:
//! write → close → reopen → hash → close → reopen → update → close
//!
//! Run with: cargo run --features jpeg,xmp,hashing --example c2pa_streaming

use asset_io::{Asset, SegmentKind, Updates, update_segment_with_structure};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;

fn main() -> asset_io::Result<()> {
    // Input file
    let input_path = "tests/fixtures/FireflyTrain.jpg";
    let output_path = "/tmp/c2pa_streaming_output.jpg";

    println!("=== Streaming Write-Hash-Update Workflow ===\n");

    // Step 1: Open source asset
    let mut asset = Asset::open(input_path)?;
    println!("📂 Opened: {}", input_path);

    // Step 2: Create placeholder JUMBF
    // In a real C2PA workflow, you'd generate a proper placeholder manifest
    let placeholder_size = 20000;
    let placeholder = vec![0u8; placeholder_size];
    println!("📦 Created placeholder JUMBF: {} bytes", placeholder.len());

    // Step 3: Prepare updates
    let updates = Updates::new().set_jumbf(placeholder);

    // Step 4: Open output file with write+seek (no read needed with true single-pass!)
    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_path)?;
    println!("📝 Created output file: {}", output_path);

    // Step 5: Write and hash in ONE PASS
    // This is the key optimization - we hash while writing!
    println!("\n⚡ Writing and hashing in single pass...");
    let mut hasher = Sha256::new();
    let chunk_size = 8192; // 8KB chunks

    let structure = asset.write_with_processing(
        &mut output,
        &updates,
        chunk_size,
        &[SegmentKind::Jumbf], // Exclude JUMBF from hash
        &mut |chunk| hasher.update(chunk),
    )?;

    let hash = hasher.finalize();
    println!("✅ Write complete!");
    println!("🔐 Hash: {:02x?}", &hash[..8]); // Show first 8 bytes

    // Step 6: Generate final C2PA manifest
    // In a real workflow, you'd use the c2pa crate to build a proper manifest
    println!("\n📋 Generating final C2PA manifest with hash...");
    let final_manifest = create_mock_c2pa_manifest(&hash);
    println!("📦 Final manifest: {} bytes", final_manifest.len());

    // Step 7: Update JUMBF in-place (file still open!)
    println!("\n✏️  Updating JUMBF in-place...");
    let bytes_written = update_segment_with_structure(
        &mut output,
        &structure,
        SegmentKind::Jumbf,
        final_manifest,
    )?;
    println!("✅ Updated {} bytes", bytes_written);

    // Step 8: Close output (automatic on drop)
    drop(output);
    println!("\n💾 File saved: {}", output_path);

    // Verify the result
    println!("\n=== Verification ===");
    let mut result_asset = Asset::open(output_path)?;
    if let Some(jumbf) = result_asset.jumbf()? {
        println!("✅ JUMBF found: {} bytes", jumbf.len());
        println!("✅ Contains hash: {}", contains_hash(&jumbf, &hash));
    } else {
        println!("❌ No JUMBF found!");
    }

    println!("\n=== Performance Benefits ===");
    println!("Traditional approach:");
    println!("  1. Write file");
    println!("  2. Close and reopen");
    println!("  3. Read entire file to hash");
    println!("  4. Close and reopen");
    println!("  5. Update JUMBF in-place");
    println!("  Total: 2 full writes + 1 full read = 3 passes");
    println!();
    println!("Streaming approach:");
    println!("  1. Write and hash simultaneously");
    println!("  2. Update JUMBF in-place (file still open)");
    println!("  Total: 1 full write + 1 small seek = 1 pass");
    println!();
    println!("Result: ~3x faster for large files! 🚀");

    Ok(())
}

/// Create a mock C2PA manifest containing the hash
///
/// In a real application, you would use the c2pa crate to build a proper
/// JUMBF structure with claim, assertions, and signature.
fn create_mock_c2pa_manifest(hash: &[u8]) -> Vec<u8> {
    let mut manifest = Vec::new();

    // Create a minimal JUMBF structure that will be recognized by the parser
    // JUMBF box: 'jumb' type with description box
    
    // JUMBF super box header
    let jumbf_size: u32 = 200; // Will be padded to 20000
    manifest.extend_from_slice(&jumbf_size.to_be_bytes());
    manifest.extend_from_slice(b"jumb");
    
    // JUMBF Description Box
    let desc_size: u32 = 50;
    manifest.extend_from_slice(&desc_size.to_be_bytes());
    manifest.extend_from_slice(b"jumd");
    manifest.extend_from_slice(&[0x00]); // UUID toggle (0 = no UUID)
    manifest.extend_from_slice(b"c2pa.assertions\0"); // Label (null-terminated)
    
    // Mock C2PA data box
    let data_size: u32 = 100;
    manifest.extend_from_slice(&data_size.to_be_bytes());
    manifest.extend_from_slice(b"json");
    
    // Mock C2PA claim with hash
    manifest.extend_from_slice(b"{");
    manifest.extend_from_slice(b"\"alg\":\"sha256\",");
    manifest.extend_from_slice(b"\"hash\":\"");
    // Simple hex encoding
    for byte in hash {
        manifest.extend_from_slice(format!("{:02x}", byte).as_bytes());
    }
    manifest.extend_from_slice(b"\"");
    manifest.extend_from_slice(b"}");

    manifest
}

/// Check if the JUMBF contains the expected hash
fn contains_hash(jumbf: &[u8], hash: &[u8]) -> bool {
    // Simple hex encoding for comparison
    let mut hash_str = String::new();
    for byte in hash {
        hash_str.push_str(&format!("{:02x}", byte));
    }
    let jumbf_str = String::from_utf8_lossy(jumbf);
    jumbf_str.contains(&hash_str)
}
