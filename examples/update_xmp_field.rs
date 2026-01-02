//! Example: Update a single XMP field in-place without rewriting the entire file
//!
//! This demonstrates efficient XMP updates by overwriting only the XMP segment,
//! which is much faster than rewriting the entire image file.
//!
//! Usage:
//!   cargo run --example update_xmp_field --features jpeg,xmp <input.jpg> <key> <value>
//!
//! Example:
//!   cargo run --example update_xmp_field --features jpeg,xmp photo.jpg dc:title "My Photo"

use asset_io::{Asset, MiniXmp};
use std::env;
use std::fs::OpenOptions;

fn main() -> asset_io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 4 {
        eprintln!("Usage: {} <input_file> <xmp_key> <value>", args[0]);
        eprintln!("\nExample:");
        eprintln!("  {} photo.jpg dc:title \"My Photo\"", args[0]);
        eprintln!("  {} photo.jpg dc:creator \"John Doe\"", args[0]);
        eprintln!(
            "  {} photo.jpg dc:description \"A beautiful sunset\"",
            args[0]
        );
        std::process::exit(1);
    }

    let input_path = &args[1];
    let xmp_key = &args[2];
    let xmp_value = &args[3];

    println!("Opening: {}", input_path);

    // Open file with read+write access
    let file = OpenOptions::new().read(true).write(true).open(input_path)?;

    let mut asset = Asset::from_source(file)?;

    // Check if file has XMP
    let xmp_capacity = asset.xmp_capacity().ok_or_else(|| {
        asset_io::Error::InvalidFormat("No XMP metadata found in file".to_string())
    })?;

    println!("Found XMP with capacity: {} bytes", xmp_capacity);

    // Extract and modify XMP
    let xmp = asset.xmp()?.expect("No XMP found");
    println!("Current XMP size: {} bytes", xmp.len());

    // Convert to string for processing
    let xmp_str = String::from_utf8_lossy(&xmp).into_owned();
    let mini_xmp = MiniXmp::new(&xmp_str);

    // Show current value if it exists
    if let Some(current_value) = mini_xmp.get(xmp_key) {
        println!("Current value of '{}': {}", xmp_key, current_value);
    } else {
        println!("Key '{}' not currently set", xmp_key);
    }

    // Update the field
    let updated_mini_xmp = mini_xmp.set(xmp_key, xmp_value)?;
    let updated_xmp = updated_mini_xmp.into_inner().into_bytes();
    println!("New XMP size: {} bytes", updated_xmp.len());

    // Check if updated XMP fits in existing space
    if updated_xmp.len() as u64 > xmp_capacity {
        eprintln!(
            "\nError: Updated XMP ({} bytes) is larger than available space ({} bytes)",
            updated_xmp.len(),
            xmp_capacity
        );
        eprintln!("The file would need to be fully rewritten to accommodate the larger XMP.");
        eprintln!("Consider using shorter values or Asset::write() for a full rewrite.");
        return Err(asset_io::Error::InvalidFormat(
            "XMP too large for in-place update".to_string(),
        ));
    }

    // Update in-place
    println!("\nUpdating XMP in-place...");
    let bytes_written = asset.update_xmp_in_place(updated_xmp)?;
    println!("✓ Updated {} bytes", bytes_written);

    // Verify the update
    println!("\nVerifying update...");
    let verify_file = OpenOptions::new().read(true).open(input_path)?;
    let mut verify_asset = Asset::from_source(verify_file)?;
    let verify_xmp = verify_asset.xmp()?.expect("XMP should exist");
    let verify_xmp_str = String::from_utf8_lossy(&verify_xmp).into_owned();
    let verify_mini_xmp = MiniXmp::new(&verify_xmp_str);

    if let Some(new_value) = verify_mini_xmp.get(xmp_key) {
        println!("✓ Verified: '{}' = '{}'", xmp_key, new_value);
        if new_value == *xmp_value {
            println!("✓ Success! XMP field updated in-place.");
        } else {
            eprintln!("✗ Warning: Value mismatch!");
        }
    } else {
        eprintln!("✗ Warning: Could not verify update");
    }

    Ok(())
}
