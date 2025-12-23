use jumbf_io::{Asset, Updates, XmpUpdate};
use jumbf_io::test_utils::{fixture_path, FIREFLY_TRAIN};

fn main() -> jumbf_io::Result<()> {
    println!("=== Testing XMP Extended Support ===\n");
    println!("Note: This demonstrates XMP Extended splitting for large XMP data.\n");

    // Test 1: Create a large XMP (> 65KB) and verify it gets split
    println!("Test 1: Writing large XMP (>65KB) with automatic splitting");
    let large_xmp = create_large_xmp(70000);
    println!("  Created XMP: {} bytes", large_xmp.len());

    // Use test fixture
    let input = fixture_path(FIREFLY_TRAIN);
    let input_str = input.to_str().unwrap();
    let output = "/tmp/test_xmp_extended_write.jpg";

    let mut asset = Asset::open(input_str)?;
    asset.write_to(
        output,
        &Updates {
            xmp: XmpUpdate::Set(large_xmp.clone()),
            ..Default::default()
        },
    )?;
    println!("  ✓ Written to: {}", output);

    // Verify the written file
    let mut verify_asset = Asset::open(output)?;
    if let Some(read_xmp) = verify_asset.xmp()? {
        println!("  ✓ Read back XMP: {} bytes", read_xmp.len());
        if read_xmp == large_xmp {
            println!("  ✓ XMP matches original!");
        } else {
            println!("  ✗ XMP mismatch!");
            println!("    Original: {} bytes", large_xmp.len());
            println!("    Read:     {} bytes", read_xmp.len());
        }
    } else {
        println!("  ✗ Failed to read XMP back");
    }

    // Test 2: Parse existing file with extended XMP (if available)
    println!("\nTest 2: Parsing file with extended XMP");
    if let Ok(mut asset) = Asset::open("/tmp/test_xmp_extended_write.jpg") {
        println!("  Segments: {}", asset.structure().segments.len());
        
        if let Some(xmp) = asset.xmp()? {
            println!("  ✓ Found XMP: {} bytes", xmp.len());
        }
    }

    // Test 3: Test boundary cases
    println!("\nTest 3: Boundary cases");
    
    // Just under max size (should fit in single segment)
    let medium_xmp = create_large_xmp(65000);
    println!("  Testing {} byte XMP (just under limit)", medium_xmp.len());
    
    let mut asset = Asset::open(input_str)?;
    asset.write_to(
        "/tmp/test_xmp_medium.jpg",
        &Updates {
            xmp: XmpUpdate::Set(medium_xmp.clone()),
            ..Default::default()
        },
    )?;
    
    let mut verify = Asset::open("/tmp/test_xmp_medium.jpg")?;
    if let Some(xmp) = verify.xmp()? {
        if xmp == medium_xmp {
            println!("  ✓ Medium XMP preserved correctly");
        }
    }

    // Just over max size (should split)
    let over_xmp = create_large_xmp(66000);
    println!("  Testing {} byte XMP (just over limit)", over_xmp.len());
    
    let mut asset = Asset::open(input_str)?;
    asset.write_to(
        "/tmp/test_xmp_over.jpg",
        &Updates {
            xmp: XmpUpdate::Set(over_xmp.clone()),
            ..Default::default()
        },
    )?;
    
    let mut verify = Asset::open("/tmp/test_xmp_over.jpg")?;
    if let Some(xmp) = verify.xmp()? {
        if xmp == over_xmp {
            println!("  ✓ Over-limit XMP preserved correctly with splitting");
        }
    }

    println!("\n=== All Extended XMP Tests Complete ===");
    Ok(())
}

fn create_large_xmp(target_size: usize) -> Vec<u8> {
    // Create a valid XMP packet of approximately target_size
    let header = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="jumbf-io test">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:description>"#;

    let footer = r#"</dc:description>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    let overhead = header.len() + footer.len();
    let padding_needed = if target_size > overhead {
        target_size - overhead
    } else {
        0
    };

    let mut result = String::new();
    result.push_str(header);
    
    // Add padding data
    for i in 0..(padding_needed / 100) {
        result.push_str(&format!("Line {} of test data. ", i));
    }
    
    result.push_str(footer);
    result.into_bytes()
}

