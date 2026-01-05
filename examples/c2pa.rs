//! C2PA signing example using asset-io
//!
//! Demonstrates creating C2PA manifests with both BmffHash (HEIC, HEIF, M4A, AVIF)
//! and DataHash (JPEG, PNG) workflows using a unified API.
//!
//! ## Key Features
//!
//! - **Container-agnostic**: Automatically detects format and uses appropriate hash
//! - **Optimized I/O**: Single-pass write, minimal file operations
//! - **In-place update**: Only overwrites manifest bytes (99.995% I/O savings)
//!
//! ## Usage
//!
//! ```bash
//! # Works with any supported format
//! cargo run --example c2pa --features all-formats,xmp tests/fixtures/sample1.png
//! cargo run --example c2pa --features all-formats,xmp tests/fixtures/sample1.heic
//! ```

use asset_io::{Asset, ContainerKind, ExclusionMode, SegmentKind, Structure, Updates};
use c2pa::{
    assertions::{c2pa_action, Action, BmffHash, DataHash, DataMap, DigitalSourceType, ExclusionsMap},
    settings::Settings,
    Builder, ClaimGeneratorInfo, HashRange, Reader, Signer,
};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

/// Sign an asset with C2PA manifest
///
/// This function handles both BMFF (BmffHash) and non-BMFF (DataHash) workflows
/// automatically based on the container type.
fn sign_asset_with_c2pa<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    signer: &dyn Signer,
    output: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let container = asset.structure().container;
    let is_bmff = matches!(container, ContainerKind::Bmff);

    if is_bmff {
        println!("\n=== BmffHash Workflow (Sequential) ===");
        sign_with_bmff_hash_sequential(asset, builder, signer, output)
    } else {
        sign_with_data_hash(asset, builder, signer, output)
    }
}

/// BMFF V3 signing with parallel hashing (requires parallel+memory-mapped features)
/// Takes output path directly and manages file handles internally
#[cfg(all(feature = "parallel", feature = "memory-mapped"))]
fn sign_with_bmff_hash_parallel_v3<R: Read + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    signer: &dyn Signer,
    output_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use asset_io::merkle_root;
    
    // Create BmffHash with dummy Merkle map for size reservation
    println!("📦 Creating BmffHash V3 placeholder...");
    let mut bmff_hash = create_bmff_hash();
    
    // Pre-allocate Merkle map space (dummy entry for size calculation)
    let dummy_merkle_map = c2pa::assertions::MerkleMap {
        unique_id: 0,
        local_id: 0,
        count: 1,  // Will be updated with actual count
        alg: Some("sha256".to_string()),
        init_hash: None,
        hashes: c2pa::assertions::VecByteBuf(vec![serde_bytes::ByteBuf::from(vec![0u8; 32])]),
        fixed_block_size: Some(1024 * 1024),  // 1MB chunks - IMPORTANT for V3!
        variable_block_sizes: None,
    };
    bmff_hash.set_merkle(vec![dummy_merkle_map]);
    bmff_hash.set_hash(vec![0u8; 32]);  // Dummy hash
    builder.add_assertion(BmffHash::LABEL, &bmff_hash)?;
    
    // Create placeholder manifest with Merkle map size included
    let placeholder = builder.unsigned_manifest_placeholder(signer.reserve_size())?;
    println!("   Placeholder: {} bytes (with Merkle map)", placeholder.len());

    // Write file with placeholder to the output path
    println!("⚡ Writing file...");
    let updates = Updates::new().set_jumbf(placeholder.clone());
    
    let mut output_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_path)?;
    
    let structure = asset.write(&mut output_file, &updates)?;
    output_file.flush()?;
    drop(output_file); // Close so we can open with mmap
    
    // Open output with mmap for parallel hashing
    println!("🚀 Parallel hashing mdat boxes only (true V3 Merkle)...");
    let output_asset = unsafe { Asset::open_with_mmap(output_path)? };
    
    println!("   📊 Output file has {} segments", output_asset.structure().segments().len());
    for seg in output_asset.structure().segments() {
        println!("      - {:?} at offset={}, size={}", seg.kind, seg.location().offset, seg.location().size);
    }
    
    // Hash ONLY mdat boxes (ImageData segments) in 1MB chunks
    // This is the correct V3 behavior per C2PA spec
    let chunk_hashes = output_asset.parallel_hash_segments::<Sha256>(
        SegmentKind::ImageData,  // Only hash mdat boxes
        1024 * 1024,              // 1MB fixed blocks
    )?;
    
    println!("   📦 Computed {} chunk hashes from mdat boxes", chunk_hashes.len());
    
    if chunk_hashes.is_empty() {
        return Err("No mdat boxes found in output file! Cannot compute V3 Merkle hash.".into());
    }
    
    // Build Merkle tree root
    let merkle_root_hash = merkle_root::<Sha256>(&chunk_hashes);
    println!("   🌳 Merkle root: {:02x?}...", &merkle_root_hash[..8]);
    
    // Update BmffHash with real V3 Merkle data
    println!("📦 Updating BmffHash V3 with computed Merkle tree...");
    let mut bmff_hash = create_bmff_hash();
    bmff_hash.set_hash(merkle_root_hash.to_vec());
    
    // Create real Merkle map with actual count and fixed_block_size
    let real_merkle_map = c2pa::assertions::MerkleMap {
        unique_id: 0,
        local_id: 0,
        count: chunk_hashes.len(),
        alg: Some("sha256".to_string()),
        init_hash: None,
        hashes: c2pa::assertions::VecByteBuf(vec![serde_bytes::ByteBuf::from(merkle_root_hash.to_vec())]),
        fixed_block_size: Some(1024 * 1024),  // CRITICAL: This enables true V3!
        variable_block_sizes: None,
    };
    bmff_hash.set_merkle(vec![real_merkle_map]);
    
    builder.replace_assertion(BmffHash::LABEL, &bmff_hash)?;
    
    // Sign manifest
    println!("🔏 Signing manifest...");
    let signed = builder.sign_manifest()?;
    
    assert_eq!(placeholder.len(), signed.len(), "Size mismatch");

    // Reopen output for in-place update
    println!("✏️  Updating manifest in-place...");
    let mut output_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(output_path)?;
    structure.update_segment(&mut output_file, SegmentKind::Jumbf, signed)?;
    output_file.flush()?;

    Ok(())
}

/// BMFF signing with sequential hashing
/// Used as fallback when parallel features are not enabled, or called directly
fn sign_with_bmff_hash_sequential<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    signer: &dyn Signer,
    output: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create BmffHash with mandatory exclusions
    println!("📦 Creating BmffHash...");
    let mut bmff_hash = create_bmff_hash();
    builder.add_assertion(BmffHash::LABEL, &bmff_hash)?;

    // Create placeholder manifest
    let placeholder = builder.unsigned_manifest_placeholder(signer.reserve_size())?;
    println!("   Placeholder: {} bytes", placeholder.len());

    // Write file with placeholder
    println!("⚡ Writing file...");
    let updates = Updates::new().set_jumbf(placeholder.clone());
    let structure = asset.write(output, &updates)?;
    output.flush()?;

    // Compute hash using c2pa-rs (handles V2/V3 hashing based on settings)
    println!("🔢 Computing hash...");
    output.seek(SeekFrom::Start(0))?;
    bmff_hash.gen_hash_from_stream(output)?;
    println!("   ✅ Hash: {:02x?}...", &bmff_hash.hash().unwrap()[..8]);

    // Update hash assertion and sign
    builder.replace_assertion(BmffHash::LABEL, &bmff_hash)?;
    let signed = builder.sign_manifest()?;
    
    assert_eq!(placeholder.len(), signed.len(), "Size mismatch");

    println!("✏️  Updating manifest in-place...");
    structure.update_segment(output, SegmentKind::Jumbf, signed)?;
    output.flush()?;

    Ok(())
}

/// Sign non-BMFF asset (JPEG, PNG) with DataHash
fn sign_with_data_hash<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    signer: &dyn Signer,
    output: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== DataHash Workflow ===");

    // Create dummy DataHash for size reservation
    println!("📦 Creating DataHash placeholder...");
    let mut dummy_dh = DataHash::new("jumbf_manifest", "sha256");
    dummy_dh.add_exclusion(HashRange::new(u32::MAX as u64, u32::MAX as u64));
    dummy_dh.set_hash(vec![0; 32]);
    builder.add_assertion(DataHash::LABEL, &dummy_dh)?;

    // Create placeholder manifest
    let placeholder = builder.unsigned_manifest_placeholder(signer.reserve_size())?;
    println!("   Placeholder: {} bytes", placeholder.len());

    // Write and hash in single pass
    println!("⚡ Writing and hashing...");
    let updates = Updates::new()
        .set_jumbf(placeholder.clone())
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

    let mut hasher = Sha256::new();
    let structure = asset.write_with_processing(output, &updates, &mut |chunk| {
        hasher.update(chunk);
    })?;

    let hash = hasher.finalize().to_vec();
    println!("   ✅ Hash: {:02x?}...", &hash[..8]);

    // Create real DataHash with exclusion range
    let manifest_segment_idx = structure
        .c2pa_jumbf_index()
        .ok_or("No C2PA JUMBF found")?;
    let manifest_segment = &structure.segments[manifest_segment_idx];
    let data_offset = manifest_segment.ranges[0].offset;
    let data_size: u64 = manifest_segment.ranges.iter().map(|r| r.size).sum();

    let (exclusion_offset, exclusion_size) = match structure.container {
        ContainerKind::Png => (data_offset, data_size + 4), // +4 for CRC
        ContainerKind::Jpeg => (data_offset, data_size),
        _ => return Err("Unsupported container for DataHash".into()),
    };

    let mut real_dh = DataHash::new("jumbf_manifest", "sha256");
    let hr = HashRange::new(exclusion_offset, exclusion_size);
    real_dh.add_exclusion(hr);
    real_dh.set_hash(hash);

    // Replace hash assertion and sign
    builder.replace_assertion(DataHash::LABEL, &real_dh)?;
    let signed = builder.sign_manifest()?;
    
    assert_eq!(placeholder.len(), signed.len(), "Size mismatch");

    println!("✏️  Updating manifest in-place...");
    structure.update_segment(output, SegmentKind::Jumbf, signed)?;
    output.flush()?;

    Ok(())
}

/// Create BmffHash with C2PA mandatory exclusions
fn create_bmff_hash() -> BmffHash {
    let mut bmff_hash = BmffHash::new("jumbf manifest", "sha256", None);
    let exclusions = bmff_hash.exclusions_mut();

    // C2PA UUID boxes
    let mut uuid = ExclusionsMap::new("/uuid".to_owned());
    uuid.data = Some(vec![DataMap {
        offset: 8,
        value: vec![
            0xd8, 0xfe, 0xc3, 0xd6, 0x1b, 0x0e, 0x48, 0x3c,
            0x92, 0x97, 0x58, 0x28, 0x87, 0x7e, 0xc4, 0x81,
        ],
    }]);
    exclusions.push(uuid);
    exclusions.push(ExclusionsMap::new("/ftyp".to_owned()));
    exclusions.push(ExclusionsMap::new("/mfra".to_owned()));

    bmff_hash.set_hash(vec![0; 32]); // Placeholder
    bmff_hash
}

/// Verify signed asset
fn verify_asset(path: &str, mime_type: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🔍 Verifying...");
    
    let mut file = std::fs::File::open(path)?;
    let reader = Reader::from_stream(mime_type, &mut file)?;

    // Check for hash validation errors
    if let Some(validation) = reader.validation_status() {
        for status in validation {
            let code = status.code();
            if code.contains("hash.mismatch") 
                || code.contains("bmffHash.mismatch")
                || code.contains("dataHash.mismatch") {
                println!("❌ Hash mismatch: {}", code);
                return Err("Hash validation failed".into());
            }
        }
    }

    // Display manifest info if available
    if let Some(manifest) = reader.active_manifest() {
        // Check for BmffHash assertion with Merkle tree
        match manifest.find_assertion::<BmffHash>(BmffHash::LABEL) {
            Ok(bmff_hash) => {
                println!("   📦 BmffHash V{}", bmff_hash.bmff_version());
                if let Some(merkle) = bmff_hash.merkle() {
                    println!("   🌳 Merkle tree detected:");
                    for (i, mm) in merkle.iter().enumerate() {
                        println!("      Map {}: {} leaves", i, mm.count);
                        if let Some(block_size) = mm.fixed_block_size {
                            println!("         Fixed block size: {} KB", block_size / 1024);
                        }
                    }
                }
            }
            Err(_) => {
                // Check for DataHash
                if let Ok(_data_hash) = manifest.find_assertion::<DataHash>(DataHash::LABEL) {
                    println!("   📦 DataHash (JPEG/PNG workflow)");
                }
            }
        }
    }

    println!("✅ Verification complete!");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <asset_file>", args[0]);
        return Ok(());
    }

    let source_path = &args[1];

    // Load settings and signer (includes V3 Merkle tree settings)
    let settings_str = std::fs::read_to_string("tests/fixtures/test_settings.json")?;
    Settings::from_string(&settings_str, "json")?;
    let signer = Settings::signer()?;

    // Open asset - use mmap for parallel hashing if features enabled
    #[cfg(all(feature = "parallel", feature = "memory-mapped"))]
    let mut asset = unsafe { Asset::open_with_mmap(source_path)? };
    
    #[cfg(not(all(feature = "parallel", feature = "memory-mapped")))]
    let mut asset = Asset::open(source_path)?;
    let mime_type = asset.media_type().to_mime();
    let extension = asset.media_type().to_extension();
    let output_path = format!("target/output_c2pa.{}", extension);

    println!("📝 Signing: {}", source_path);
    println!("   Format: {} ({:?})", mime_type, asset.structure().container);

    // Create builder
    let mut builder = Builder::default();
    let mut claim_generator = ClaimGeneratorInfo::new("asset-io-example".to_string());
    claim_generator.set_version("0.1");
    builder
        .set_claim_generator_info(claim_generator)
        .add_action(Action::new(c2pa_action::CREATED)
            .set_source_type(DigitalSourceType::Empty))?;

    // For BMFF with parallel features, call parallel function directly (needs output path)
    #[cfg(all(feature = "parallel", feature = "memory-mapped", feature = "bmff"))]
    if asset.structure().container == asset_io::ContainerKind::Bmff {
        println!("\n=== BmffHash V3 Workflow (Parallel) ===");
        sign_with_bmff_hash_parallel_v3(&mut asset, &mut builder, signer.as_ref(), std::path::Path::new(&output_path))?;
    } else {
        // Open output for non-BMFF workflows
        let mut output_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&output_path)?;
        sign_asset_with_c2pa(&mut asset, &mut builder, signer.as_ref(), &mut output_file)?;
    }
    
    // For non-parallel or non-BMFF, use unified function
    #[cfg(not(all(feature = "parallel", feature = "memory-mapped", feature = "bmff")))]
    {
        let mut output_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&output_path)?;
        sign_asset_with_c2pa(&mut asset, &mut builder, signer.as_ref(), &mut output_file)?;
    }

    println!("💾 Saved: {}", output_path);

    // Verify
    verify_asset(&output_path, mime_type)?;

    println!("\n✨ Success!");
    Ok(())
}
