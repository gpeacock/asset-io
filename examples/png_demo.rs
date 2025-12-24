//! PNG format demonstration
//!
//! This example shows how to work with PNG files, including:
//! - Parsing PNG structure
//! - Reading XMP from iTXt chunks
//! - Reading JUMBF/C2PA from caBX chunks
//! - Adding/modifying metadata
//!
//! Usage: cargo run --example png_demo --features png

use asset_io::{Asset, FormatHandler, JumbfUpdate, PngHandler, Updates, XmpUpdate};
use std::fs::File;

fn main() -> asset_io::Result<()> {
    println!("=== PNG Format Demo ===\n");

    // 1. Parse PNG file structure
    println!("1. Parsing PNG file...");
    let mut file = File::open("tests/fixtures/GreenCat.png")?;
    let handler = PngHandler::new();
    let structure = handler.parse(&mut file)?;

    println!("   ✓ Parsed {} segments", structure.segments.len());
    println!("   ✓ Total size: {} bytes\n", structure.total_size);

    // 2. Read metadata using Asset API
    println!("2. Reading metadata...");
    let mut asset = Asset::open("tests/fixtures/GreenCat.png")?;

    if let Some(xmp) = asset.xmp()? {
        println!("   ✓ Found XMP: {} bytes", xmp.len());
        // PNG stores XMP in iTXt chunks with keyword "XML:com.adobe.xmp"
    } else {
        println!("   • No XMP found");
    }

    if let Some(jumbf) = asset.jumbf()? {
        println!("   ✓ Found JUMBF: {} bytes", jumbf.len());
        // PNG stores JUMBF in caBX chunks (C2PA Box)
    } else {
        println!("   • No JUMBF found");
    }

    // 3. Add XMP metadata
    println!("\n3. Adding XMP metadata...");
    let new_xmp = b"<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">
  <rdf:Description rdf:about=\"\">
    <dc:title>Green Cat PNG Demo</dc:title>
    <dc:creator>asset-io example</dc:creator>
  </rdf:Description>
</rdf:RDF>"
        .to_vec();

    asset.write_to(
        "/tmp/png_with_xmp.png",
        &Updates {
            xmp: XmpUpdate::Set(new_xmp),
            ..Default::default()
        },
    )?;

    println!("   ✓ Wrote PNG with XMP to /tmp/png_with_xmp.png");

    // 4. Verify the output
    println!("\n4. Verifying output...");
    let mut output_asset = Asset::open("/tmp/png_with_xmp.png")?;

    if let Some(xmp) = output_asset.xmp()? {
        println!("   ✓ XMP verified: {} bytes", xmp.len());
        let xmp_str = String::from_utf8_lossy(&xmp);
        if xmp_str.contains("Green Cat PNG Demo") {
            println!("   ✓ XMP content verified");
        }
    }

    // 5. Add JUMBF/C2PA data
    println!("\n5. Adding JUMBF/C2PA data...");
    let jumbf_data = b"Example C2PA JUMBF data for PNG".to_vec();

    output_asset.write_to(
        "/tmp/png_with_c2pa.png",
        &Updates {
            xmp: XmpUpdate::Keep, // Keep the XMP we just added
            jumbf: JumbfUpdate::Set(jumbf_data),
            ..Default::default()
        },
    )?;

    println!("   ✓ Wrote PNG with XMP + JUMBF to /tmp/png_with_c2pa.png");

    // 6. Final verification
    println!("\n6. Final verification...");
    let mut final_asset = Asset::open("/tmp/png_with_c2pa.png")?;

    let has_xmp = final_asset.xmp()?.is_some();
    let has_jumbf = final_asset.jumbf()?.is_some();

    println!("   ✓ XMP present: {}", has_xmp);
    println!("   ✓ JUMBF present: {}", has_jumbf);

    if has_xmp && has_jumbf {
        println!("\n✅ PNG demo complete! Both XMP and JUMBF successfully embedded.");
    }

    println!("\n=== PNG Format Notes ===");
    println!("• XMP stored in iTXt chunks (keyword: XML:com.adobe.xmp)");
    println!("• JUMBF/C2PA stored in caBX chunks (C2PA Box)");
    println!("• No multi-segment complexity like JPEG APP11");
    println!("• Each chunk has: length(4) + type(4) + data + CRC(4)");

    Ok(())
}
