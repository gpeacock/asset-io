//! Test BMFF writing functionality
//!
//! Run with: cargo run --example test_bmff_write --features bmff,xmp

use asset_io::{Asset, Updates};
use std::path::Path;

fn main() -> asset_io::Result<()> {
    println!("🔍 Testing BMFF Writing");

    let input_path = Path::new("../c2pa-rs/sdk/tests/fixtures/video1.mp4");
    if !input_path.exists() {
        println!("❌ Test file not found: {}", input_path.display());
        return Ok(());
    }

    // Test 1: Read original
    println!("\n📖 Reading original file...");
    let mut asset = Asset::open(input_path)?;
    println!("  Container: {:?}", asset.container());
    println!("  Media Type: {:?}", asset.media_type());
    println!("  Size: {} bytes", asset.structure().total_size);

    let original_xmp = asset.xmp()?;
    let original_jumbf = asset.jumbf()?;
    println!(
        "  Original XMP: {} bytes",
        original_xmp.as_ref().map(|x| x.len()).unwrap_or(0)
    );
    println!(
        "  Original JUMBF: {} bytes",
        original_jumbf.as_ref().map(|j| j.len()).unwrap_or(0)
    );

    // Test 2: Write with no changes (keep everything)
    println!("\n✏️  Test 1: Write with no changes");
    let output1 = Path::new("/tmp/test_bmff_keep.mp4");
    let updates = Updates::new();
    asset.write_to(output1, &updates)?;
    println!("  ✓ Written to: {}", output1.display());

    // Verify
    let mut verify_asset = Asset::open(output1)?;
    let verify_xmp = verify_asset.xmp()?;
    let verify_jumbf = verify_asset.jumbf()?;
    println!(
        "  Verified XMP: {} bytes",
        verify_xmp.as_ref().map(|x| x.len()).unwrap_or(0)
    );
    println!(
        "  Verified JUMBF: {} bytes",
        verify_jumbf.as_ref().map(|j| j.len()).unwrap_or(0)
    );

    // Test 3: Remove XMP, keep JUMBF
    println!("\n✏️  Test 2: Remove XMP, keep JUMBF");
    let mut asset2 = Asset::open(input_path)?;
    let output2 = Path::new("/tmp/test_bmff_remove_xmp.mp4");
    let updates = Updates::new().remove_xmp();
    asset2.write_to(output2, &updates)?;
    println!("  ✓ Written to: {}", output2.display());

    let mut verify_asset2 = Asset::open(output2)?;
    let verify_xmp2 = verify_asset2.xmp()?;
    let verify_jumbf2 = verify_asset2.jumbf()?;
    println!(
        "  Verified XMP: {} bytes",
        verify_xmp2.as_ref().map(|x| x.len()).unwrap_or(0)
    );
    println!(
        "  Verified JUMBF: {} bytes",
        verify_jumbf2.as_ref().map(|j| j.len()).unwrap_or(0)
    );

    // Test 4: Set new XMP
    println!("\n✏️  Test 3: Set new XMP");
    let mut asset3 = Asset::open(input_path)?;
    let output3 = Path::new("/tmp/test_bmff_new_xmp.mp4");
    let new_xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:description>Test XMP</dc:description></rdf:Description></rdf:RDF></x:xmpmeta>".to_vec();
    let updates = Updates::new().set_xmp(new_xmp.clone());
    asset3.write_to(output3, &updates)?;
    println!("  ✓ Written to: {}", output3.display());

    let mut verify_asset3 = Asset::open(output3)?;
    let verify_xmp3 = verify_asset3.xmp()?;
    println!(
        "  Verified new XMP: {} bytes",
        verify_xmp3.as_ref().map(|x| x.len()).unwrap_or(0)
    );
    if let Some(xmp) = verify_xmp3 {
        if xmp == new_xmp {
            println!("  ✓ XMP matches!");
        } else {
            println!("  ⚠️  XMP doesn't match");
        }
    }

    println!("\n✅ All BMFF write tests complete!");
    println!("   Test files in /tmp/test_bmff_*.mp4");

    Ok(())
}
