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

use asset_io::{Asset, ExclusionMode, ProcessChunk, SegmentKind, Structure, Updates};
use c2pa::{
    assertions::{
        c2pa_action, Action, BmffHash, BoxHash, BoxMap, DataHash, ExclusionsMap, MerkleMap,
        SubsetMap, VecByteBuf,
    },
    Builder, ClaimGeneratorInfo, HashRange, Settings,
};
use serde_bytes::ByteBuf;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, Write};
use std::time::Instant;

type SignResult = (Structure, Vec<u8>);
type Error = Box<dyn std::error::Error>;

/// Extra bytes reserved beyond the placeholder JUMBF for BmffHash (Merkle maps grow with mdat).
const BMFF_PLACEHOLDER_PADDING: usize = 50 * 1024;

/// Extra bytes reserved for BoxHash (covers the per-segment BoxMap entries plus signing overhead).
const BOX_HASH_PADDING: usize = 16 * 1024;

const HASH_ALG: &str = "sha256";

// ---------------------------------------------------------------------------
// Signing paths
// ---------------------------------------------------------------------------

/// Single-pass V3 BmffHash over the output stream.
///
/// Hashes `offset || content` for each non-excluded top-level box, and builds
/// Merkle leaves for mdat content starting at byte 16 of the box.
fn sign_bmff<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    output: &mut W,
) -> Result<SignResult, Error> {
    let mut placeholder = BmffHash::new("jumbf manifest", HASH_ALG, None);
    placeholder.set_default_exclusions();
    placeholder.add_place_holder_hash()?;
    builder.add_assertion(&format!("{}.v3", BmffHash::LABEL), &placeholder)?;

    let mut placeholder_jumbf = builder.placeholder("application/c2pa")?;
    placeholder_jumbf.extend(std::iter::repeat(0u8).take(BMFF_PLACEHOLDER_PADDING));

    let updates = Updates::new()
        .set_jumbf(placeholder_jumbf)
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

    let mut main_hasher = Sha256::new();
    // BTreeMap preserves insertion order by key, giving stable per-mdat leaf ordering.
    let mut mdat_leaves: std::collections::BTreeMap<usize, Vec<(u64, Vec<u8>)>> =
        Default::default();

    let mut processor = |chunk: &dyn ProcessChunk| {
        if let Some(id) = chunk.id() {
            // mdat content from byte 16 onwards → one Merkle leaf per chunk.
            let data = chunk.data();
            if !data.is_empty() {
                mdat_leaves
                    .entry(id)
                    .or_default()
                    .push((data.len() as u64, Sha256::digest(data).to_vec()));
            }
        } else {
            // Offset values and box content both feed the main hash.
            main_hasher.update(chunk.data());
        }
        Ok(())
    };

    let structure = asset.write_with_processing(output, &updates, &mut processor)?;
    output.flush()?;

    // Assemble the final BmffHash from accumulated single-pass data.
    let mut bmff_hash = BmffHash::new("jumbf manifest", HASH_ALG, None);
    bmff_hash.set_default_exclusions();
    bmff_hash.set_bmff_version(3);
    bmff_hash.set_hash(main_hasher.finalize().to_vec());

    if !mdat_leaves.is_empty() {
        let mut mdat_excl = ExclusionsMap::new("/mdat".to_owned());
        mdat_excl.subset = Some(vec![SubsetMap {
            offset: 16,
            length: 0,
        }]);
        bmff_hash.add_exclusions(&mut vec![mdat_excl]);

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
                    alg: Some(HASH_ALG.to_string()),
                    init_hash: None,
                    hashes: VecByteBuf(leaf_hashes.into_iter().map(ByteBuf::from).collect()),
                    fixed_block_size: None,
                    variable_block_sizes: Some(leaf_sizes),
                }
            })
            .collect();
        bmff_hash.set_merkle(merkle_maps);
    }

    builder
        .definition
        .assertions
        .retain(|a| !a.label.contains(BmffHash::LABEL));
    builder.add_assertion(&format!("{}.v3", BmffHash::LABEL), &bmff_hash)?;

    let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
    Ok((structure, signed_jumbf))
}

/// Single-pass DataHash over the full output stream (JUMBF region excluded).
fn sign_data_hash<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    output: &mut W,
) -> Result<SignResult, Error> {
    let placeholder_jumbf = builder.placeholder("application/c2pa")?;

    let updates = Updates::new()
        .set_jumbf(placeholder_jumbf)
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

    let mut hasher = Sha256::new();
    let mut processor = |chunk: &dyn ProcessChunk| {
        hasher.update(chunk.data());
        Ok(())
    };

    let structure = asset.write_with_processing(output, &updates, &mut processor)?;
    output.flush()?;

    let (exclusion_offset, exclusion_size) = structure
        .exclusion_range_for_segment(SegmentKind::Jumbf)
        .ok_or("no JUMBF exclusion range")?;

    let mut data_hash = DataHash::new("jumbf_manifest", HASH_ALG);
    data_hash.add_exclusion(HashRange::new(exclusion_offset, exclusion_size));
    data_hash.set_hash(hasher.finalize().to_vec());

    builder
        .definition
        .assertions
        .retain(|a| !a.label.starts_with(DataHash::LABEL));
    builder.add_assertion(DataHash::LABEL, &data_hash)?;

    let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
    Ok((structure, signed_jumbf))
}

/// Single-pass BoxHash: one SHA-256 per named logical segment, accumulated during the write.
///
/// The format write path emits a zero-data segment boundary before each logical chunk.
/// The processor uses those boundaries to finalise the outgoing segment's hash and begin
/// the next one. The JUMBF segment is excluded from hashing but still recorded in the
/// BoxMap (with an empty hash) to preserve position parity with the verifier's sequence.
fn sign_box_hash<R: Read + Seek, W: Read + Write + Seek>(
    asset: &mut Asset<R>,
    builder: &mut Builder,
    output: &mut W,
) -> Result<SignResult, Error> {
    builder.add_assertion(BoxHash::LABEL, &BoxHash { boxes: Vec::new() })?;
    let mut placeholder_jumbf = builder.placeholder("application/c2pa")?;
    placeholder_jumbf.extend(std::iter::repeat(0u8).take(BOX_HASH_PADDING));

    let updates = Updates::new()
        .set_jumbf(placeholder_jumbf)
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::EntireSegment);

    let mut seg_name: Option<String> = None;
    let mut seg_hasher: Option<Sha256> = None;
    let mut box_maps: Vec<BoxMap> = Vec::new();

    let mut processor = |chunk: &dyn ProcessChunk| {
        if let Some(seg) = chunk.segment() {
            // Boundary signal — finalise the outgoing segment first.
            if let Some(n) = seg_name.take() {
                box_maps.push(match seg_hasher.take() {
                    Some(h) => BoxMap {
                        names: vec![n],
                        alg: Some(HASH_ALG.to_string()),
                        hash: ByteBuf::from(h.finalize().to_vec()),
                        excluded: None,
                        pad: ByteBuf::from(vec![]),
                        range_start: 0,
                        range_len: 0,
                    },
                    None => BoxMap {
                        // JUMBF segment — excluded, empty hash maintains sequence parity.
                        names: vec![n],
                        alg: None,
                        hash: ByteBuf::from(vec![]),
                        excluded: None,
                        pad: ByteBuf::from(vec![]),
                        range_start: 0,
                        range_len: 0,
                    },
                });
            }
            // Map C2PA JUMBF to the BoxHash name "C2PA"; other segments use their path.
            let is_c2pa = seg.is_c2pa();
            seg_name = Some(
                if is_c2pa {
                    "C2PA"
                } else {
                    seg.path.as_deref().unwrap_or("")
                }
                .to_string(),
            );
            seg_hasher = if is_c2pa { None } else { Some(Sha256::new()) };
        } else if let Some(h) = &mut seg_hasher {
            h.update(chunk.data());
        }
        Ok(())
    };

    let structure = asset.write_with_processing(output, &updates, &mut processor)?;
    output.flush()?;

    // Finalise the last segment (no trailing boundary signal to trigger it).
    if let (Some(n), Some(h)) = (seg_name, seg_hasher) {
        box_maps.push(BoxMap {
            names: vec![n],
            alg: Some(HASH_ALG.to_string()),
            hash: ByteBuf::from(h.finalize().to_vec()),
            excluded: None,
            pad: ByteBuf::from(vec![]),
            range_start: 0,
            range_len: 0,
        });
    }

    builder
        .definition
        .assertions
        .retain(|a| !a.label.starts_with(BoxHash::LABEL));
    builder.add_assertion(BoxHash::LABEL, &BoxHash { boxes: box_maps })?;

    let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
    Ok((structure, signed_jumbf))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<(), Error> {
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

    // Load settings: prefer ASSET_IO_SETTINGS env var, fall back to test fixtures.
    let settings_path = std::env::var("ASSET_IO_SETTINGS")
        .unwrap_or_else(|_| "tests/fixtures/test_settings.json".to_string());
    let settings = Settings::from_string(&std::fs::read_to_string(&settings_path)?, "json")?;
    let context = c2pa::Context::new().with_settings(settings)?.into_shared();
    let mut builder = Builder::from_shared_context(&context);

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

    let mut output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_path)?;

    let native_format = asset.media_type().to_mime();
    let is_bmff = matches!(asset.structure().container, asset_io::ContainerKind::Bmff);

    let mut cgi = ClaimGeneratorInfo::new("asset-io-embeddable-example".to_string());
    cgi.set_version("0.1.0");
    builder.set_claim_generator_info(cgi);
    builder
        .add_action(Action::new(c2pa_action::CREATED).set_parameter("identifier", input_path)?)?;

    let t = Instant::now();

    let (structure, signed_jumbf) = if builder.needs_placeholder(&native_format) {
        if is_bmff {
            sign_bmff(&mut asset, &mut builder, &mut output)?
        } else {
            sign_data_hash(&mut asset, &mut builder, &mut output)?
        }
    } else {
        sign_box_hash(&mut asset, &mut builder, &mut output)?
    };

    structure.update_segment(&mut output, SegmentKind::Jumbf, signed_jumbf)?;
    output.flush()?;

    println!(
        "Signed {} -> {} in {:?}",
        input_path,
        output_path,
        t.elapsed()
    );
    Ok(())
}
