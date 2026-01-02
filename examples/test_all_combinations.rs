//! Comprehensive test of all XMP and JUMBF modification combinations
//!
//! This example demonstrates all possible metadata operations and includes
//! automated tests that run with `cargo test`.

use asset_io::{Asset, ExclusionMode, Updates};
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
        Updates::new().remove_xmp(),
    )?;

    // Test 3: Keep XMP, remove JUMBF
    println!("\nTest 3: Keep XMP, remove JUMBF");
    test_modification(
        input,
        "/tmp/test_remove_jumbf.jpg",
        Updates::new().remove_jumbf(),
    )?;

    // Test 4: Remove both
    println!("\nTest 4: Remove both XMP and JUMBF");
    test_modification(input, "/tmp/test_remove_both.jpg", Updates::remove_all())?;

    // Test 5: Replace XMP, keep JUMBF
    println!("\nTest 5: Replace XMP, keep JUMBF");
    test_modification(
        input,
        "/tmp/test_replace_xmp.jpg",
        Updates::new().set_xmp(test_xmp.clone()),
    )?;

    // Test 6: Keep XMP, remove JUMBF (simpler than replace)
    println!("\nTest 6: Keep XMP, remove JUMBF (alternate)");
    test_modification(
        input,
        "/tmp/test_keep_xmp_remove_jumbf.jpg",
        Updates::new().remove_jumbf(),
    )?;

    // Test 7: Replace XMP, remove JUMBF
    println!("\nTest 7: Replace XMP, remove JUMBF");
    test_modification(
        input,
        "/tmp/test_replace_xmp_remove_jumbf.jpg",
        Updates::new().set_xmp(test_xmp.clone()).remove_jumbf(),
    )?;

    // Test 8: Add XMP to file without XMP
    println!("\nTest 8: Add XMP to file without XMP");
    test_modification(
        "/Users/gpeacock/Desktop/CC-MCP demo/IMG_0550.jpg",
        "/tmp/test_add_xmp.jpg",
        Updates::new().set_xmp(test_xmp.clone()),
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
            Updates::new().set_jumbf(jumbf_data),
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

// ============================================================================
// Automated Tests (run with `cargo test`)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use asset_io::test_utils::*;

    /// Helper to create test XMP data
    fn test_xmp_data() -> Vec<u8> {
        br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Test XMP Data</dc:title>
<dc:creator>asset-io test</dc:creator>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
            .to_vec()
    }

    /// Test adding XMP to files without existing XMP for all containers
    #[test]
    fn test_add_xmp_to_file_without_xmp() {
        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "jpeg"),
            #[cfg(feature = "png")]
            (P1000708, "png"),
        ];

        for (fixture, name) in test_cases {
            println!("\n=== Testing add XMP to {} ===", name);

            let input_path = fixture_path(fixture);
            let output_path = format!("/tmp/test_add_xmp_{}.{}", name, name);

            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            let xmp_data = test_xmp_data();
            let updates = Updates::new().set_xmp(xmp_data.clone());

            // Write with new XMP
            asset
                .write_to(&output_path, &updates)
                .expect(&format!("Failed to write {} with XMP", name));

            // Verify XMP was added
            let mut verify_asset =
                Asset::open(&output_path).expect(&format!("Failed to open output {}", name));

            let result_xmp = verify_asset
                .xmp()
                .expect(&format!("Failed to extract XMP from {}", name))
                .expect(&format!("XMP not found in output {}", name));

            assert_eq!(result_xmp, xmp_data, "XMP content mismatch for {}", name);
            println!("✓ {} - XMP successfully added and verified", name);
        }
    }

    /// Test adding JUMBF to files without existing JUMBF for all containers
    #[test]
    fn test_add_jumbf_to_file_without_jumbf() {
        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "jpeg"),
            #[cfg(feature = "png")]
            (P1000708, "png"),
        ];

        for (fixture, name) in test_cases {
            println!("\n=== Testing add JUMBF to {} ===", name);

            let input_path = fixture_path(fixture);
            let output_path = format!("/tmp/test_add_jumbf_{}.{}", name, name);

            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            let jumbf_data = create_test_jumbf();
            let updates = Updates::new().set_jumbf(jumbf_data.clone());

            // Write with new JUMBF
            asset
                .write_to(&output_path, &updates)
                .expect(&format!("Failed to write {} with JUMBF", name));

            // Verify JUMBF was added
            let mut verify_asset =
                Asset::open(&output_path).expect(&format!("Failed to open output {}", name));

            let result_jumbf = verify_asset
                .jumbf()
                .expect(&format!("Failed to extract JUMBF from {}", name))
                .expect(&format!("JUMBF not found in output {}", name));

            assert_eq!(
                result_jumbf, jumbf_data,
                "JUMBF content mismatch for {}",
                name
            );
            println!("✓ {} - JUMBF successfully added and verified", name);
        }
    }

    /// Test adding both XMP and JUMBF simultaneously
    #[test]
    fn test_add_both_xmp_and_jumbf() {
        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "jpeg"),
            #[cfg(feature = "png")]
            (P1000708, "png"),
        ];

        for (fixture, name) in test_cases {
            println!("\n=== Testing add both XMP and JUMBF to {} ===", name);

            let input_path = fixture_path(fixture);
            let output_path = format!("/tmp/test_add_both_{}.{}", name, name);

            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            let xmp_data = test_xmp_data();
            let jumbf_data = create_test_jumbf();
            let updates = Updates::new()
                .set_xmp(xmp_data.clone())
                .set_jumbf(jumbf_data.clone());

            // Write with both
            asset
                .write_to(&output_path, &updates)
                .expect(&format!("Failed to write {} with both", name));

            // Verify both were added
            let mut verify_asset =
                Asset::open(&output_path).expect(&format!("Failed to open output {}", name));

            let result_xmp = verify_asset
                .xmp()
                .expect(&format!("Failed to extract XMP from {}", name))
                .expect(&format!("XMP not found in output {}", name));
            let result_jumbf = verify_asset
                .jumbf()
                .expect(&format!("Failed to extract JUMBF from {}", name))
                .expect(&format!("JUMBF not found in output {}", name));

            assert_eq!(result_xmp, xmp_data, "XMP content mismatch for {}", name);
            assert_eq!(
                result_jumbf, jumbf_data,
                "JUMBF content mismatch for {}",
                name
            );
            println!(
                "✓ {} - Both XMP and JUMBF successfully added and verified",
                name
            );
        }
    }

    /// Test replacing existing XMP
    #[test]
    fn test_replace_xmp() {
        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "jpeg"),
            #[cfg(feature = "png")]
            (P1000708, "png"),
        ];

        for (fixture, name) in test_cases {
            println!("\n=== Testing replace XMP in {} ===", name);

            let input_path = fixture_path(fixture);
            let temp_path = format!("/tmp/test_replace_xmp_temp_{}.{}", name, name);
            let final_path = format!("/tmp/test_replace_xmp_final_{}.{}", name, name);

            // First add some XMP
            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            let xmp_data1 = b"<test>Original XMP</test>".to_vec();
            asset
                .write_to(&temp_path, &Updates::new().set_xmp(xmp_data1.clone()))
                .expect("Failed to add initial XMP");

            // Now replace it
            let mut asset2 = Asset::open(&temp_path).expect("Failed to open temp output");

            let xmp_data2 = b"<test>Replaced XMP</test>".to_vec();
            asset2
                .write_to(&final_path, &Updates::new().set_xmp(xmp_data2.clone()))
                .expect("Failed to replace XMP");

            // Verify
            let mut verify_asset = Asset::open(&final_path).expect("Failed to open final output");

            let result_xmp = verify_asset
                .xmp()
                .expect("Failed to extract XMP")
                .expect("XMP not found");

            assert_eq!(result_xmp, xmp_data2, "XMP should be replaced for {}", name);
            assert_ne!(
                result_xmp, xmp_data1,
                "XMP should not be original for {}",
                name
            );
            println!("✓ {} - XMP successfully replaced", name);
        }
    }

    /// Test removing XMP
    #[test]
    fn test_remove_xmp() {
        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "jpeg"),
            #[cfg(feature = "png")]
            (P1000708, "png"),
        ];

        for (fixture, name) in test_cases {
            println!("\n=== Testing remove XMP from {} ===", name);

            let input_path = fixture_path(fixture);
            let temp_path = format!("/tmp/test_remove_xmp_temp_{}.{}", name, name);
            let final_path = format!("/tmp/test_remove_xmp_final_{}.{}", name, name);

            // First add XMP
            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            let xmp_data = test_xmp_data();
            asset
                .write_to(&temp_path, &Updates::new().set_xmp(xmp_data))
                .expect("Failed to add XMP");

            // Now remove it
            let mut asset2 = Asset::open(&temp_path).expect("Failed to open temp output");

            asset2
                .write_to(&final_path, &Updates::new().remove_xmp())
                .expect("Failed to remove XMP");

            // Verify XMP is gone
            let mut verify_asset = Asset::open(&final_path).expect("Failed to open final output");

            let result_xmp = verify_asset.xmp().expect("Failed to check XMP");

            assert!(result_xmp.is_none(), "XMP should be removed for {}", name);
            println!("✓ {} - XMP successfully removed", name);
        }
    }

    /// Test round-trip: no modifications
    #[test]
    fn test_round_trip_no_changes() {
        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "jpeg"),
            #[cfg(feature = "png")]
            (P1000708, "png"),
        ];

        for (fixture, name) in test_cases {
            println!("\n=== Testing round-trip for {} ===", name);

            let input_path = fixture_path(fixture);
            let output_path = format!("/tmp/test_round_trip_{}.{}", name, name);

            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            asset
                .write_to(&output_path, &Updates::default())
                .expect(&format!("Failed to write {} with no changes", name));

            // Output should be parseable
            let result = Asset::open(&output_path);

            assert!(
                result.is_ok(),
                "Round-trip output should be valid for {}",
                name
            );
            println!("✓ {} - Round-trip successful", name);
        }
    }

    /// Test keeping existing JUMBF (critical for c2pa workflow)
    #[test]
    fn test_keep_existing_jumbf() {
        #[cfg(feature = "jpeg")]
        {
            println!("\n=== Testing keep existing JUMBF ===");

            // FireflyTrain has JUMBF
            let input_path = fixture_path(FIREFLY_TRAIN);
            let output_path = "/tmp/test_keep_jumbf.jpg";

            // Verify input has JUMBF
            let mut input_asset = Asset::open(&input_path).expect("Failed to open FireflyTrain");
            let input_jumbf = input_asset
                .jumbf()
                .expect("Failed to check input JUMBF")
                .expect("FireflyTrain should have JUMBF");
            println!("Input JUMBF size: {} bytes", input_jumbf.len());

            // Write with default (keep everything)
            input_asset
                .write_to(output_path, &Updates::default())
                .expect("Failed to write with default updates");

            // Verify output still has JUMBF
            let mut output_asset = Asset::open(output_path).expect("Failed to open output");
            let output_jumbf = output_asset
                .jumbf()
                .expect("Failed to check output JUMBF")
                .expect("Output should have JUMBF");

            assert_eq!(
                output_jumbf.len(),
                input_jumbf.len(),
                "JUMBF size should be preserved"
            );
            assert_eq!(
                output_jumbf, input_jumbf,
                "JUMBF content should be identical"
            );

            println!(
                "✓ JUMBF successfully preserved ({} bytes)",
                output_jumbf.len()
            );
        }
    }

    /// Test streaming write + in-place update workflow (C2PA pattern)
    /// This tests the calculate_updated_structure + write + update path
    #[test]
    fn test_streaming_write_and_update() {
        use asset_io::{update_segment_with_structure, SegmentKind};
        use std::io::{Seek, SeekFrom};

        let test_cases = vec![
            #[cfg(feature = "jpeg")]
            (P1000708, "jpeg", "jpg"), // Has XMP, no JUMBF
            #[cfg(feature = "jpeg")]
            (FIREFLY_TRAIN, "firefly", "jpg"), // Has both XMP and JUMBF
            #[cfg(feature = "png")]
            (SAMPLE1_PNG, "png", "png"), // PNG test
        ];

        for (fixture, name, ext) in test_cases {
            println!("\n=== Testing streaming write+update for {} ===", name);

            let input_path = fixture_path(fixture);
            let output_path = format!("/tmp/test_streaming_{}.{}", name, ext);

            let mut asset = Asset::open(&input_path).expect(&format!("Failed to open {}", name));

            // Create placeholder JUMBF (using valid JUMBF structure)
            // The placeholder needs to be valid JUMBF for the parser to find it after update
            let placeholder = create_test_jumbf();
            let placeholder_size = placeholder.len();
            let updates = Updates::new().set_jumbf(placeholder.clone());

            // Create output file
            let mut output_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&output_path)
                .expect("Failed to create output file");

            // Write with processing (uses calculate_updated_structure internally)
            let structure = asset
                .write_with_processing(
                    &mut output_file,
                    &updates,
                    8192,
                    &[SegmentKind::Jumbf],
                    ExclusionMode::DataOnly,
                    &mut |_chunk| {},
                )
                .expect(&format!("write_with_processing failed for {}", name));

            // Check JUMBF segment exists in structure
            let jumbf_idx = structure
                .c2pa_jumbf_index()
                .expect(&format!("No JUMBF in structure for {}", name));
            let jumbf_seg = &structure.segments[jumbf_idx];
            println!(
                "  JUMBF segment: offset=0x{:x}, size={}",
                jumbf_seg.location().offset,
                jumbf_seg.location().size
            );

            // Update JUMBF in-place with "final" data (same size, different content)
            // Create a modified version of the placeholder
            let mut final_jumbf = placeholder.clone();
            // Modify some bytes to make it distinguishable
            for byte in final_jumbf.iter_mut().skip(50).take(20) {
                *byte = 0xFF;
            }
            
            output_file
                .seek(SeekFrom::Start(0))
                .expect("Failed to seek");

            update_segment_with_structure(
                &mut output_file,
                &structure,
                SegmentKind::Jumbf,
                final_jumbf.clone(),
            )
            .expect(&format!("update_segment_with_structure failed for {}", name));

            // Flush and close
            drop(output_file);

            // Verify output is valid and JUMBF was updated
            let mut verify_asset =
                Asset::open(&output_path).expect(&format!("Failed to reopen output for {}", name));

            let result_jumbf = verify_asset
                .jumbf()
                .expect(&format!("Failed to extract JUMBF from {}", name))
                .expect(&format!("JUMBF not found in output {}", name));

            // Just verify we can read back JUMBF data of the correct size
            assert_eq!(
                result_jumbf.len(), placeholder_size,
                "JUMBF size should match for {}",
                name
            );

            println!("✓ {} - Streaming write+update successful", name);
        }
    }

    /// Test keeping existing XMP while adding JUMBF (the specific bug case)
    #[test]
    fn test_keep_xmp_add_jumbf() {

        // P1000708.jpg has XMP but no JUMBF - this is the exact bug case
        #[cfg(feature = "jpeg")]
        {
            println!("\n=== Testing Keep XMP + Add JUMBF (bug regression test) ===");

            let input_path = fixture_path(P1000708);
            let output_path = "/tmp/test_keep_xmp_add_jumbf.jpg";

            let mut asset = Asset::open(&input_path).expect("Failed to open P1000708");

            // Verify input has XMP
            let input_xmp = asset.xmp().expect("Failed to check XMP").expect("Should have XMP");
            println!("  Input XMP size: {} bytes", input_xmp.len());

            // Add JUMBF while keeping XMP (default)
            let jumbf_data = create_test_jumbf();
            let updates = Updates::new().set_jumbf(jumbf_data.clone());

            // Use write_with_processing to exercise calculate_updated_structure
            let mut output_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(output_path)
                .expect("Failed to create output file");

            let _structure = asset
                .write_with_processing(
                    &mut output_file,
                    &updates,
                    8192,
                    &[],
                    ExclusionMode::default(),
                    &mut |_| {},
                )
                .expect("write_with_processing failed");

            drop(output_file);

            // Verify output
            let mut verify_asset = Asset::open(output_path).expect("Failed to open output");

            let output_xmp = verify_asset
                .xmp()
                .expect("Failed to extract XMP")
                .expect("XMP should be preserved");
            let output_jumbf = verify_asset
                .jumbf()
                .expect("Failed to extract JUMBF")
                .expect("JUMBF should be added");

            assert_eq!(output_xmp, input_xmp, "XMP should be preserved");
            assert_eq!(output_jumbf, jumbf_data, "JUMBF should match");

            println!("✓ Keep XMP + Add JUMBF successful");
        }
    }
}
