//! TEMPORARY C2PA Integration Test
//!
//! Tests how asset-io integrates with the c2pa crate's embeddable APIs.
//!
//! TODO: DELETE THIS FILE after validation is complete.
//! This is intentionally kept separate and will NOT be part of the library long-term.
//!
//! To remove this example:
//! 1. Delete this file: examples/c2pa_integration.rs
//! 2. Remove c2pa from [dev-dependencies] in Cargo.toml
//! 3. Remove [[example]] section for c2pa_integration in Cargo.toml

use asset_io::Asset;
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== C2PA + asset-io Integration Test ===\n");

    // Test if we can find a sample image
    let test_image = find_test_image()?;
    println!("Testing with: {}\n", test_image);

    // ========================================
    // PART 1: Read file structure with asset-io
    // ========================================
    println!("--- Part 1: Reading with asset-io ---");

    let file = File::open(&test_image)?;
    let mut asset = Asset::from_source(file)?;

    println!("Container: {:?}", asset.container());
    println!("Media type: {:?}", asset.media_type());
    println!("Segments: {}", asset.structure().segments.len());

    for (i, segment) in asset.structure().segments.iter().enumerate() {
        let location = segment.location();
        let seg_type = match segment {
            asset_io::Segment::Header { .. } => "Header",
            asset_io::Segment::Xmp { segments, .. } => {
                if segments.len() > 1 {
                    "XMP (multi-part)"
                } else {
                    "XMP"
                }
            }
            asset_io::Segment::Jumbf { segments, .. } => {
                if segments.len() > 1 {
                    "JUMBF (multi-part)"
                } else {
                    "JUMBF/C2PA"
                }
            }
            asset_io::Segment::ImageData { .. } => "ImageData",
            asset_io::Segment::Exif { .. } => "EXIF",
            asset_io::Segment::Other { label, .. } => label,
        };

        println!(
            "  Segment {}: {:20} @ offset {:#x} ({} bytes)",
            i, seg_type, location.offset, location.size
        );
    }

    // ========================================
    // PART 2: Extract metadata with asset-io
    // ========================================
    println!("\n--- Part 2: Extracting Metadata ---");

    // Try to extract XMP
    #[cfg(feature = "xmp")]
    if let Some(xmp_data) = asset.xmp()? {
        println!("Found XMP ({} bytes)", xmp_data.len());

        // Parse some common XMP fields
        use asset_io::xmp::get_keys;
        let xmp_str = String::from_utf8_lossy(&xmp_data);
        let values = get_keys(
            &xmp_str,
            &["dcterms:provenance", "xmpMM:InstanceID", "xmpMM:DocumentID"],
        );

        println!("  dcterms:provenance: {:?}", values[0]);
        println!("  xmpMM:InstanceID: {:?}", values[1]);
        println!("  xmpMM:DocumentID: {:?}", values[2]);
    } else {
        println!("No XMP found");
    }

    // Try to extract EXIF
    #[cfg(feature = "exif")]
    {
        // Check if there's an EXIF segment in the structure
        let has_exif = asset
            .structure()
            .segments
            .iter()
            .any(|s| matches!(s, asset_io::Segment::Exif { .. }));

        if has_exif {
            println!("\nFound EXIF segment");
            // Could extract and parse EXIF data here if needed
        } else {
            println!("\nNo EXIF found");
        }
    }

    // ========================================
    // PART 3: C2PA Integration
    // ========================================
    println!("\n--- Part 3: C2PA Integration ---");
    println!("Testing C2PA embeddable APIs...");

    // TODO: Add C2PA Builder/Reader usage here
    // This is where you'd test:
    // - c2pa::Builder for creating manifests
    // - c2pa::Reader for reading existing C2PA data
    // - How asset-io's segment API helps with JUMBF manipulation

    #[cfg(feature = "c2pa-test")]
    {
        test_c2pa_builder(&test_image)?;
        test_c2pa_reader(&test_image)?;
    }
    #[cfg(not(feature = "c2pa-test"))]
    {
        println!("C2PA testing disabled (enable with --features c2pa-test)");
        println!("\nWhat we could test:");
        println!("  1. Create C2PA manifest using asset-io metadata");
        println!("  2. Embed manifest and verify with c2pa::Builder");
        println!("  3. Read C2PA data and cross-check with asset-io segments");
        println!("  4. Update XMP/EXIF alongside C2PA data");
    }

    // ========================================
    // PART 4: Modifications with asset-io
    // ========================================
    println!("\n--- Part 4: Modification Capabilities ---");
    println!("asset-io can help C2PA by:");
    println!("  ✓ Reading existing metadata for claims");
    println!("  ✓ Navigating file segments for JUMBF insertion");
    println!("  ✓ Updating XMP after C2PA signing");
    println!("  ✓ Extracting thumbnails for manifest ingredients");

    #[cfg(feature = "xmp")]
    {
        println!("\nExample XMP update workflow:");
        if let Some(xmp_data) = asset.xmp()? {
            let xmp_str = String::from_utf8_lossy(&xmp_data);

            // Simulate adding provenance info
            use asset_io::xmp::add_key;
            let updated = add_key(&xmp_str, "dc:provenance", "C2PA signed")?;
            println!("  Added dc:provenance to XMP");
            println!("  New XMP size: {} bytes", updated.len());
        }
    }

    println!("\n=== Integration Test Complete ===");
    Ok(())
}

#[cfg(feature = "c2pa-test")]
fn test_c2pa_builder(_image_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n  Testing c2pa::Builder...");

    // TODO: Use c2pa::Builder to:
    // 1. Create a manifest
    // 2. Add assertions (from asset-io metadata)
    // 3. Sign and embed

    println!("  (Builder test not yet implemented)");
    Ok(())
}

#[cfg(feature = "c2pa-test")]
fn test_c2pa_reader(_image_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n  Testing c2pa::Reader...");

    // TODO: Use c2pa::Reader to:
    // 1. Read existing C2PA data
    // 2. Compare with asset-io segment locations
    // 3. Validate manifest

    println!("  (Reader test not yet implemented)");
    Ok(())
}

/// Find a suitable test image
fn find_test_image() -> Result<String, Box<dyn std::error::Error>> {
    // Try to find test fixtures
    let candidates = vec![
        "tests/fixtures/Designer.jpeg",
        "tests/fixtures/FireflyTrain.jpg",
        "tests/fixtures/GreenCat.png",
        "tests/fixtures/P1000708.jpg",
        "tests/assets/test.jpg",
        "sample.jpg",
    ];

    for path in candidates {
        if std::path::Path::new(path).exists() {
            return Ok(path.to_string());
        }
    }

    Err("No test image found. Please provide a JPEG/PNG file in tests/fixtures/".into())
}
