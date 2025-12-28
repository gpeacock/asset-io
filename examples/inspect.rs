//! Example: Parse and inspect an image file
//!
//! This example shows how to parse an image file (JPEG, PNG) and inspect its structure
//! without loading the entire file into memory.

use asset_io::Asset;
use std::env;

fn main() -> asset_io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <image_file>", args[0]);
        std::process::exit(1);
    }

    let filename = &args[1];
    println!("Parsing: {}", filename);

    // Auto-detect format and parse
    let mut asset = Asset::open(filename)?;

    println!("\nFile structure:");
    println!("  Container: {:?}", asset.container());
    println!("  Media type: {:?}", asset.media_type());
    println!("  Total size: {} bytes", asset.structure().total_size);
    println!("  Segments: {}", asset.structure().segments.len());

    // Check for XMP (loads lazily only if present)
    match asset.xmp()? {
        Some(xmp) => {
            println!("\n✓ Found XMP metadata ({} bytes)", xmp.len());
            // Print first 100 chars
            let preview =
                std::str::from_utf8(&xmp[..xmp.len().min(100)]).unwrap_or("<binary data>");
            println!("  Preview: {}...", preview);
        }
        None => println!("\n✗ No XMP metadata found"),
    }

    // Check for JUMBF (loads and assembles only if present)
    match asset.jumbf()? {
        Some(jumbf) => {
            println!("\n✓ Found JUMBF data ({} bytes)", jumbf.len());
        }
        None => println!("\n✗ No JUMBF data found"),
    }

    // Show segment breakdown
    println!("\nSegment breakdown:");
    for (i, segment) in asset.structure().segments.iter().enumerate() {
        let location = segment.location();
        let seg_type = match segment {
            asset_io::Segment::Header { .. } => "Header".to_string(),
            asset_io::Segment::Xmp { segments, .. } => {
                if segments.len() > 1 {
                    format!("XMP ({} parts)", segments.len())
                } else {
                    "XMP".to_string()
                }
            }
            asset_io::Segment::Jumbf { segments, .. } => {
                if segments.len() > 1 {
                    format!("JUMBF ({} parts)", segments.len())
                } else {
                    "JUMBF".to_string()
                }
            }
            asset_io::Segment::ImageData { .. } => "ImageData".to_string(),
            asset_io::Segment::Exif { .. } => {
                #[cfg(feature = "exif")]
                {
                    if let asset_io::Segment::Exif { thumbnail, .. } = segment {
                        if thumbnail.is_some() {
                            "EXIF (with thumbnail)".to_string()
                        } else {
                            "EXIF".to_string()
                        }
                    } else {
                        "EXIF".to_string()
                    }
                }
                #[cfg(not(feature = "exif"))]
                {
                    "EXIF".to_string()
                }
            }
            asset_io::Segment::Other { label, .. } => label.to_string(),
        };
        println!(
            "  [{:3}] {:20} at offset {:8}, size {:8}",
            i, seg_type, location.offset, location.size
        );
    }

    Ok(())
}
