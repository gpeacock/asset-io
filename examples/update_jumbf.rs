//! Example: Update JUMBF manifest in-place (C2PA placeholder workflow)
//!
//! This demonstrates the C2PA workflow where a placeholder manifest is written first,
//! then replaced with the final signed manifest using in-place updates.
//!
//! Usage:
//!   cargo run --example update_jumbf --features jpeg,xmp <input.jpg> <output.jpg>

use asset_io::{Asset, Updates};
use std::env;
use std::fs::OpenOptions;

fn main() -> asset_io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        eprintln!("Usage: {} <input_file> <output_file>", args[0]);
        eprintln!("\nExample:");
        eprintln!("  {} photo.jpg photo_with_manifest.jpg", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    println!("=== JUMBF In-Place Update Example ===\n");
    println!("Step 1: Write placeholder manifest");

    // Step 1: Create a placeholder manifest (simulating c2pa placeholder)
    // In real C2PA workflow, this would be builder.data_hashed_placeholder()
    let placeholder_size = 20000; // Reserve 20KB for the final manifest
    let placeholder_manifest = create_placeholder_jumbf(placeholder_size);
    println!("  Placeholder size: {} bytes", placeholder_manifest.len());

    // Write file with placeholder
    let mut asset = Asset::open(input_path)?;
    asset.write_to(
        output_path,
        &Updates::new().set_jumbf(placeholder_manifest.clone()),
    )?;
    println!("  ✓ Written to: {}", output_path);

    // Step 2: Simulate signing (in real workflow, this would involve hashing and signing)
    println!("\nStep 2: Simulate signing process");
    println!("  (In real C2PA: hash image, sign with private key)");

    // Create a "signed" manifest (smaller than placeholder)
    let signed_manifest = create_signed_jumbf(15000); // Actual manifest is smaller
    println!("  Signed manifest size: {} bytes", signed_manifest.len());

    // Step 3: Update manifest in-place
    println!("\nStep 3: Update manifest in-place");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(output_path)?;

    let mut asset = Asset::from_source(file)?;

    // Check capacity
    let capacity = asset.jumbf_capacity().ok_or_else(|| {
        asset_io::Error::InvalidFormat("No JUMBF found in output file".to_string())
    })?;
    println!("  JUMBF capacity: {} bytes", capacity);

    // Validate size
    if signed_manifest.len() as u64 > capacity {
        return Err(asset_io::Error::InvalidFormat(format!(
            "Signed manifest ({} bytes) exceeds placeholder capacity ({} bytes)",
            signed_manifest.len(),
            capacity
        )));
    }

    // Update in-place
    let bytes_written = asset.update_jumbf_in_place(signed_manifest)?;
    println!("  ✓ Updated {} bytes in-place", bytes_written);

    // Step 4: Verify
    println!("\nStep 4: Verify update");
    let mut verify_asset = Asset::open(output_path)?;
    let jumbf = verify_asset.jumbf()?.expect("JUMBF should exist");
    println!("  ✓ JUMBF size: {} bytes", jumbf.len());
    println!(
        "  ✓ First 4 bytes: {:02x} {:02x} {:02x} {:02x}",
        jumbf[0], jumbf[1], jumbf[2], jumbf[3]
    );

    println!("\n✓ Success! JUMBF updated in-place without rewriting the entire file.");
    println!("\nThis workflow is much faster than rewriting because:");
    println!("  • Only the JUMBF segment is overwritten");
    println!("  • Image data remains untouched");
    println!("  • File structure is preserved");

    Ok(())
}

/// Create a placeholder JUMBF structure (simulates c2pa placeholder)
fn create_placeholder_jumbf(size: usize) -> Vec<u8> {
    let mut jumbf = Vec::new();

    // JUMBF superbox header
    jumbf.extend_from_slice(&(size as u32).to_be_bytes()); // Size
    jumbf.extend_from_slice(b"jumb"); // Type

    // JUMBF description box
    jumbf.extend_from_slice(&[0, 0, 0, 50]); // Size
    jumbf.extend_from_slice(b"jumd"); // Type
    jumbf.extend_from_slice(b"c2pa"); // UUID prefix (simplified)
    jumbf.extend_from_slice(&[0; 12]); // Rest of UUID
    jumbf.extend_from_slice(b"placeholder\0"); // Label

    // Pad to requested size
    while jumbf.len() < size {
        jumbf.push(0);
    }

    jumbf
}

/// Create a "signed" JUMBF structure (simulates c2pa signed manifest)
fn create_signed_jumbf(size: usize) -> Vec<u8> {
    let mut jumbf = Vec::new();

    // JUMBF superbox header
    jumbf.extend_from_slice(&(size as u32).to_be_bytes()); // Size
    jumbf.extend_from_slice(b"jumb"); // Type

    // JUMBF description box
    jumbf.extend_from_slice(&[0, 0, 0, 50]); // Size
    jumbf.extend_from_slice(b"jumd"); // Type
    jumbf.extend_from_slice(b"c2pa"); // UUID prefix (simplified)
    jumbf.extend_from_slice(&[0; 12]); // Rest of UUID
    jumbf.extend_from_slice(b"signed_manifest\0"); // Label

    // Add some "signature" data
    jumbf.extend_from_slice(&[0; 100]); // Simulated signature

    // Pad to requested size
    while jumbf.len() < size {
        jumbf.push(0);
    }

    jumbf
}
