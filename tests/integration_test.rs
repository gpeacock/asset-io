// Integration tests using the test_utils module

#[cfg(test)]
mod fixture_tests {
    use jumbf_io::{test_utils::*, Asset, Updates, XmpUpdate};
    
    #[cfg(feature = "memory-mapped")]
    use jumbf_io::FormatHandler;

    #[test]
    fn test_embedded_fixture_access() {
        // This test works with or without embed-fixtures feature
        let result = fixture_bytes(FIREFLY_TRAIN);
        assert!(result.is_ok(), "Should be able to load FIREFLY_TRAIN fixture");
        
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
        
        let mut asset = Asset::open(&path)
            .expect("Failed to parse asset");
        
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
        
        let mut asset = Asset::open(&path)
            .expect("Failed to parse asset");
        
        let new_xmp = b"<test>Modified XMP</test>".to_vec();
        
        asset.write_to(output_path, &Updates {
            xmp: XmpUpdate::Set(new_xmp.clone()),
            ..Default::default()
        }).expect("Failed to write asset");
        
        // Parse the output
        let mut verify = Asset::open(output_path)
            .expect("Failed to parse output");
        
        let result_xmp = verify.xmp().expect("Failed to read XMP").unwrap();
        assert_eq!(result_xmp, new_xmp, "XMP should be modified");
        
        // Cleanup
        std::fs::remove_file(output_path).ok();
    }

    #[test]
    fn test_list_fixtures() {
        let fixtures = list_fixtures().expect("Failed to list fixtures");
        
        // Should have at least some fixtures
        assert!(!fixtures.is_empty(), "Should have fixtures in tests/fixtures");
        
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
        assert!(!registry.is_empty(), "Registry should be populated with embed-fixtures");
        
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
        assert!(registry.is_empty(), "Registry should be empty without embed-fixtures");
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
        let handler = jumbf_io::JpegHandler::new();
        let mut file_for_parse = File::open(&path).expect("Failed to open file");
        let mut structure = handler.parse(&mut file_for_parse).expect("Failed to parse");
        
        // Attach mmap to structure
        structure = structure.with_mmap(mmap);
        
        // Test 1: Get a byte range via mmap (zero-copy)
        let range = jumbf_io::ByteRange {
            offset: 0,
            size: 100,
        };
        
        let slice = structure.get_mmap_slice(range).expect("Should get mmap slice");
        assert_eq!(slice.len(), 100);
        assert_eq!(slice[0], 0xFF); // JPEG SOI marker
        assert_eq!(slice[1], 0xD8);
        
        println!("  ✓ Zero-copy slice access works");
        
        // Test 2: Verify we can iterate through segments via mmap
        let mut total_bytes_accessed = 0u64;
        for (i, segment) in structure.segments.iter().enumerate() {
            let loc = segment.location();
            if let Some(slice) = structure.get_mmap_slice(jumbf_io::ByteRange {
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
        
        println!("  ✓ Accessed {} bytes via mmap across {} segments", 
                 total_bytes_accessed, structure.segments.len());
        
        // Test 3: LazyData with mmap
        #[cfg(feature = "memory-mapped")]
        {
            use jumbf_io::LazyData;
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

