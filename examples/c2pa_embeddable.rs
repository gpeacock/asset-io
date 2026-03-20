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
//! # Sign any supported format (streaming from file)
//! cargo run --example c2pa_embeddable --features all-formats,xmp <input> <output>
//!
//! # Use memory-mapped I/O for large files (zero-copy, faster)
//! cargo run --example c2pa_embeddable --features all-formats,xmp,memory-mapped -- --mmap <input> <output>
//!
//! # Debug hash mismatches (prints JUMBF segment ranges, exclusion ranges, capacity)
//! cargo run --example c2pa_embeddable --features all-formats,xmp -- --debug <input> <output>
//! ```

use asset_io::{Asset, ProcessChunk, SegmentKind};
use c2pa::{
    assertions::{c2pa_action, Action, BmffHash, DataHash},
    Builder, ClaimGeneratorInfo, HashRange, Reader, Settings, ValidationState,
};
use sha2::{Digest, Sha256};
use std::io::{BufReader, Write};
use std::time::Instant;

/// Buffer size for read/write (64KB vs default 8KB)
const BUF_SIZE: usize = 64 * 1024;

/// Print timing when RUST_PROFILE=1
fn profile(label: &str, start: Instant) {
    if std::env::var("RUST_PROFILE").is_ok() {
        eprintln!("  {}: {:?}", label, start.elapsed());
        use std::io::Write;
        let _ = std::io::stderr().flush();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let use_mmap = args.iter().any(|a| a == "--mmap");
    let debug = args.iter().any(|a| a == "--debug");
    let files: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();

    if files.len() < 2 {
        eprintln!(
            "Usage: {} [--mmap] [--debug] <input_file> <output_file>",
            args[0]
        );
        eprintln!("\nExample:");
        eprintln!("  {} photo.jpg signed.jpg", args[0]);
        eprintln!("  {} video.mp4 signed.mp4", args[0]);
        eprintln!("  {} image.heic signed.heic", args[0]);
        #[cfg(feature = "memory-mapped")]
        eprintln!(
            "  {} --mmap large_video.mov signed.mov  # zero-copy for large files",
            args[0]
        );
        eprintln!(
            "  {} --debug video.mov out.mov  # debug hash mismatches (BMFF)",
            args[0]
        );
        return Ok(());
    }

    let input_path = files[0];
    let output_path = files[1];

    println!("🚀 C2PA Embeddable API Example");
    println!("   Input:  {}", input_path);
    println!("   Output: {}", output_path);
    if use_mmap {
        println!("   Mode:   memory-mapped (zero-copy)");
    } else {
        println!("   Mode:   streaming");
    }
    if std::env::var("RUST_PROFILE").is_ok() {
        eprintln!("\n⏱️  Profile (RUST_PROFILE=1):");
    }
    println!();

    // Load settings and create context
    let settings_str = std::fs::read_to_string("tests/fixtures/test_settings.json")?;
    let settings = Settings::from_string(&settings_str, "json")?;

    // Create Builder with Context from Settings
    let context = c2pa::Context::new().with_settings(settings)?.into_shared();
    let mut builder = Builder::from_shared_context(&context);

    // Step 1: Open asset (streaming or memory-mapped)
    let t_open = Instant::now();
    let mut asset = if use_mmap {
        #[cfg(feature = "memory-mapped")]
        {
            println!("📂 Opening asset (memory-mapped)...");
            unsafe { Asset::open_with_mmap(input_path)? }
        }
        #[cfg(not(feature = "memory-mapped"))]
        {
            eprintln!("Warning: --mmap ignored (build without memory-mapped feature)");
            println!("📂 Opening asset (streaming)...");
            Asset::open(input_path)?
        }
    } else {
        println!("📂 Opening asset (streaming)...");
        Asset::open(input_path)?
    };
    profile("open", t_open);
    let native_format = asset.media_type().to_mime(); // "video/mp4", "image/jpeg", etc.
    let is_bmff = matches!(asset.structure().container, asset_io::ContainerKind::Bmff);
    println!(
        "   Format: {} ({})",
        native_format,
        if is_bmff { "BMFF" } else { "non-BMFF" }
    );

    // Set claim generator info
    let mut claim_generator = ClaimGeneratorInfo::new("asset-io-embeddable-example".to_string());
    claim_generator.set_version("0.1.0");
    builder.set_claim_generator_info(claim_generator);

    // Add a simple "created" action
    builder
        .add_action(Action::new(c2pa_action::CREATED).set_parameter("identifier", input_path)?)?;

    let t_sign_total = Instant::now();
    if builder.needs_placeholder(native_format) {
        // For BMFF: add BmffHash explicitly so we get BmffHash (like c2patool), not DataHash.
        // We use "application/c2pa" for placeholder composition so asset-io gets raw JUMBF
        // (asset-io adds the UUID box header itself; SDK's BMFF compose would double-wrap).
        let t_placeholder = Instant::now();
        let (structure, mut output_file, signed_jumbf) = if is_bmff {
            // BMFF: use placeholder() + hash_bmff_mdat_bytes for one-pass mdat hashing
            let ph_alg = "sha256";
            let mut placeholder_bmff = BmffHash::new("jumbf manifest", ph_alg, None);
            placeholder_bmff.set_default_exclusions();
            placeholder_bmff.add_place_holder_hash()?;
            let assertion_label = format!("{}.v3", BmffHash::LABEL);
            builder.add_assertion(&assertion_label, &placeholder_bmff)?;

            let mut placeholder_jumbf = builder.placeholder("application/c2pa")?;
            placeholder_jumbf.extend(std::iter::repeat(0u8).take(256 * 1024));

            let updates = asset_io::Updates::new()
                .set_jumbf(placeholder_jumbf.clone())
                .exclude_from_processing(
                    vec![SegmentKind::Jumbf],
                    asset_io::ExclusionMode::DataOnly,
                );

            let mut output_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(output_path)?;

            let mut processor = |chunk: &dyn ProcessChunk| {
                if let Some(id) = chunk.id() {
                    let _ = builder.hash_bmff_mdat_bytes(
                        id,
                        chunk.data(),
                        chunk.large_size().unwrap_or(false),
                    );
                }
            };
            let structure =
                asset.write_with_processing(&mut output_file, &updates, &mut processor)?;
            output_file.flush()?;
            profile("asset_write", t_placeholder);

            let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
            (structure, output_file, signed_jumbf)
        } else {
            // DataHash: single-pass write+hash using sign_data_hashed_embeddable
            let signer = Settings::signer()?;
            let placeholder_jumbf = builder.placeholder("application/c2pa")?;
            profile("placeholder", t_placeholder);

            let updates = asset_io::Updates::new()
                .set_jumbf(placeholder_jumbf.clone())
                .exclude_from_processing(
                    vec![SegmentKind::Jumbf],
                    asset_io::ExclusionMode::DataOnly,
                );

            let mut output_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(output_path)?;

            let t_write = Instant::now();
            let mut hasher = Sha256::new();
            let mut processor = |chunk: &dyn ProcessChunk| {
                hasher.update(chunk.data());
            };
            let structure =
                asset.write_with_processing(&mut output_file, &updates, &mut processor)?;
            output_file.flush()?;
            profile("asset_write", t_write);

            let (exclusion_offset, exclusion_size) = structure
                .exclusion_range_for_segment(SegmentKind::Jumbf)
                .ok_or("Failed to compute exclusion range for JUMBF segment")?;

            let mut data_hash = DataHash::new("jumbf_manifest", "sha256");
            data_hash.add_exclusion(HashRange::new(exclusion_offset, exclusion_size));
            data_hash.set_hash(hasher.finalize().to_vec());

            let signed_jumbf = builder.sign_data_hashed_embeddable(
                signer.as_ref(),
                &data_hash,
                "application/c2pa",
            )?;
            (structure, output_file, signed_jumbf)
        };

        if debug {
            println!("\n🔬 Debug: JUMBF segment and exclusion ranges");
            if let Some(idx) = structure.c2pa_jumbf_index() {
                let seg = &structure.segments()[idx];
                for (i, r) in seg.ranges.iter().enumerate() {
                    println!(
                        "   JUMBF range[{}]: offset={}, size={} (end={})",
                        i,
                        r.offset,
                        r.size,
                        r.offset + r.size
                    );
                }
                let total: u64 = seg.ranges.iter().map(|r| r.size).sum();
                println!("   Total capacity: {} bytes", total);
            }
            if let Some((ex_off, ex_sz)) = structure.exclusion_range_for_segment(SegmentKind::Jumbf)
            {
                println!(
                    "   Exclusion range: offset={}, size={} (end={})",
                    ex_off,
                    ex_sz,
                    ex_off + ex_sz
                );
            } else {
                println!("   Exclusion range: (none computed)");
            }
            println!("   Structure total_size: {}", structure.total_size);
            println!();
        }

        if debug {
            let capacity = structure
                .c2pa_jumbf_index()
                .map(|idx| {
                    let seg = &structure.segments()[idx];
                    let n = if is_bmff { 1 } else { seg.ranges.len() };
                    seg.ranges[..n].iter().map(|r| r.size).sum::<u64>()
                })
                .unwrap_or(0);
            println!(
                "🔬 Debug: signed_jumbf len={}, capacity={}, fits={}",
                signed_jumbf.len(),
                capacity,
                signed_jumbf.len() as u64 <= capacity
            );
        }

        // update the manifest in place
        structure.update_segment(&mut output_file, SegmentKind::Jumbf, signed_jumbf)?;
        output_file.flush()?;
    } else {
        println!("BoxHash workflow (no placeholder needed)");
        // BoxHash workflow (no placeholder needed)
        // Buffered read for hash from source
        let source_file = std::fs::File::open(input_path)?;
        let mut buf_reader = BufReader::with_capacity(BUF_SIZE, source_file);
        builder.update_hash_from_stream(native_format, &mut buf_reader)?;

        // sign the manifest
        let signed_jumbf = builder.sign_embeddable("application/c2pa")?;

        // update the manifest in place (read+write+seek required for BMFF chunk offset adjustment)
        let updates = asset_io::Updates::new().set_jumbf(signed_jumbf);
        let mut output_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(output_path)?;
        asset.write(&mut output_file, &updates)?;
        output_file.flush()?;
    }

    let sign_duration = t_sign_total.elapsed();
    println!("💾 Saved: {}", output_path);
    println!("   ⏱️  Signing: {:?}", sign_duration);

    // Verify the signature
    println!("🔍 Verifying signature...");
    let t_verify = Instant::now();
    let mut verify_file = std::fs::File::open(output_path)?;
    match Reader::from_stream(native_format, &mut verify_file) {
        Ok(reader) => {
            let state = reader.validation_state();
            match state {
                ValidationState::Trusted => {
                    println!("   ✅ Trusted (chain verified to a trusted root)");
                }
                ValidationState::Valid => {
                    println!("   ✅ Valid (cryptographic integrity verified)");
                }
                ValidationState::Invalid => {
                    println!("   ❌ Invalid manifest store");
                    if let Some(results) = reader.validation_results() {
                        if let Some(active) = results.active_manifest() {
                            for failure in active.failure() {
                                println!(
                                    "      • {}: {}",
                                    failure.code(),
                                    failure.explanation().unwrap_or("")
                                );
                            }
                        }
                    }
                }
            }

            // Show manifest info regardless of trust level
            if let Some(manifest) = reader.active_manifest() {
                println!(
                    "   📋 Manifest label: {}",
                    manifest.label().unwrap_or("unknown")
                );
                if let Some(title) = manifest.title() {
                    println!("   📝 Title: {}", title);
                }

                if manifest
                    .find_assertion::<c2pa::assertions::BmffHash>(c2pa::assertions::BmffHash::LABEL)
                    .is_ok()
                {
                    println!("   🔐 Hard binding: BmffHash");
                } else if manifest
                    .find_assertion::<c2pa::assertions::DataHash>(c2pa::assertions::DataHash::LABEL)
                    .is_ok()
                {
                    println!("   🔐 Hard binding: DataHash");
                } else if manifest
                    .find_assertion::<c2pa::assertions::BoxHash>(c2pa::assertions::BoxHash::LABEL)
                    .is_ok()
                {
                    println!("   🔐 Hard binding: BoxHash");
                }
            }
        }
        Err(e) => {
            println!("   ❌ Failed to read manifest: {}", e);
        }
    }
    println!("   ⏱️  Verifying: {:?}", t_verify.elapsed());

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
