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
        sign_with_bmff_hash(asset, builder, signer, output)
    } else {
        sign_with_data_hash(asset, builder, signer, output)
    }
}

/// Sign BMFF asset (HEIC, HEIF, M4A, AVIF) with BmffHash
fn sign_with_bmff_hash<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    signer: &dyn Signer,
    output: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== BmffHash Workflow ===");

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

    // Compute hash using c2pa-rs (handles V2 offset hashing)
    println!("🔢 Computing hash...");
    output.seek(SeekFrom::Start(0))?;
    bmff_hash.gen_hash_from_stream(output)?;
    println!("   ✅ Hash: {:02x?}...", &bmff_hash.hash().unwrap()[..8]);

    // Sign and update
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

    // Create dummy DataHash (will be replaced with real one)
    println!("📦 Creating DataHash...");
    let (dummy_dh, dummy_size) = create_dummy_data_hash()?;
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
    let real_dh = create_real_data_hash(&structure, hash, dummy_size)?;

    // Sign and update
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

/// Create dummy DataHash for size reservation
fn create_dummy_data_hash() -> Result<(DataHash, usize), Box<dyn std::error::Error>> {
    let mut dummy = DataHash::new("jumbf_manifest", "sha256");
    dummy.add_exclusion(HashRange::new(u32::MAX as u64, u32::MAX as u64));
    dummy.set_hash(vec![0; 32]);

    let size = serde_cbor::to_vec(&dummy)?.len();
    Ok((dummy, size))
}

/// Create real DataHash with actual exclusion range
fn create_real_data_hash(
    structure: &Structure,
    hash: Vec<u8>,
    target_size: usize,
) -> Result<DataHash, Box<dyn std::error::Error>> {
    let (offset, size) = asset_io::exclusion_range_for_segment(structure, SegmentKind::Jumbf)
        .ok_or("No JUMBF segment found")?;

    let mut data_hash = DataHash::new("jumbf_manifest", "sha256");
    data_hash.add_exclusion(HashRange::new(offset, size));
    data_hash.set_hash(hash);
    data_hash.pad_to_size(target_size)?;

    Ok(data_hash)
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

    // Load settings and signer
    let settings_str = std::fs::read_to_string("tests/fixtures/test_settings.json")?;
    Settings::from_string(&settings_str, "json")?;
    let signer = Settings::signer()?;

    // Open asset
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

    // Open output with read+write (keeps handle open throughout)
    let mut output_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output_path)?;

    // Sign asset (unified function handles both workflows)
    sign_asset_with_c2pa(&mut asset, &mut builder, signer.as_ref(), &mut output_file)?;

    println!("💾 Saved: {}", output_path);

    // Verify
    verify_asset(&output_path, mime_type)?;

    println!("\n✨ Success!");
    Ok(())
}
