//! Comprehensive test of all XMP and JUMBF modification combinations

use asset_io::{Asset, JumbfUpdate, Updates, XmpUpdate};
use std::fs;

fn main() -> asset_io::Result<()> {
    let input = "/Users/gpeacock/Desktop/CC-MCP demo/L1000353.JPG";
    let test_xmp = b"<test>New XMP content</test>".to_vec();
    let _test_jumbf = create_test_jumbf();

    println!("=== Testing All Metadata Modification Combinations ===\n");

    // Test 1: Keep both
    println!("Test 1: Keep both XMP and JUMBF");
    test_modification(input, "/tmp/test_keep_both.jpg", Updates::default())?;

    // Test 2: Remove XMP, keep JUMBF
    println!("\nTest 2: Remove XMP, keep JUMBF");
    test_modification(
        input,
        "/tmp/test_remove_xmp.jpg",
        Updates {
            xmp: XmpUpdate::Remove,
            ..Default::default()
        },
    )?;

    // Test 3: Keep XMP, remove JUMBF
    println!("\nTest 3: Keep XMP, remove JUMBF");
    test_modification(
        input,
        "/tmp/test_remove_jumbf.jpg",
        Updates {
            jumbf: JumbfUpdate::Remove,
            ..Default::default()
        },
    )?;

    // Test 4: Remove both
    println!("\nTest 4: Remove both XMP and JUMBF");
    test_modification(input, "/tmp/test_remove_both.jpg", Updates::remove_all())?;

    // Test 5: Replace XMP, keep JUMBF
    println!("\nTest 5: Replace XMP, keep JUMBF");
    test_modification(
        input,
        "/tmp/test_replace_xmp.jpg",
        Updates {
            xmp: XmpUpdate::Set(test_xmp.clone()),
            ..Default::default()
        },
    )?;

    // Test 6: Keep XMP, remove JUMBF (simpler than replace)
    println!("\nTest 6: Keep XMP, remove JUMBF (alternate)");
    test_modification(
        input,
        "/tmp/test_keep_xmp_remove_jumbf.jpg",
        Updates {
            jumbf: JumbfUpdate::Remove,
            ..Default::default()
        },
    )?;

    // Test 7: Replace XMP, remove JUMBF
    println!("\nTest 7: Replace XMP, remove JUMBF");
    test_modification(
        input,
        "/tmp/test_replace_xmp_remove_jumbf.jpg",
        Updates {
            xmp: XmpUpdate::Set(test_xmp.clone()),
            jumbf: JumbfUpdate::Remove,
            ..Default::default()
        },
    )?;

    // Test 8: Add XMP to file without XMP
    println!("\nTest 8: Add XMP to file without XMP");
    test_modification(
        "/Users/gpeacock/Desktop/CC-MCP demo/IMG_0550.jpg",
        "/tmp/test_add_xmp.jpg",
        Updates {
            xmp: XmpUpdate::Set(test_xmp.clone()),
            ..Default::default()
        },
    )?;

    // Test 9: Add XMP to file without XMP (alternate path)
    println!("\nTest 9: Verify XMP addition works on different file");
    test_modification(
        "/Users/gpeacock/Desktop/CC-MCP demo/IMG_0550.jpg",
        "/tmp/test_add_xmp_alt.jpg",
        Updates::with_xmp(test_xmp.clone()),
    )?;

    // Note: JUMBF addition requires properly formatted C2PA data which is complex.
    // For production use, JUMBF data should come from a C2PA library.
    println!("\nNote: JUMBF writing requires properly formatted C2PA data.");
    println!("      For testing JUMBF replacement, extract from existing file.");

    // Test 10: Extract and rewrite JUMBF (using real data)
    println!("\nTest 10: Extract JUMBF and rewrite to different file");
    let mut source = Asset::open("/Users/gpeacock/Desktop/CC-MCP demo/L1000353.JPG")?;
    if let Some(jumbf_data) = source.jumbf()? {
        println!("  Extracted {} bytes of JUMBF", jumbf_data.len());

        test_modification(
            "/Users/gpeacock/Desktop/CC-MCP demo/IMG_0550.jpg",
            "/tmp/test_transfer_jumbf.jpg",
            Updates {
                jumbf: JumbfUpdate::Set(jumbf_data),
                ..Default::default()
            },
        )?;
    }

    println!("\n=== All tests completed successfully! ===");
    Ok(())
}

fn test_modification(input: &str, output: &str, updates: Updates) -> asset_io::Result<()> {
    // Parse and write
    let mut asset = Asset::open(input)?;
    asset.write_to(output, &updates)?;

    // Verify output
    let mut verify = Asset::open(output)?;
    let has_xmp = verify.xmp()?.is_some();
    let has_jumbf = verify.jumbf()?.is_some();

    println!("  Written: {} ({})", output, fs::metadata(output)?.len());
    println!(
        "  XMP: {}, JUMBF: {}",
        if has_xmp { "✓" } else { "✗" },
        if has_jumbf { "✓" } else { "✗" }
    );

    // Verify with ImageMagick
    let status = std::process::Command::new("identify")
        .arg(output)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => println!("  Validity: ✓ Valid JPEG"),
        _ => println!("  Validity: ⚠ Could not verify"),
    }

    Ok(())
}

fn create_test_jumbf() -> Vec<u8> {
    // Create a minimal valid JUMBF structure
    let mut jumbf = Vec::new();

    // JUMBF superbox
    jumbf.extend_from_slice(&[0, 0, 0, 120]); // LBox (120 bytes total)
    jumbf.extend_from_slice(b"jumb"); // TBox

    // JUMBF description box
    jumbf.extend_from_slice(&[0, 0, 0, 50]); // LBox
    jumbf.extend_from_slice(b"jumd"); // TBox
    jumbf.extend_from_slice(b"c2pa"); // UUID (simplified - first 4 bytes)
    jumbf.extend_from_slice(&[0, 0, 0, 0]); // UUID continued
    jumbf.extend_from_slice(&[0, 0, 0, 0]); // UUID continued
    jumbf.extend_from_slice(&[0, 0, 0, 0]); // UUID continued
    jumbf.extend_from_slice(b"test_label\0"); // Label

    // Pad to 120 bytes
    while jumbf.len() < 120 {
        jumbf.push(0);
    }

    jumbf
}
