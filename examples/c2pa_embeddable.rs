//! C2PA signing using the embeddable API.
//!
//! Demonstrates the embeddable signing workflow where:
//! - asset-io handles all format-specific I/O and embedding
//! - Raw JUMBF bytes cross the boundary in both directions
//! - c2pa-rs handles hash binding using the native format's handler
//!
//! ## Usage
//!
//! ```bash
//! # Sign any supported format
//! cargo run --example c2pa_embeddable --features all-formats <input> <output>
//!
//! # Memory-mapped open for large inputs
//! cargo run --example c2pa_embeddable --features all-formats,memory-mapped -- --mmap <input> <output>
//! ```

use asset_io::{Asset, ProcessChunk, SegmentKind};
use c2pa::{
    assertions::{
        c2pa_action, Action, BmffHash, BoxHash, BoxMap, DataHash, ExclusionsMap, MerkleMap,
        SubsetMap, VecByteBuf,
    },
    Builder, ClaimGeneratorInfo, HashRange, Settings,
};
use serde_bytes::ByteBuf;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::time::Instant;

/// Extra bytes reserved beyond the placeholder JUMBF for BmffHash (Merkle maps grow with mdat).
const BMFF_PLACEHOLDER_PADDING: usize = 50 * 1024;

/// Extra bytes reserved for BoxHash (covers the per-segment BoxMap entries plus signing overhead).
const BOX_HASH_PADDING: usize = 16 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let use_mmap = args.iter().any(|a| a == "--mmap");
    let files: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();

    if files.len() < 2 {
        eprintln!("Usage: {} [--mmap] <input> <output>", args[0]);
        return Err("missing file operands".into());
    }

    let input_path = files[0];
    let output_path = files[1];

    // Load settings: prefer a settings file named by ASSET_IO_SETTINGS env var, otherwise
    // fall back to the default test settings.
    let settings_path = std::env::var("ASSET_IO_SETTINGS")
        .unwrap_or_else(|_| "tests/fixtures/test_settings.json".to_string());
    let settings_str = std::fs::read_to_string(&settings_path)?;
    let settings = Settings::from_string(&settings_str, "json")?;
    let context = c2pa::Context::new().with_settings(settings)?.into_shared();
    let mut builder = Builder::from_shared_context(&context);

    // Open the source asset.
    let mut asset = if use_mmap {
        #[cfg(feature = "memory-mapped")]
        {
            unsafe { Asset::open_with_mmap(input_path)? }
        }
        #[cfg(not(feature = "memory-mapped"))]
        {
            eprintln!("Warning: --mmap ignored (feature not enabled)");
            Asset::open(input_path)?
        }
    } else {
        Asset::open(input_path)?
    };

    let native_format = asset.media_type().to_mime();
    let is_bmff = matches!(asset.structure().container, asset_io::ContainerKind::Bmff);
    println!(
        "Format: {} ({})",
        native_format,
        if is_bmff { "BMFF" } else { "non-BMFF" }
    );

    // Claim metadata.
    let mut cgi = ClaimGeneratorInfo::new("asset-io-embeddable-example".to_string());
    cgi.set_version("0.1.0");
    builder.set_claim_generator_info(cgi);
    builder
        .add_action(Action::new(c2pa_action::CREATED).set_parameter("identifier", input_path)?)?;

    let t = Instant::now();

    if builder.needs_placeholder(&native_format) {
        let (structure, mut output_file, signed_jumbf) = if is_bmff {
            // BMFF path: single-pass V3 BmffHash computation.
            //
            // The write path emits chunks in this order for each non-excluded top-level box:
            //   1. process_offset → SimpleChunk(8-byte big-endian output offset)
            //   2. box content    → SimpleChunk(all box bytes, not in exclude_mode)
            //
            // For mdat the write path splits the box at byte 16:
            //   1. process_offset → SimpleChunk(8-byte offset)
            //   2. box[0..16]     → SimpleChunk(first 16 bytes, main hash region)
            //   3. box[16..]      → MdatChunk(remainder, excluded from main hash → Merkle)
            //
            // All SimpleChunks (both offset values and box content) feed main_hasher, which
            // exactly mirrors what the verifier computes: offset(8) || content for each box.
            // MdatChunks become Merkle leaves (one per contiguous streaming segment).
            let ph_alg = "sha256";
            let mut placeholder_bmff = BmffHash::new("jumbf manifest", ph_alg, None);
            placeholder_bmff.set_default_exclusions();
            placeholder_bmff.add_place_holder_hash()?;
            builder.add_assertion(&format!("{}.v3", BmffHash::LABEL), &placeholder_bmff)?;

            let mut placeholder_jumbf = builder.placeholder("application/c2pa")?;
            placeholder_jumbf.extend(std::iter::repeat(0u8).take(BMFF_PLACEHOLDER_PADDING));

            let updates = asset_io::Updates::new()
                .set_jumbf(placeholder_jumbf)
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

            let mut main_hasher = Sha256::new();
            // BTreeMap<mdat_id, Vec<(leaf_byte_size, leaf_sha256)>>; BTreeMap preserves order.
            let mut mdat_leaves: std::collections::BTreeMap<usize, Vec<(u64, Vec<u8>)>> =
                Default::default();

            let mut processor = |chunk: &dyn ProcessChunk| {
                if let Some(id) = chunk.id() {
                    // mdat content from byte 16 of the box onward → one Merkle leaf per chunk.
                    // The write path already trims to box[16..], no additional skip needed.
                    let data = chunk.data();
                    if !data.is_empty() {
                        mdat_leaves
                            .entry(id)
                            .or_default()
                            .push((data.len() as u64, Sha256::digest(data).to_vec()));
                    }
                } else {
                    // process_offset values AND box content both feed the main hash.
                    main_hasher.update(chunk.data());
                }
                Ok(())
            };

            let structure =
                asset.write_with_processing(&mut output_file, &updates, &mut processor)?;
            output_file.flush()?;

            // Assemble the final BmffHash from single-pass data.
            let mut bmff_hash = BmffHash::new("jumbf manifest", ph_alg, None);
            bmff_hash.set_default_exclusions();
            bmff_hash.set_bmff_version(3);
            bmff_hash.set_hash(main_hasher.finalize().to_vec());

            if !mdat_leaves.is_empty() {
                // Per the C2PA spec, the mdat Merkle subset starts at offset 16 from the
                // box start (skipping the box header regardless of whether it is 8 or 16
                // bytes, which is handled by the hash_start logic in the processor above).
                let mut mdat_excl = ExclusionsMap::new("/mdat".to_owned());
                mdat_excl.subset = Some(vec![SubsetMap {
                    offset: 16,
                    length: 0,
                }]);
                bmff_hash.add_exclusions(&mut vec![mdat_excl]);

                // Build MerkleMaps with all leaf hashes and their exact byte sizes stored
                // as variable_block_sizes. This lets the verifier re-hash each chunk
                // independently without needing proof boxes.
                let merkle_maps: Vec<MerkleMap> = mdat_leaves
                    .into_iter()
                    .enumerate()
                    .map(|(index, (mdat_id, leaves))| {
                        let (leaf_sizes, leaf_hashes): (Vec<u64>, Vec<Vec<u8>>) =
                            leaves.into_iter().unzip();
                        MerkleMap {
                            unique_id: mdat_id,
                            local_id: index,
                            count: leaf_hashes.len(),
                            alg: Some(ph_alg.to_string()),
                            init_hash: None,
                            hashes: VecByteBuf(
                                leaf_hashes.into_iter().map(ByteBuf::from).collect(),
                            ),
                            fixed_block_size: None,
                            variable_block_sizes: Some(leaf_sizes),
                        }
                    })
                    .collect();
                bmff_hash.set_merkle(merkle_maps);
            }

            // Replace the placeholder assertion with the fully-computed BmffHash.
            builder
                .definition
                .assertions
                .retain(|a| !a.label.contains(BmffHash::LABEL));
            builder.add_assertion(&format!("{}.v3", BmffHash::LABEL), &bmff_hash)?;

            let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
            (structure, output_file, signed_jumbf)
        } else {
            // Non-BMFF path: DataHash over the full file (excluding the JUMBF region).
            let signer = Settings::signer()?;
            let placeholder_jumbf = builder.placeholder("application/c2pa")?;

            let updates = asset_io::Updates::new()
                .set_jumbf(placeholder_jumbf)
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

            let mut hasher = Sha256::new();
            let mut processor = |chunk: &dyn ProcessChunk| {
                hasher.update(chunk.data());
                Ok(())
            };
            let structure =
                asset.write_with_processing(&mut output_file, &updates, &mut processor)?;
            output_file.flush()?;

            let (exclusion_offset, exclusion_size) = structure
                .exclusion_range_for_segment(SegmentKind::Jumbf)
                .ok_or("no JUMBF exclusion range")?;

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

        // Validate that the signed JUMBF fits in the reserved slot.
        let capacity: u64 = structure
            .c2pa_jumbf_index()
            .and_then(|i| structure.segments().get(i))
            .map(|seg| {
                if is_bmff {
                    seg.ranges.first().map(|r| r.size).unwrap_or(0)
                } else {
                    seg.ranges.iter().map(|r| r.size).sum()
                }
            })
            .unwrap_or(0);

        if signed_jumbf.len() as u64 > capacity {
            return Err(format!(
                "Signed JUMBF ({} bytes) exceeds reserved capacity ({} bytes). \
                 Increase BMFF_PLACEHOLDER_PADDING.",
                signed_jumbf.len(),
                capacity
            )
            .into());
        }

        structure.update_segment(&mut output_file, SegmentKind::Jumbf, signed_jumbf)?;
        output_file.flush()?;
    } else {
        // BoxHash path: single-pass write with per-segment hashing via NamedChunk boundaries.
        //
        // The format write path (PNG / JPEG) emits a zero-data NamedChunk boundary signal
        // before every logical structural chunk.  The processor uses these boundaries to
        // finalise the previous segment's hash and start a fresh one, accumulating a
        // separate SHA-256 per named segment in a single write pass.
        //
        // The "C2PA" boundary precedes the excluded JUMBF region; no bytes from the JUMBF
        // chunk reach the processor, so its BoxMap entry is stored with an empty hash
        // (matching what c2pa-rs's make_box_maps / get_box_map produces for that slot).
        //
        // BoxHash applies only to JPEG / PNG / GIF — never BMFF.
        let ph_alg = "sha256";

        // Add a placeholder BoxHash so that builder.placeholder() reserves enough space.
        builder.add_assertion(BoxHash::LABEL, &BoxHash { boxes: Vec::new() })?;
        let mut placeholder_jumbf = builder.placeholder("application/c2pa")?;
        placeholder_jumbf.extend(std::iter::repeat(0u8).take(BOX_HASH_PADDING));

        let updates = asset_io::Updates::new()
            .set_jumbf(placeholder_jumbf)
            .exclude_from_processing(
                vec![SegmentKind::Jumbf],
                asset_io::ExclusionMode::EntireSegment,
            );

        let mut output_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(output_path)?;

        // Per-segment accumulator state captured by the processor closure.
        let mut seg_name: Option<String> = None;
        let mut seg_hasher: Option<Sha256> = None;
        let mut box_maps: Vec<BoxMap> = Vec::new();

        let mut processor = |chunk: &dyn ProcessChunk| {
            if let Some(seg) = chunk.segment() {
                // Boundary signal — finalise the outgoing segment.
                if let Some(n) = seg_name.take() {
                    let bm = if let Some(h) = seg_hasher.take() {
                        BoxMap {
                            names: vec![n],
                            alg: Some(ph_alg.to_string()),
                            hash: ByteBuf::from(h.finalize().to_vec()),
                            excluded: None,
                            pad: ByteBuf::from(vec![]),
                            range_start: 0,
                            range_len: 0,
                        }
                    } else {
                        // C2PA JUMBF — excluded; stored with empty hash to maintain
                        // position parity with c2pa-rs's get_box_map sequence.
                        BoxMap {
                            names: vec![n],
                            alg: None,
                            hash: ByteBuf::from(vec![]),
                            excluded: None,
                            pad: ByteBuf::from(vec![]),
                            range_start: 0,
                            range_len: 0,
                        }
                    };
                    box_maps.push(bm);
                }
                // Map C2PA JUMBF to the BoxHash name "C2PA"; all other segments
                // use their container-native path.
                let is_c2pa = seg.is_c2pa();
                let box_name = if is_c2pa {
                    "C2PA"
                } else {
                    seg.path.as_deref().unwrap_or("")
                };
                // Start the incoming segment — no hasher for C2PA (bytes are excluded).
                seg_name = Some(box_name.to_string());
                seg_hasher = if is_c2pa { None } else { Some(Sha256::new()) };
            } else {
                // Regular data bytes — feed into the current segment's hasher.
                if let Some(h) = &mut seg_hasher {
                    h.update(chunk.data());
                }
            }
            Ok(())
        };

        let structure = asset.write_with_processing(&mut output_file, &updates, &mut processor)?;
        output_file.flush()?;

        // Finalise the last segment (no following boundary signal to trigger it).
        if let Some(n) = seg_name {
            if let Some(h) = seg_hasher {
                box_maps.push(BoxMap {
                    names: vec![n],
                    alg: Some(ph_alg.to_string()),
                    hash: ByteBuf::from(h.finalize().to_vec()),
                    excluded: None,
                    pad: ByteBuf::from(vec![]),
                    range_start: 0,
                    range_len: 0,
                });
            }
        }

        // Swap out the placeholder BoxHash for the real one.
        let bh = BoxHash { boxes: box_maps };
        builder
            .definition
            .assertions
            .retain(|a| !a.label.starts_with(BoxHash::LABEL));
        builder.add_assertion(BoxHash::LABEL, &bh)?;

        let signed_jumbf = builder.sign_embeddable("application/c2pa")?;

        structure.update_segment(&mut output_file, SegmentKind::Jumbf, signed_jumbf)?;
        output_file.flush()?;
    }

    println!(
        "Signed {} -> {} in {:?}",
        input_path,
        output_path,
        t.elapsed()
    );
    Ok(())
}
