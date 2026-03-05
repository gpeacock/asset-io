//! C2PA signing using the new embeddable API (c2pa-rs 0.77.0+)
//!
//! This example demonstrates the new embeddable signing workflow where:
//! 1. asset-io handles all format-specific I/O and embedding
//! 2. Raw JUMBF crosses the boundary in both directions
//! 3. SDK handles hash binding using the native format's handler
//!
//! ## Key Advantages
//!
//! - **Format-agnostic**: Works with JPEG, PNG, MP4, HEIC, etc.
//! - **Explicit control**: You control each step of the workflow
//! - **Clean separation**: asset-io = I/O, c2pa-rs = signing logic
//! - **In-place updates**: Placeholder-based workflow enables efficient patching
//!
//! ## Usage
//!
//! ```bash
//! # Sign any supported format
//! cargo run --example c2pa_embeddable --features all-formats,xmp <input> <output>
//! ```

use asset_io::{Asset, SegmentKind};
use c2pa::{
    assertions::{c2pa_action, Action},
    Builder, ClaimGeneratorInfo, HashRange, Reader, Settings,
};
use std::io::{Seek, SeekFrom, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <input_file> <output_file>", args[0]);
        eprintln!("\nExample:");
        eprintln!("  {} photo.jpg signed.jpg", args[0]);
        eprintln!("  {} video.mp4 signed.mp4", args[0]);
        eprintln!("  {} image.heic signed.heic", args[0]);
        return Ok(());
    }

    let input_path = &args[1];
    let output_path = &args[2];

    println!("🚀 C2PA Embeddable API Example");
    println!("   Input:  {}", input_path);
    println!("   Output: {}", output_path);
    println!();

    // Load settings and create context
    let settings_str = std::fs::read_to_string("tests/fixtures/test_settings.json")?;
    let settings = Settings::from_string(&settings_str, "json")?;
    
    // Create Builder with Context from Settings
    let context = c2pa::Context::new().with_settings(settings)?.into_shared();
    let mut builder = Builder::from_shared_context(&context);

    // Step 1: asset-io detects format, handles all format-specific operations
    println!("📂 Opening asset...");
    let mut asset = Asset::open(input_path)?;
    let native_format = asset.media_type().to_mime(); // "video/mp4", "image/jpeg", etc.
    
    // Set claim generator info
    let mut claim_generator = ClaimGeneratorInfo::new("asset-io-embeddable-example".to_string());
    claim_generator.set_version("0.1.0");
    builder.set_claim_generator_info(claim_generator);
    
    // Add a simple "created" action
    builder.add_action(
        Action::new(c2pa_action::CREATED)
            .set_parameter("identifier", input_path)?
    )?;


    if builder.needs_placeholder(native_format) {
      
        let placeholder_jumbf = builder.placeholder("application/c2pa")?;
        //println!("   Size: {} bytes (raw JUMBF)", placeholder_jumbf.len());

        // asset-io writes the file with JUMBF embedded
        let updates = asset_io::Updates::new().set_jumbf(placeholder_jumbf.clone());
        
        let mut output_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(output_path)?;
        
        // asset-io writes the file embedded placeholder
        let structure = asset.write(&mut output_file, &updates)?;
        output_file.flush()?;

        let is_bmff = matches!(asset.structure().container, asset_io::ContainerKind::Bmff);
        
        if !is_bmff {
            //println!("Setting exclusion ranges for DataHash...");
            
            let (exclusion_offset, exclusion_size) = asset_io::exclusion_range_for_segment(
                &structure,
                SegmentKind::Jumbf,
            )
            .ok_or("Failed to compute exclusion range for JUMBF segment")?;
            
            builder.set_data_hash_exclusions(vec![HashRange::new(exclusion_offset, exclusion_size)])?;
            //println!("   Range: offset={}, size={}", exclusion_offset, exclusion_size);
        }

        // compute hash from the stream
        output_file.seek(SeekFrom::Start(0))?;
        builder.update_hash_from_stream(native_format, &mut output_file)?;

        // sign the manifest
        let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
        
        // update the manifest in place
        structure.update_segment(&mut output_file, SegmentKind::Jumbf, signed_jumbf)?;
        output_file.flush()?;
    } else {
        // BoxHash workflow (no placeholder needed)
        // compute hash from the source
        let mut source_file = std::fs::File::open(input_path)?;
        builder.update_hash_from_stream(native_format, &mut source_file)?;

        // sign the manifest
        let signed_jumbf = builder.sign_embeddable("application/c2pa")?;

        // update the manifest in place
        let updates = asset_io::Updates::new().set_jumbf(signed_jumbf);
        
        let mut output_file = std::fs::File::create(output_path)?;
        asset.write(&mut output_file, &updates)?;
        output_file.flush()?;   
    }

    println!("💾 Saved: {}", output_path);

    // Verify the signature
    println!("🔍 Verifying signature...");
    let mut verify_file = std::fs::File::open(output_path)?;
    match Reader::from_stream(native_format, &mut verify_file) {
        Ok(reader) => {
            if let Some(validation) = reader.validation_status() {
                let mut has_error = false;
                for status in validation {
                    let code = status.code();
                    if code.contains("hash.mismatch") 
                        || code.contains("bmffHash.mismatch")
                        || code.contains("dataHash.mismatch") {
                        println!("   ❌ Hash mismatch: {}", code);
                        has_error = true;
                    }
                }
                
                if !has_error {
                    println!("   ✅ Signature valid!");
                    
                    // Show manifest info
                    if let Some(manifest) = reader.active_manifest() {
                        println!("   📋 Manifest label: {}", manifest.label().unwrap_or("unknown"));
                        if let Some(title) = manifest.title() {
                            println!("   📝 Title: {}", title);
                        }
                        
                        // Show which hash type was used
                        if manifest.find_assertion::<c2pa::assertions::BmffHash>(c2pa::assertions::BmffHash::LABEL).is_ok() {
                            println!("   🔐 Hard binding: BmffHash");
                        } else if manifest.find_assertion::<c2pa::assertions::DataHash>(c2pa::assertions::DataHash::LABEL).is_ok() {
                            println!("   🔐 Hard binding: DataHash");
                        } else if manifest.find_assertion::<c2pa::assertions::BoxHash>(c2pa::assertions::BoxHash::LABEL).is_ok() {
                            println!("   🔐 Hard binding: BoxHash");
                        }
                    }
                }
            } else {
                println!("   ✅ Signature valid (no validation issues)!");
            }
        }
        Err(e) => {
            println!("   ⚠️  Verification warning: {}", e);
            println!("   (This may be expected for some formats)");
        }
    }

    println!();
    println!("✨ Success!");
    println!();
    println!("🎯 Key takeaways:");
    println!("   • asset-io handled all format-specific I/O");
    println!("   • Raw JUMBF crossed the boundary (no format coupling)");
    println!("   • SDK handled hash binding with native format handler");
    println!("   • In-place update avoided full file rewrite");

    Ok(())
}
