//! Example: Parse and inspect a JPEG file
//!
//! This example shows how to parse a JPEG file and inspect its structure
//! without loading the entire file into memory.

use jumbf_io::{JpegHandler, FormatHandler};
use std::fs::File;
use std::env;

fn main() -> jumbf_io::Result<()> {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        eprintln!("Usage: {} <jpeg_file>", args[0]);
        std::process::exit(1);
    }
    
    let filename = &args[1];
    println!("Parsing: {}", filename);
    
    // Open file
    let mut file = File::open(filename)?;
    
    // Parse structure (single pass, no data loading)
    let handler = JpegHandler::new();
    let mut structure = handler.parse(&mut file)?;
    
    println!("\nFile structure:");
    println!("  Format: {:?}", structure.format);
    println!("  Total size: {} bytes", structure.total_size);
    println!("  Segments: {}", structure.segments.len());
    
    // Check for XMP (loads lazily only if present)
    match structure.xmp(&mut file)? {
        Some(xmp) => {
            println!("\n✓ Found XMP metadata ({} bytes)", xmp.len());
            // Print first 100 chars
            let preview = std::str::from_utf8(&xmp[..xmp.len().min(100)])
                .unwrap_or("<binary data>");
            println!("  Preview: {}...", preview);
        }
        None => println!("\n✗ No XMP metadata found"),
    }
    
    // Check for JUMBF (loads and assembles only if present)
    match structure.jumbf(&mut file)? {
        Some(jumbf) => {
            println!("\n✓ Found JUMBF data ({} bytes)", jumbf.len());
        }
        None => println!("\n✗ No JUMBF data found"),
    }
    
    // Show segment breakdown
    println!("\nSegment breakdown:");
    for (i, segment) in structure.segments.iter().enumerate() {
        let location = segment.location();
        let seg_type = match segment {
            jumbf_io::Segment::Header { .. } => "Header".to_string(),
            jumbf_io::Segment::Xmp { .. } => "XMP   ".to_string(),
            jumbf_io::Segment::Jumbf { .. } => "JUMBF ".to_string(),
            jumbf_io::Segment::ImageData { .. } => "Image ".to_string(),
            jumbf_io::Segment::Other { marker, .. } => format!("Other (0x{:02X})", marker),
        };
        println!(
            "  [{:3}] {} at offset {}, size {}",
            i, seg_type, location.offset, location.size
        );
    }
    
    Ok(())
}

