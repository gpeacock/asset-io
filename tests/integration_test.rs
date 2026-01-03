// Integration tests using the test_utils module

#[cfg(test)]
mod fixture_tests {
    use asset_io::{test_utils::*, Asset, Updates};

    #[test]
    fn test_embedded_fixture_access() {
        // This test works with or without embed-fixtures feature
        let result = fixture_bytes(FIREFLY_TRAIN);
        assert!(
            result.is_ok(),
            "Should be able to load FIREFLY_TRAIN fixture"
        );

        let data = result.unwrap();
        assert!(!data.is_empty(), "Fixture data should not be empty");

        // Verify it starts with JPEG magic bytes
        assert_eq!(&data[0..2], &[0xFF, 0xD8], "Should be valid JPEG");
    }

    #[test]
    fn test_create_streams() {
        let result = create_test_streams(FIREFLY_TRAIN);
        assert!(result.is_ok(), "Should create streams for FIREFLY_TRAIN");

        let (format, input, _output) = result.unwrap();
        assert_eq!(format, "image/jpeg");
        assert!(!input.get_ref().is_empty());
    }

    #[test]
    fn test_fixture_path_resolution() {
        // Should return path to tests/fixtures by default
        let path = fixture_path("test.jpg");
        assert!(path.to_str().unwrap().contains("tests/fixtures"));
        assert!(path.to_str().unwrap().ends_with("test.jpg"));
    }

    #[test]
    fn test_parse_with_test_utils() {
        // Demonstrate using test utils with Asset API
        let path = fixture_path(FIREFLY_TRAIN);

        let mut asset = Asset::open(&path).expect("Failed to parse asset");

        // Verify we can read metadata
        let xmp = asset.xmp().expect("Failed to read XMP");
        assert!(xmp.is_some(), "FireflyTrain should have XMP");

        let structure = asset.structure();
        assert!(!structure.segments.is_empty(), "Should have segments");
    }

    #[test]
    fn test_round_trip_with_test_utils() {
        // Test that we can parse, modify, and write back
        let path = fixture_path(FIREFLY_TRAIN);
        let output_path = "/tmp/test_integration_output.jpg";

        let mut asset = Asset::open(&path).expect("Failed to parse asset");

        let new_xmp = b"<test>Modified XMP</test>".to_vec();

        asset
            .write_to(output_path, &Updates::new().set_xmp(new_xmp.clone()))
            .expect("Failed to write asset");

        // Parse the output
        let mut verify = Asset::open(output_path).expect("Failed to parse output");

        let result_xmp = verify.xmp().expect("Failed to read XMP").unwrap();
        assert_eq!(result_xmp, new_xmp, "XMP should be modified");

        // Cleanup
        std::fs::remove_file(output_path).ok();
    }

    #[test]
    fn test_list_fixtures() {
        let fixtures = list_fixtures().expect("Failed to list fixtures");

        // Should have at least some fixtures
        assert!(
            !fixtures.is_empty(),
            "Should have fixtures in tests/fixtures"
        );

        // All should be supported image formats (JPEG or PNG)
        for fixture in &fixtures {
            let lower = fixture.to_lowercase();
            assert!(
                lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".png"),
                "Fixture {} should be a supported format (JPEG or PNG)",
                fixture
            );
        }
    }

    #[test]
    #[cfg(feature = "embed-fixtures")]
    fn test_registry_populated() {
        let registry = get_registry();

        // With embed-fixtures, registry should be populated
        assert!(
            !registry.is_empty(),
            "Registry should be populated with embed-fixtures"
        );

        // Check known fixtures
        assert!(registry.contains_key(FIREFLY_TRAIN));
        assert!(registry.contains_key(DESIGNER));

        // Verify data is valid
        let (data, format) = registry.get(FIREFLY_TRAIN).unwrap();
        assert_eq!(*format, "image/jpeg");
        assert!(&data[0..2] == &[0xFF, 0xD8], "Should be valid JPEG");
    }

    #[test]
    #[cfg(not(feature = "embed-fixtures"))]
    fn test_registry_empty_without_feature() {
        let registry = get_registry();

        // Without embed-fixtures, registry should be empty
        assert!(
            registry.is_empty(),
            "Registry should be empty without embed-fixtures"
        );
    }

    #[test]
    fn test_extended_fixtures_env_var() {
        // This test demonstrates using JUMBF_TEST_FIXTURES
        // In practice, you'd set this env var in your shell or CI

        // If the env var is set, we should be able to load fixtures from it
        if let Ok(custom_dir) = std::env::var("JUMBF_TEST_FIXTURES") {
            println!("Using extended fixtures from: {}", custom_dir);

            // Try to load a fixture that might be in the extended set
            let path = fixture_path("capture.jpg");
            if path.exists() {
                let data = std::fs::read(&path).expect("Should read extended fixture");
                assert!(&data[0..2] == &[0xFF, 0xD8], "Should be valid JPEG");
                println!("Successfully loaded extended fixture: capture.jpg");
            }
        } else {
            println!("JUMBF_TEST_FIXTURES not set, using default fixtures only");
        }
    }

    #[test]
    #[cfg(feature = "memory-mapped")]
    fn test_memory_mapped_access() {
        // Open a fixture with memory mapping using the public API
        let path = fixture_path(FIREFLY_TRAIN);

        // SAFETY: Test fixture is read-only and won't be modified during test
        let asset = unsafe { Asset::open_with_mmap(&path).expect("Failed to open with mmap") };

        let structure = asset.structure();
        println!(
            "Memory-mapped {} ({} bytes)",
            path.display(),
            structure.total_size
        );

        // Test 1: Get a byte range via mmap (zero-copy)
        let range = asset_io::ByteRange {
            offset: 0,
            size: 100,
        };

        let slice = structure
            .get_mmap_slice(range)
            .expect("Should get mmap slice");
        assert_eq!(slice.len(), 100);
        assert_eq!(slice[0], 0xFF); // JPEG SOI marker
        assert_eq!(slice[1], 0xD8);

        println!("  ✓ Zero-copy slice access works");

        // Test 2: Verify we can iterate through segments via mmap
        let mut total_bytes_accessed = 0u64;
        for (i, segment) in structure.segments.iter().enumerate() {
            let loc = segment.location();
            if let Some(slice) = structure.get_mmap_slice(asset_io::ByteRange {
                offset: loc.offset,
                size: loc.size,
            }) {
                assert_eq!(slice.len(), loc.size as usize);
                total_bytes_accessed += loc.size;

                // Verify first segment is SOI marker
                if i == 0 {
                    assert_eq!(slice[0], 0xFF);
                }
            }
        }

        println!(
            "  ✓ Accessed {} bytes via mmap across {} segments",
            total_bytes_accessed,
            structure.segments.len()
        );

        println!("✓ All memory-mapped tests passed!");
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use asset_io::{test_utils, Asset};

    #[test]
    fn test_image_data_range() {
        println!("\n=== Testing image_data_range() ===");

        let path = test_utils::fixture_path(test_utils::P1000708);
        let asset = Asset::open(&path).expect("Failed to open file");
        let structure = asset.structure();

        // Should find image data
        let range = structure
            .image_data_range()
            .expect("Should have image data");

        println!("  Image data range:");
        println!("    Offset: {}", range.offset);
        println!("    Size: {} bytes", range.size);

        // Verify it's reasonable
        assert!(range.offset > 0, "Offset should be after headers");
        assert!(range.size > 1000, "Image data should be substantial");
        assert!(
            range.offset + range.size <= structure.total_size,
            "Range should fit in file"
        );

        println!("  ✓ image_data_range() works correctly");
    }

    #[test]
    #[cfg(feature = "exif")]
    fn test_embedded_thumbnail() {
        println!("\n=== Testing read_embedded_thumbnail() ===");

        use asset_io::Asset;
        let path = test_utils::fixture_path(test_utils::P1000708);
        let mut asset = Asset::open(&path).expect("Failed to open file");

        // Try to get embedded thumbnail (may or may not exist)
        match asset.read_embedded_thumbnail() {
            Ok(Some(thumb)) => {
                println!("  ✓ Found embedded thumbnail:");
                println!("    Format: {:?}", thumb.format);
                println!("    Data size: {} bytes", thumb.data.len());
                if let (Some(w), Some(h)) = (thumb.width, thumb.height) {
                    println!("    Dimensions: {}x{}", w, h);
                }

                // Verify it's reasonable
                assert!(!thumb.data.is_empty(), "Thumbnail data should not be empty");
                assert!(thumb.data.len() < 100_000, "Thumbnail should be small");
            }
            Ok(None) => {
                println!("  ℹ No embedded thumbnail (expected for most test files)");
            }
            Err(e) => {
                panic!("Error checking for embedded thumbnail: {}", e);
            }
        }

        println!("  ✓ embedded_thumbnail() API works correctly");
    }

    #[test]
    #[cfg(feature = "memory-mapped")]
    fn test_mmap_image_slice() {
        use asset_io::Asset;

        println!("\n=== Testing memory-mapped image access ===");

        let path = test_utils::fixture_path(test_utils::P1000708);

        // SAFETY: Test fixture is read-only and won't be modified during test
        let asset = unsafe { Asset::open_with_mmap(&path).expect("Failed to open with mmap") };

        let structure = asset.structure();

        // Get image data range
        let range = structure
            .image_data_range()
            .expect("Should have image data");

        // Get zero-copy slice
        let slice = structure.get_mmap_slice(range).expect("Should get slice");

        println!("  Image data slice:");
        println!("    Size: {} bytes", slice.len());
        println!(
            "    First bytes: {:02X} {:02X} {:02X} {:02X}",
            slice[0], slice[1], slice[2], slice[3]
        );

        // Verify it's the right size
        assert_eq!(slice.len(), range.size as usize);

        // JPEG compressed data often starts with FF DA (SOS marker)
        // or contains JPEG data
        assert_eq!(slice[0], 0xFF, "Should be JPEG marker");

        println!("  ✓ Zero-copy memory-mapped image access works");
    }

    #[test]
    fn test_thumbnail_creation() {
        println!("\n=== Testing Thumbnail creation ===");

        use asset_io::{Thumbnail, ThumbnailKind};

        // Test with_dimensions
        let thumb = Thumbnail::with_dimensions(vec![0u8; 100], ThumbnailKind::Jpeg, 160, 120);
        assert_eq!(thumb.data.len(), 100);
        assert_eq!(thumb.format, ThumbnailKind::Jpeg);
        assert_eq!(thumb.width, Some(160));
        assert_eq!(thumb.height, Some(120));

        // Test new (without dimensions)
        let thumb_no_dims = Thumbnail::new(vec![0u8; 50], ThumbnailKind::Png);
        assert_eq!(thumb_no_dims.data.len(), 50);
        assert_eq!(thumb_no_dims.format, ThumbnailKind::Png);
        assert_eq!(thumb_no_dims.width, None);
        assert_eq!(thumb_no_dims.height, None);

        println!("  ✓ Thumbnail creation works correctly");
    }
}

#[cfg(all(test, feature = "png"))]
mod png_tests {
    use asset_io::{Asset, ContainerKind, Updates};
    use std::io::Cursor;

    #[test]
    fn test_png_parse_real_file() {
        let asset = Asset::open("tests/fixtures/GreenCat.png").unwrap();

        assert_eq!(asset.structure().container, ContainerKind::Png);
        assert!(!asset.structure().segments.is_empty());
        println!(
            "Parsed {} segments from GreenCat.png",
            asset.structure().segments.len()
        );
    }

    #[test]
    fn test_png_round_trip_real_file() {
        let mut asset = Asset::open("tests/fixtures/GreenCat.png").unwrap();

        // Write with no changes
        let mut output = Vec::new();
        let updates = Updates::default();
        asset.write(&mut output, &updates).unwrap();

        // Output should be valid PNG
        assert_eq!(&output[0..8], b"\x89PNG\r\n\x1a\n");

        // Write output to temp file for debugging
        std::fs::write("/tmp/png_output.png", &output).ok();

        // Should be parseable
        let mut output_cursor = Cursor::new(output.clone());
        let result = Asset::from_source(&mut output_cursor);
        if let Err(e) = &result {
            eprintln!("Parse error: {:?}", e);
        }
        assert!(result.is_ok(), "Output PNG should be parseable");
    }

    #[test]
    fn test_png_add_xmp_to_real_file() {
        let mut asset = Asset::open("tests/fixtures/GreenCat.png").unwrap();

        // Add XMP metadata
        let xmp_data = b"<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><test>PNG XMP</test></rdf:RDF>".to_vec();
        let mut output = Vec::new();
        let updates = Updates::new().set_xmp(xmp_data.clone());
        asset.write(&mut output, &updates).unwrap();

        // Parse the output and verify XMP was added
        let mut output_cursor = Cursor::new(output);
        let mut parsed = Asset::from_source(&mut output_cursor).unwrap();

        let result_xmp = parsed.xmp().unwrap();
        assert!(result_xmp.is_some());
        assert_eq!(result_xmp.unwrap(), xmp_data);
    }

    #[test]
    fn test_png_add_jumbf_to_real_file() {
        let mut asset = Asset::open("tests/fixtures/GreenCat.png").unwrap();

        // Add JUMBF metadata
        let jumbf_data = b"Test JUMBF data for PNG".to_vec();
        let mut output = Vec::new();
        let updates = Updates::new().set_jumbf(jumbf_data.clone());
        asset.write(&mut output, &updates).unwrap();

        // Parse the output and verify JUMBF was added
        let mut output_cursor = Cursor::new(output);
        let mut parsed = Asset::from_source(&mut output_cursor).unwrap();

        let result_jumbf = parsed.jumbf().unwrap();
        assert!(result_jumbf.is_some());
        assert_eq!(result_jumbf.unwrap(), jumbf_data);
    }

    #[test]
    fn test_png_add_both_xmp_and_jumbf_to_real_file() {
        let mut asset = Asset::open("tests/fixtures/GreenCat.png").unwrap();

        // Add both XMP and JUMBF
        let xmp_data = b"<test>XMP Data</test>".to_vec();
        let jumbf_data = b"JUMBF Data".to_vec();

        let mut output = Vec::new();
        let updates = Updates::new()
            .set_xmp(xmp_data.clone())
            .set_jumbf(jumbf_data.clone());
        asset.write(&mut output, &updates).unwrap();

        // Verify both were added
        let mut output_cursor = Cursor::new(output);
        let mut parsed = Asset::from_source(&mut output_cursor).unwrap();

        assert_eq!(parsed.xmp().unwrap().unwrap(), xmp_data);
        assert_eq!(parsed.jumbf().unwrap().unwrap(), jumbf_data);
    }
}

/// Tests for streaming write + in-place update workflow (critical for C2PA)
#[cfg(test)]
mod streaming_tests {
    use asset_io::{test_utils::*, Asset, ExclusionMode, SegmentKind, Updates};
    use std::io::{Seek, SeekFrom};

    /// Create a minimal valid JUMBF structure for testing
    fn create_test_jumbf() -> Vec<u8> {
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

    /// Test streaming write + in-place update workflow (C2PA pattern)
    /// This tests the calculate_updated_structure + write + update path
    #[test]
    fn test_streaming_write_and_update() {
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
            let placeholder = create_test_jumbf();
            let placeholder_size = placeholder.len();
            let updates = Updates::new()
                .set_jumbf(placeholder.clone())
                .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

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
                .write_with_processing(&mut output_file, &updates, &mut |_chunk| {})
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
            let mut final_jumbf = placeholder.clone();
            // Modify some bytes to make it distinguishable
            for byte in final_jumbf.iter_mut().skip(50).take(20) {
                *byte = 0xFF;
            }

            output_file
                .seek(SeekFrom::Start(0))
                .expect("Failed to seek");

            structure
                .update_segment(&mut output_file, SegmentKind::Jumbf, final_jumbf.clone())
                .expect(&format!("update_segment failed for {}", name));

            // Flush and close
            drop(output_file);

            // Verify output is valid and JUMBF was updated
            let mut verify_asset =
                Asset::open(&output_path).expect(&format!("Failed to reopen output for {}", name));

            let result_jumbf = verify_asset
                .jumbf()
                .expect(&format!("Failed to extract JUMBF from {}", name))
                .expect(&format!("JUMBF not found in output {}", name));

            // Verify we can read back JUMBF data of the correct size
            assert_eq!(
                result_jumbf.len(),
                placeholder_size,
                "JUMBF size should match for {}",
                name
            );

            println!("✓ {} - Streaming write+update successful", name);
        }
    }

    /// Test keeping existing XMP while adding JUMBF (regression test)
    #[test]
    #[cfg(feature = "jpeg")]
    fn test_keep_xmp_add_jumbf() {
        println!("\n=== Testing Keep XMP + Add JUMBF (regression test) ===");

        let input_path = fixture_path(P1000708);
        let output_path = "/tmp/test_keep_xmp_add_jumbf.jpg";

        let mut asset = Asset::open(&input_path).expect("Failed to open P1000708");

        // Verify input has XMP
        let input_xmp = asset
            .xmp()
            .expect("Failed to check XMP")
            .expect("Should have XMP");
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
            .write_with_processing(&mut output_file, &updates, &mut |_| {})
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

    /// Test all metadata modification combinations
    #[test]
    fn test_metadata_modifications() {
        #[cfg(feature = "jpeg")]
        {
            let test_xmp = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Test XMP Data</dc:title>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
                .to_vec();

            let input_path = fixture_path(P1000708);

            // Test: Add XMP
            {
                let output = "/tmp/test_add_xmp.jpg";
                let mut asset = Asset::open(&input_path).expect("Failed to open");
                asset
                    .write_to(output, &Updates::new().set_xmp(test_xmp.clone()))
                    .expect("Write failed");

                let mut verify = Asset::open(output).expect("Failed to reopen");
                assert!(verify.xmp().unwrap().is_some(), "XMP should be added");
            }

            // Test: Add JUMBF
            {
                let output = "/tmp/test_add_jumbf.jpg";
                let jumbf = create_test_jumbf();
                let mut asset = Asset::open(&input_path).expect("Failed to open");
                asset
                    .write_to(output, &Updates::new().set_jumbf(jumbf.clone()))
                    .expect("Write failed");

                let mut verify = Asset::open(output).expect("Failed to reopen");
                let result = verify.jumbf().unwrap().expect("JUMBF should be added");
                assert_eq!(result, jumbf);
            }

            // Test: Remove XMP
            {
                // First add XMP, then remove it
                let temp = "/tmp/test_remove_xmp_temp.jpg";
                let output = "/tmp/test_remove_xmp.jpg";

                let mut asset = Asset::open(&input_path).expect("Failed to open");
                asset
                    .write_to(temp, &Updates::new().set_xmp(test_xmp.clone()))
                    .expect("Write failed");

                let mut asset2 = Asset::open(temp).expect("Failed to open temp");
                asset2
                    .write_to(output, &Updates::new().remove_xmp())
                    .expect("Write failed");

                let mut verify = Asset::open(output).expect("Failed to reopen");
                assert!(verify.xmp().unwrap().is_none(), "XMP should be removed");
            }

            println!("✓ All metadata modification tests passed");
        }
    }
}
