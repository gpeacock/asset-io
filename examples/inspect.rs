//! Example: Parse and inspect a media file
//!
//! This example shows how to parse any supported media file (JPEG, PNG, HEIC, AVIF, MP4, etc.)
//! and inspect its structure without loading the entire file into memory.
//!
//! Run: `cargo run --example inspect --features all-formats,xmp,exif -- <file>`

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

    // Check for EXIF metadata
    match asset.exif_info()? {
        Some(info) => {
            println!("\n✓ Found EXIF metadata");
            println!("  Preview: {}", info);
            // Show individual fields if present
            if let Some(ref make) = info.make {
                println!("  Make: {}", make);
            }
            if let Some(ref model) = info.model {
                println!("  Model: {}", model);
            }
            if let Some(ref dt) = info.date_time_original.or(info.date_time) {
                println!("  DateTime: {}", dt);
            }
            if let Some(ref software) = info.software {
                println!("  Software: {}", software);
            }
            if let Some(orientation) = info.orientation {
                println!("  Orientation: {}", orientation);
            }
        }
        None => println!("\n✗ No EXIF metadata found"),
    }

    // Check for embedded thumbnail (from EXIF)
    match asset.read_embedded_thumbnail()? {
        Some(thumb) => {
            let dims = match (thumb.width, thumb.height) {
                (Some(w), Some(h)) => format!("{}x{}", w, h),
                _ => "unknown dimensions".to_string(),
            };
            println!(
                "\n✓ Found embedded thumbnail ({:?}, {}, {} bytes)",
                thumb.format, dims, thumb.data.len()
            );
        }
        None => println!("\n✗ No embedded thumbnail found"),
    }

    // Show segment breakdown
    println!("\nSegment breakdown:");
    for (i, segment) in asset.structure().segments.iter().enumerate() {
        let location = segment.location();
        let seg_type = if segment.is_header() {
            "Header".to_string()
        } else if segment.is_xmp() {
            if segment.ranges.len() > 1 {
                format!("XMP ({} parts)", segment.ranges.len())
            } else {
                "XMP".to_string()
            }
        } else if segment.is_jumbf() {
            if segment.ranges.len() > 1 {
                format!("JUMBF ({} parts)", segment.ranges.len())
            } else {
                "JUMBF".to_string()
            }
        } else if segment.is_image_data() {
            "ImageData".to_string()
        } else if segment.is_exif() {
            "EXIF".to_string()
        } else {
            segment.path.as_deref().unwrap_or("Other").to_string()
        };
        println!(
            "  [{:3}] {:20} at offset {:8}, size {:8}",
            i, seg_type, location.offset, location.size
        );
    }

    Ok(())
}
