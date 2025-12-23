// Integration tests using the test_utils module

#[cfg(test)]
mod fixture_tests {
    use jumbf_io::{test_utils::*, Asset, Updates, XmpUpdate};

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
}

