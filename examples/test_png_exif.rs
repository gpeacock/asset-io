//! Test PNG eXIf chunk support

#[cfg(feature = "png")]
use std::io::Cursor;

// Create a minimal PNG with an eXIf chunk
#[cfg(feature = "png")]
fn create_png_with_exif() -> Vec<u8> {
    let mut data = Vec::new();

    // PNG signature
    data.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    // IHDR chunk (minimal 1x1 image)
    data.extend_from_slice(&[0, 0, 0, 13]); // length
    data.extend_from_slice(b"IHDR");
    data.extend_from_slice(&[
        0, 0, 0, 1, // width: 1
        0, 0, 0, 1, // height: 1
        8, // bit depth
        2, // color type: RGB
        0, 0, 0, // compression, filter, interlace
    ]);
    data.extend_from_slice(&[0x90, 0x77, 0x53, 0xDE]); // CRC

    // eXIf chunk (minimal TIFF header with big-endian marker)
    let exif_data = b"MM\0\x002\0\0\0\x08"; // Big-endian TIFF header
    data.extend_from_slice(&(exif_data.len() as u32).to_be_bytes());
    data.extend_from_slice(b"eXIf");
    data.extend_from_slice(exif_data);
    // CRC for "eXIf" + data
    data.extend_from_slice(&[0x6D, 0xC7, 0xA8, 0x5F]); // pre-calculated CRC

    // IDAT chunk (minimal compressed data)
    data.extend_from_slice(&[0, 0, 0, 10]); // length
    data.extend_from_slice(b"IDAT");
    data.extend_from_slice(&[0x78, 0x9C, 0x63, 0, 1, 0, 0, 5, 0, 1]); // minimal deflate
    data.extend_from_slice(&[0xD9, 0x66, 0x38, 0x0C]); // CRC

    // IEND chunk
    data.extend_from_slice(&[0, 0, 0, 0]); // length
    data.extend_from_slice(b"IEND");
    data.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]); // CRC

    data
}

fn main() {
    println!("=== Testing PNG eXIf Chunk Support ===\n");

    #[cfg(not(feature = "png"))]
    {
        println!("PNG feature not enabled");
        return;
    }

    #[cfg(feature = "png")]
    {
        use asset_io::{Asset, MediaType};

        let png_data = create_png_with_exif();
        println!(
            "Created minimal PNG with eXIf chunk: {} bytes",
            png_data.len()
        );

        let cursor = Cursor::new(png_data.clone());
        let mut asset =
            Asset::from_source_with_format(cursor, MediaType::Png).expect("Failed to parse PNG");

        println!("Parsed {} segments", asset.structure().segments().len());

        // Check for EXIF segment
        let exif_segment = asset
            .structure()
            .segments()
            .iter()
            .find(|s| s.is_exif());

        if let Some(exif_segment) = exif_segment {
            let location = exif_segment.location();
            println!("✓ EXIF segment detected!");
            println!("  Offset: {}", location.offset);
            println!("  Size: {} bytes", location.size);

            // Verify the data is correct
            let exif_start = location.offset as usize;
            let exif_end = exif_start + location.size as usize;
            let exif_data = &png_data[exif_start..exif_end];

            if exif_data.starts_with(b"MM") {
                println!("  ✓ TIFF header (big-endian) detected");
            } else if exif_data.starts_with(b"II") {
                println!("  ✓ TIFF header (little-endian) detected");
            }

            println!("\n✓ PNG eXIf support working correctly!");
        } else {
            println!("✗ Failed to detect EXIF segment");
            std::process::exit(1);
        }

        // Test round-trip through writer
        println!("\nTesting round-trip write...");
        let mut output = Vec::new();
        asset
            .write(&mut output, &asset_io::Updates::default())
            .expect("Failed to write PNG");

        println!("Written {} bytes", output.len());

        // Parse the written PNG
        let output_cursor = Cursor::new(output);
        let output_asset = Asset::from_source_with_format(output_cursor, MediaType::Png)
            .expect("Failed to parse written PNG");

        let has_exif_output = output_asset
            .structure()
            .segments()
            .iter()
            .any(|s| s.is_exif());

        if has_exif_output {
            println!("✓ EXIF preserved after write!");
        } else {
            println!("✗ EXIF lost during write");
            std::process::exit(1);
        }

        println!("\n=== All PNG eXIf Tests Passed ===");
    }
}
