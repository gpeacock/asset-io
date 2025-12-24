//! Example: Format-agnostic asset handling
//!
//! This example shows how to work with media files without knowing
//! their format upfront. The library automatically detects the format.

use asset_io::{Asset, JumbfUpdate, Updates, XmpUpdate};
use std::env;

fn main() -> asset_io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <media_file> [output_file]", args[0]);
        eprintln!("\nSupported formats: JPEG, PNG (coming soon), MP4 (coming soon)");
        eprintln!("\nExamples:");
        eprintln!("  {} image.jpg", args[0]);
        eprintln!("  {} photo.jpg modified.jpg", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    println!("Opening: {}", input_path);

    // Open file - format is auto-detected
    let mut asset = Asset::open(input_path)?;

    println!("\nFile information:");
    println!("  Format: {:?}", asset.format());
    println!("  Total size: {} bytes", asset.structure().total_size);
    println!("  Segments: {}", asset.structure().segments.len());

    // Check for metadata
    let _has_xmp = if let Some(xmp) = asset.xmp()? {
        println!("\n✓ Found XMP metadata: {} bytes", xmp.len());
        // Preview first 100 chars
        let preview = std::str::from_utf8(&xmp[..xmp.len().min(100)]).unwrap_or("<binary data>");
        println!("  Preview: {}...", preview);
        true
    } else {
        println!("\n✗ No XMP metadata found");
        false
    };

    let has_jumbf = if let Some(jumbf) = asset.jumbf()? {
        println!("\n✓ Found JUMBF data: {} bytes", jumbf.len());
        true
    } else {
        println!("\n✗ No JUMBF data found");
        false
    };

    // If output path provided, demonstrate modification
    if args.len() >= 3 {
        let output_path = &args[2];
        println!("\n=== Modifying and writing to: {} ===", output_path);

        // Example: Add or replace XMP
        let new_xmp = format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description>Modified by jumbf-io example</rdf:Description>\
             </rdf:RDF>"
        );

        let updates = Updates {
            xmp: XmpUpdate::Set(new_xmp.into_bytes()),
            jumbf: JumbfUpdate::Keep,
            #[cfg(feature = "thumbnails")]
            thumbnail: None,
        };

        asset.write_to(output_path, &updates)?;
        println!("✓ File written successfully!");

        // Verify the output
        println!("\nVerifying output...");
        let mut verify = Asset::open(output_path)?;

        if let Some(xmp) = verify.xmp()? {
            println!("✓ Output has XMP: {} bytes", xmp.len());
        }

        if let Some(jumbf) = verify.jumbf()? {
            println!("✓ Output has JUMBF: {} bytes", jumbf.len());
        } else if has_jumbf {
            println!("⚠ JUMBF was removed (this is expected based on updates)");
        }
    } else {
        println!("\n💡 Tip: Provide an output filename to create a modified copy");
    }

    Ok(())
}
