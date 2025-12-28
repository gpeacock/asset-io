// Integration tests using the test_utils module

#[cfg(test)]
mod fixture_tests {
    use asset_io::{test_utils::*, Asset, Updates};

    #[cfg(feature = "memory-mapped")]
    use asset_io::ContainerIO;

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
            .write_to(
                output_path,
                &Updates::new().set_xmp(new_xmp.clone()),
            )
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

        // All should be JPEGs
        for fixture in &fixtures {
            let lower = fixture.to_lowercase();
            assert!(
                lower.ends_with(".jpg") || lower.ends_with(".jpeg"),
                "Fixture {} should be a JPEG",
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
        use std::fs::File;

        // Open a fixture with memory mapping
        let path = fixture_path(FIREFLY_TRAIN);
        let file = File::open(&path).expect("Failed to open file");

        // Create memory map
        let mmap = unsafe { memmap2::Mmap::map(&file).expect("Failed to mmap") };
        let file_size = mmap.len() as u64;

        println!("Memory-mapped {} ({} bytes)", path.display(), file_size);

        // Parse the file structure
        let handler = asset_io::JpegIO::new();
        let mut file_for_parse = File::open(&path).expect("Failed to open file");
        let mut structure = handler.parse(&mut file_for_parse).expect("Failed to parse");

        // Attach mmap to structure
        structure = structure.with_mmap(mmap);

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

        // Test 3: LazyData with mmap
        #[cfg(feature = "memory-mapped")]
        {
            use asset_io::LazyData;
            use std::sync::Arc;

            let file = File::open(&path).expect("Failed to open file");
            let mmap = unsafe { memmap2::Mmap::map(&file).expect("Failed to mmap") };
            let mmap_arc = Arc::new(mmap);

            let lazy = LazyData::from_mmap(mmap_arc, 0, 100);

            // Get should work without any I/O
            let data = lazy.get().expect("Should get data from mmap");
            assert_eq!(data.len(), 100);
            assert_eq!(data[0], 0xFF);

            println!("  ✓ LazyData::MemoryMapped works");
        }

        println!("✓ All memory-mapped tests passed!");
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use asset_io::{test_utils, ContainerIO, JpegIO};
    use std::fs::File;

    #[test]
    fn test_image_data_range() {
        println!("\n=== Testing image_data_range() ===");

        let path = test_utils::fixture_path(test_utils::P1000708);
        let mut file = File::open(&path).expect("Failed to open file");

        let handler = JpegIO::new();
        let structure = handler.parse(&mut file).expect("Failed to parse");

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
        println!("\n=== Testing embedded_thumbnail() ===");

        use asset_io::Asset;
        let path = test_utils::fixture_path(test_utils::P1000708);
        let mut asset = Asset::open(&path).expect("Failed to open file");

        // Try to get embedded thumbnail (may or may not exist)
        match asset.embedded_thumbnail() {
            Ok(Some(thumb)) => {
                println!("  ✓ Found embedded thumbnail:");
                println!("    Container: {:?}", thumb.container);
                println!("    Offset: {} bytes", thumb.offset);
                println!("    Size: {} bytes", thumb.size);
                if let (Some(w), Some(h)) = (thumb.width, thumb.height) {
                    println!("    Dimensions: {}x{}", w, h);
                }

                // Verify it's reasonable
                assert!(thumb.size > 0, "Thumbnail size should not be zero");
                assert!(thumb.size < 100_000, "Thumbnail should be small");
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
        println!("\n=== Testing memory-mapped image access ===");

        let path = test_utils::fixture_path(test_utils::P1000708);
        let mut file = File::open(&path).expect("Failed to open file");

        let handler = JpegIO::new();
        let structure = handler.parse(&mut file).expect("Failed to parse");

        // Get image data range
        let range = structure
            .image_data_range()
            .expect("Should have image data");

        // Memory-map the file
        let file = File::open(&path).expect("Failed to open file");
        let mmap = unsafe { memmap2::Mmap::map(&file).expect("Failed to mmap") };
        let structure = structure.with_mmap(mmap);

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
    fn test_thumbnail_options() {
        println!("\n=== Testing ThumbnailOptions ===");

        use asset_io::ThumbnailOptions;

        let default_opts = ThumbnailOptions::default();
        assert_eq!(default_opts.max_width, 256);
        assert_eq!(default_opts.max_height, 256);
        assert_eq!(default_opts.quality, 85);
        assert!(default_opts.prefer_embedded);

        let custom_opts = ThumbnailOptions {
            max_width: 512,
            max_height: 512,
            quality: 90,
            prefer_embedded: false,
        };
        assert_eq!(custom_opts.max_width, 512);

        println!("  ✓ ThumbnailOptions works correctly");
    }

    #[test]
    fn test_embedded_thumbnail_fits() {
        println!("\n=== Testing EmbeddedThumbnail::fits() ===");

        use asset_io::{EmbeddedThumbnail, ThumbnailFormat};

        let thumb =
            EmbeddedThumbnail::with_dimensions(vec![0u8; 100], ThumbnailFormat::Jpeg, 160, 120);

        assert!(thumb.fits(256, 256), "160x120 should fit in 256x256");
        assert!(
            thumb.fits(160, 120),
            "160x120 should fit exactly in 160x120"
        );
        assert!(!thumb.fits(128, 128), "160x120 should not fit in 128x128");

        // Thumbnail without dimensions doesn't fit
        let thumb_no_dims = EmbeddedThumbnail::new(0, 100, ThumbnailFormat::Jpeg, None, None);
        assert!(!thumb_no_dims.fits(256, 256), "Unknown size should not fit");

        println!("  ✓ EmbeddedThumbnail::fits() works correctly");
    }
}

#[cfg(all(test, feature = "png"))]
mod png_tests {
    use asset_io::{
        Asset, Container, ContainerIO, MediaType, PngIO, Updates,
    };
    use std::io::Cursor;

    /// Create a minimal valid PNG (1x1 pixel, RGB)
    fn create_minimal_png() -> Vec<u8> {
        let mut data = Vec::new();

        // PNG signature
        data.extend_from_slice(b"\x89PNG\r\n\x1a\n");

        // IHDR chunk (1x1 RGB image)
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x0D, // Length: 13
        ]);
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x01, // Width: 1
            0x00, 0x00, 0x00, 0x01, // Height: 1
            0x08, // Bit depth: 8
            0x02, // Color type: RGB (2)
            0x00, // Compression: deflate
            0x00, // Filter: adaptive
            0x00, // Interlace: none
        ]);
        data.extend_from_slice(&0x90770c9e_u32.to_be_bytes()); // CRC

        // IDAT chunk (minimal compressed image data)
        // This is the zlib-compressed representation of a 1x1 RGB pixel (black)
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x0C, // Length: 12
        ]);
        data.extend_from_slice(b"IDAT");
        data.extend_from_slice(&[
            0x08, 0x1D, 0x01, 0x03, 0x00, 0xFC, 0xFF, 0x00, 0x00, 0x00, 0x06, 0x00,
        ]);
        data.extend_from_slice(&0x0364CB4D_u32.to_be_bytes()); // CRC

        // IEND chunk
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, // Length: 0
        ]);
        data.extend_from_slice(b"IEND");
        data.extend_from_slice(&0xAE426082_u32.to_be_bytes()); // CRC

        data
    }

    #[test]
    fn test_png_parse_minimal() {
        let png_data = create_minimal_png();
        let mut cursor = Cursor::new(png_data);

        let handler = PngIO::new();
        let structure = handler.parse(&mut cursor).unwrap();

        assert_eq!(structure.container, Container::Png);
        assert!(!structure.segments.is_empty());
    }

    #[test]
    fn test_png_round_trip_no_metadata() {
        let png_data = create_minimal_png();
        let mut input = Cursor::new(png_data.clone());

        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Write with no changes
        input.set_position(0);
        let mut output = Vec::new();
        let updates = Updates::default();
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Output should be valid PNG
        assert_eq!(&output[0..8], b"\x89PNG\r\n\x1a\n");

        // Should be parseable
        let mut output_cursor = Cursor::new(output);
        let result = handler.parse(&mut output_cursor);
        assert!(result.is_ok());
    }

    #[test]
    fn test_png_add_xmp() {
        let png_data = create_minimal_png();
        let mut input = Cursor::new(png_data);

        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add XMP metadata
        let xmp_data = b"<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><test>PNG XMP</test></rdf:RDF>".to_vec();
        input.set_position(0);
        let mut output = Vec::new();
        let updates = Updates::new().set_xmp(xmp_data.clone());
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Parse the output and verify XMP was added
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();

        let result_xmp = asset.xmp().unwrap();
        assert!(result_xmp.is_some());
        assert_eq!(result_xmp.unwrap(), xmp_data);
    }

    #[test]
    fn test_png_add_jumbf() {
        let png_data = create_minimal_png();
        let mut input = Cursor::new(png_data);

        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add JUMBF metadata
        let jumbf_data = b"Test JUMBF data for PNG".to_vec();
        input.set_position(0);
        let mut output = Vec::new();
        let updates = Updates::new().set_jumbf(jumbf_data.clone());
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Parse the output and verify JUMBF was added
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();

        let result_jumbf = asset.jumbf().unwrap();
        assert!(result_jumbf.is_some());
        assert_eq!(result_jumbf.unwrap(), jumbf_data);
    }

    #[test]
    fn test_png_add_both_xmp_and_jumbf() {
        let png_data = create_minimal_png();
        let mut input = Cursor::new(png_data);

        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add both XMP and JUMBF
        let xmp_data = b"<test>XMP Data</test>".to_vec();
        let jumbf_data = b"JUMBF Data".to_vec();

        input.set_position(0);
        let mut output = Vec::new();
        let updates = Updates::new()
            .set_xmp(xmp_data.clone())
            .set_jumbf(jumbf_data.clone());
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Verify both were added
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();

        assert_eq!(asset.xmp().unwrap().unwrap(), xmp_data);
        assert_eq!(asset.jumbf().unwrap().unwrap(), jumbf_data);
    }

    #[test]
    fn test_png_format_detection() {
        let png_data = create_minimal_png();
        let cursor = Cursor::new(png_data);

        let asset = Asset::from_source_with_format(cursor, MediaType::Png).unwrap();
        assert_eq!(asset.container(), Container::Png);
    }

    #[test]
    fn test_png_remove_metadata() {
        // Start with PNG that has XMP
        let base_png = create_minimal_png();
        let mut input = Cursor::new(base_png.clone());

        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add XMP first
        let xmp_data = b"<test>To be removed</test>".to_vec();
        input.set_position(0);
        let mut with_xmp = Vec::new();
        handler
            .write(
                &structure,
                &mut input,
                &mut with_xmp,
                &Updates::new().set_xmp(xmp_data),
            )
            .unwrap();

        // Now remove the XMP
        let mut input2 = Cursor::new(with_xmp);
        let structure2 = handler.parse(&mut input2).unwrap();
        input2.set_position(0);
        let mut output = Vec::new();
        handler
            .write(
                &structure2,
                &mut input2,
                &mut output,
                &Updates::new().remove_xmp(),
            )
            .unwrap();

        // Verify XMP was removed
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();
        assert!(asset.xmp().unwrap().is_none());
    }

    #[test]
    fn test_png_parse_real_file() {
        use std::fs::File;

        let mut file = File::open("tests/fixtures/GreenCat.png").unwrap();
        let handler = PngIO::new();
        let structure = handler.parse(&mut file).unwrap();

        assert_eq!(structure.container, Container::Png);
        assert!(!structure.segments.is_empty());
        println!(
            "Parsed {} segments from GreenCat.png",
            structure.segments.len()
        );
    }

    #[test]
    fn test_png_round_trip_real_file() {
        use std::fs::File;

        let mut input = File::open("tests/fixtures/GreenCat.png").unwrap();
        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Write with no changes
        let mut output = Vec::new();
        let updates = Updates::default();
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Output should be valid PNG
        assert_eq!(&output[0..8], b"\x89PNG\r\n\x1a\n");

        // Write output to temp file for debugging
        std::fs::write("/tmp/png_output.png", &output).ok();

        // Should be parseable
        let mut output_cursor = Cursor::new(output);
        let result = handler.parse(&mut output_cursor);
        if let Err(e) = &result {
            eprintln!("Parse error: {:?}", e);
        }
        assert!(result.is_ok(), "Output PNG should be parseable");
    }

    #[test]
    fn test_png_add_xmp_to_real_file() {
        use std::fs::File;

        let mut input = File::open("tests/fixtures/GreenCat.png").unwrap();
        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add XMP metadata
        let xmp_data = b"<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><test>PNG XMP</test></rdf:RDF>".to_vec();
        let mut output = Vec::new();
        let updates = Updates::new().set_xmp(xmp_data.clone());
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Parse the output and verify XMP was added
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();

        let result_xmp = asset.xmp().unwrap();
        assert!(result_xmp.is_some());
        assert_eq!(result_xmp.unwrap(), xmp_data);
    }

    #[test]
    fn test_png_add_jumbf_to_real_file() {
        use std::fs::File;

        let mut input = File::open("tests/fixtures/GreenCat.png").unwrap();
        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add JUMBF metadata
        let jumbf_data = b"Test JUMBF data for PNG".to_vec();
        let mut output = Vec::new();
        let updates = Updates::new().set_jumbf(jumbf_data.clone());
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Parse the output and verify JUMBF was added
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();

        let result_jumbf = asset.jumbf().unwrap();
        assert!(result_jumbf.is_some());
        assert_eq!(result_jumbf.unwrap(), jumbf_data);
    }

    #[test]
    fn test_png_add_both_xmp_and_jumbf_to_real_file() {
        use std::fs::File;

        let mut input = File::open("tests/fixtures/GreenCat.png").unwrap();
        let handler = PngIO::new();
        let structure = handler.parse(&mut input).unwrap();

        // Add both XMP and JUMBF
        let xmp_data = b"<test>XMP Data</test>".to_vec();
        let jumbf_data = b"JUMBF Data".to_vec();

        let mut output = Vec::new();
        let updates = Updates::new()
            .set_xmp(xmp_data.clone())
            .set_jumbf(jumbf_data.clone());
        handler
            .write(&structure, &mut input, &mut output, &updates)
            .unwrap();

        // Verify both were added
        let output_cursor = Cursor::new(output);
        let mut asset = Asset::from_source_with_format(output_cursor, MediaType::Png).unwrap();

        assert_eq!(asset.xmp().unwrap().unwrap(), xmp_data);
        assert_eq!(asset.jumbf().unwrap().unwrap(), jumbf_data);
    }
}
