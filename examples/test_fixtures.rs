use asset_io::{Asset, Updates, XmpUpdate, JumbfUpdate};
use std::fs;

// Always use test_utils now
use asset_io::test_utils::{fixture_path, list_fixtures, is_embedded};

fn main() -> asset_io::Result<()> {
    println!("=== Testing All Fixtures ===\n");
    
    // Get all available fixtures (from defined + extended directory if set)
    let fixtures = list_fixtures()?;
    
    println!("Found {} fixture(s)", fixtures.len());
    if let Ok(extended_dir) = std::env::var("JUMBF_TEST_FIXTURES") {
        println!("Using extended fixtures from: {}", extended_dir);
    }
    println!();

    let mut results = Vec::new();

    for fixture in &fixtures {
        let fixture_path = fixture_path(fixture);
        
        // Skip if doesn't exist
        if !fixture_path.exists() {
            println!("Skipping {:<25} (not found)", fixture);
            continue;
        }
        
        let fixture_path_str = fixture_path.to_str().unwrap();
        
        // Show if embedded
        let embedded_marker = if is_embedded(fixture) { "📦" } else { "📁" };
        print!("Testing {:<25} {} ", fixture, embedded_marker);
        
        match test_fixture(fixture_path_str) {
            Ok(info) => {
                println!("✓");
                results.push((fixture.clone(), Ok(info)));
            }
            Err(e) => {
                println!("✗ {}", e);
                results.push((fixture.clone(), Err(e)));
            }
        }
    }

    // Summary
    println!("\n=== Summary ===");
    let passed = results.iter().filter(|(_, r)| r.is_ok()).count();
    let failed = results.iter().filter(|(_, r)| r.is_err()).count();
    
    println!("Passed: {}/{}", passed, results.len());
    if failed > 0 {
        println!("Failed: {}", failed);
    }

    // Detailed results
    println!("\n=== Detailed Results ===");
    for (filename, result) in &results {
        match result {
            Ok(info) => {
                println!("\n{}", filename);
                println!("  Segments: {}", info.segments);
                println!("  XMP:      {} bytes", 
                    info.xmp_size.map(|s| s.to_string()).unwrap_or("None".to_string()));
                println!("  JUMBF:    {} bytes", 
                    info.jumbf_size.map(|s| s.to_string()).unwrap_or("None".to_string()));
                println!("  Size:     {} bytes", info.file_size);
                
                // Test operations
                println!("  Copy:     {}", if info.copy_works { "✓" } else { "✗" });
                println!("  XMP Mod:  {}", if info.xmp_modify_works { "✓" } else { "✗" });
                println!("  JUMBF Rm: {}", if info.jumbf_remove_works { "✓" } else { "✗" });
            }
            Err(e) => {
                println!("\n{}", filename);
                println!("  Error: {}", e);
            }
        }
    }

    if failed > 0 {
        println!("\n⚠ {} test(s) failed", failed);
        std::process::exit(1);
    } else {
        println!("\n✓ All tests passed!");
    }

    Ok(())
}

#[derive(Debug)]
struct FixtureInfo {
    segments: usize,
    xmp_size: Option<usize>,
    jumbf_size: Option<usize>,
    file_size: u64,
    copy_works: bool,
    xmp_modify_works: bool,
    jumbf_remove_works: bool,
}

fn test_fixture(path: &str) -> asset_io::Result<FixtureInfo> {
    // Parse the file
    let mut asset = Asset::open(path)?;
    
    let segments = asset.structure().segments.len();
    let file_size = fs::metadata(path)?.len();
    
    // Read metadata
    let xmp_size = asset.xmp()?.map(|x| x.len());
    let jumbf_size = asset.jumbf()?.map(|j| j.len());
    
    // Test 1: Copy unchanged
    let copy_path = "/tmp/test_copy.jpg";
    let mut asset = Asset::open(path)?;
    asset.write_to(copy_path, &Updates::default())?;
    
    let mut verify = Asset::open(copy_path)?;
    let copy_xmp = verify.xmp()?.map(|x| x.len());
    let copy_jumbf = verify.jumbf()?.map(|j| j.len());
    let copy_works = copy_xmp == xmp_size && copy_jumbf == jumbf_size;
    
    // Verify it's a valid JPEG
    std::process::Command::new("identify")
        .arg(copy_path)
        .output()
        .map_err(|e| asset_io::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("identify failed: {}", e)
        )))?;
    
    // Test 2: Modify XMP
    let xmp_mod_path = "/tmp/test_xmp_mod.jpg";
    let new_xmp = b"<test>Modified XMP</test>".to_vec();
    let mut asset = Asset::open(path)?;
    asset.write_to(xmp_mod_path, &Updates {
        xmp: XmpUpdate::Set(new_xmp.clone()),
        ..Default::default()
    })?;
    
    let mut verify = Asset::open(xmp_mod_path)?;
    let mod_xmp = verify.xmp()?;
    let xmp_modify_works = mod_xmp.as_ref().map(|x| x.as_slice()) == Some(new_xmp.as_slice());
    
    // Verify it's a valid JPEG
    std::process::Command::new("identify")
        .arg(xmp_mod_path)
        .output()
        .map_err(|e| asset_io::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("identify failed: {}", e)
        )))?;
    
    // Test 3: Remove JUMBF
    let jumbf_rm_path = "/tmp/test_jumbf_rm.jpg";
    let mut asset = Asset::open(path)?;
    asset.write_to(jumbf_rm_path, &Updates {
        jumbf: JumbfUpdate::Remove,
        ..Default::default()
    })?;
    
    let mut verify = Asset::open(jumbf_rm_path)?;
    let has_jumbf = verify.jumbf()?.is_some();
    let jumbf_remove_works = !has_jumbf;
    
    // Verify it's a valid JPEG
    std::process::Command::new("identify")
        .arg(jumbf_rm_path)
        .output()
        .map_err(|e| asset_io::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("identify failed: {}", e)
        )))?;
    
    // Cleanup
    let _ = fs::remove_file(copy_path);
    let _ = fs::remove_file(xmp_mod_path);
    let _ = fs::remove_file(jumbf_rm_path);
    
    Ok(FixtureInfo {
        segments,
        xmp_size,
        jumbf_size,
        file_size,
        copy_works,
        xmp_modify_works,
        jumbf_remove_works,
    })
}
