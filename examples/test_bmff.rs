//! Test BMFF support with sample files
//!
//! Run with: cargo run --example test_bmff --features bmff

use std::path::Path;

fn test_file(path: &str) -> asset_io::Result<()> {
    let path = Path::new(path);

    if !path.exists() {
        println!("⚠️  File not found: {}", path.display());
        return Ok(());
    }

    println!("\n📁 Testing: {}", path.display());

    // Test with Asset API (auto-detection)
    let mut asset = asset_io::Asset::open(path)?;
    println!("  ContainerKind: {:?}", asset.container());
    println!("  Media Type: {:?}", asset.media_type());
    println!("  Size: {} bytes", asset.structure().total_size);
    println!("  Segments: {}", asset.structure().segments().len());

    // Check for XMP
    if let Some(xmp) = asset.xmp()? {
        println!("  ✓ XMP found: {} bytes", xmp.len());
    } else {
        println!("  ✗ No XMP");
    }

    // Check for JUMBF/C2PA
    if let Some(jumbf) = asset.jumbf()? {
        println!("  ✓ JUMBF found: {} bytes", jumbf.len());
    } else {
        println!("  ✗ No JUMBF");
    }

    Ok(())
}

fn main() -> asset_io::Result<()> {
    println!("🔍 Testing BMFF Support");

    // Test files from asset-io
    test_file("tests/fixtures/sample1.heic")?;
    test_file("tests/fixtures/sample1.avif")?;

    // Test files from c2pa-rs
    test_file("../c2pa-rs/sdk/tests/fixtures/sample1.heic")?;
    test_file("../c2pa-rs/sdk/tests/fixtures/sample1.avif")?;
    test_file("../c2pa-rs/sdk/tests/fixtures/video1.mp4")?;

    println!("\n✅ BMFF tests complete!");
    Ok(())
}
