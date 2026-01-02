/// BMFF C2PA Hash Example
///
/// This demonstrates how to create a C2PA BmffHash structure using asset-io's API.
/// This prototype will move to c2pa-rs once validated.
///
/// The BmffHash includes:
/// - Mandatory exclusions (ftyp, uuid/c2pa, mfra)
/// - Optional Merkle tree for mdat boxes (for large files)
/// - Proper ExclusionsMap structures
///
/// Run with: cargo run --features bmff,hashing --example bmff_c2pa_hash
use asset_io::Asset;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

// C2PA data structures (simplified versions for prototype)
#[derive(Debug)]
struct BmffHash {
    name: String,
    alg: String,
    hash: Option<Vec<u8>>,
    exclusions: Vec<ExclusionsMap>,
    merkle: Option<Vec<MerkleMap>>,
}

#[derive(Debug, Clone)]
struct ExclusionsMap {
    box_path: String,
    data: Option<Vec<DataMap>>,
    subset: Option<Vec<SubsetMap>>,
}

#[derive(Debug, Clone)]
struct DataMap {
    offset: u64,
    value: Vec<u8>,
}

#[derive(Debug, Clone)]
struct SubsetMap {
    offset: u64,
    length: u64,
}

#[derive(Debug, Clone)]
struct MerkleMap {
    unique_id: usize,
    local_id: usize,
    count: usize,
    alg: Option<String>,
    fixed_block_size: Option<u64>,
    hashes: Vec<Vec<u8>>,
}

impl BmffHash {
    fn new(name: &str, alg: &str) -> Self {
        Self {
            name: name.to_string(),
            alg: alg.to_string(),
            hash: None,
            exclusions: Vec::new(),
            merkle: None,
        }
    }

    fn add_mandatory_exclusions(&mut self) {
        // 1. Exclude C2PA UUID boxes
        let uuid = ExclusionsMap {
            box_path: "/uuid".to_string(),
            data: Some(vec![DataMap {
                offset: 8,
                value: vec![
                    0xd8, 0xfe, 0xc3, 0xd6, 0x1b, 0x0e, 0x48, 0x3c, 0x92, 0x97, 0x58, 0x28, 0x87,
                    0x7e, 0xc4, 0x81,
                ], // C2PA UUID
            }]),
            subset: None,
        };
        self.exclusions.push(uuid);

        // 2. Exclude ftyp box
        self.exclusions.push(ExclusionsMap {
            box_path: "/ftyp".to_string(),
            data: None,
            subset: None,
        });

        // 3. Exclude mfra box (movie fragment random access)
        self.exclusions.push(ExclusionsMap {
            box_path: "/mfra".to_string(),
            data: None,
            subset: None,
        });
    }
}

fn main() -> asset_io::Result<()> {
    let input = "/Users/gpeacock/Downloads/IMG_9250.mov";

    println!("🔐 BMFF C2PA Hash Structure Example");
    println!("====================================\n");

    // Parse the file
    let mut asset = Asset::open(input)?;
    let structure = asset.structure();

    println!("File: {}", input);
    println!(
        "Size: {:.2} MB",
        structure.total_size as f64 / 1024.0 / 1024.0
    );
    println!("Segments: {}\n", structure.segments().len());

    // Create BmffHash with mandatory exclusions
    let mut bmff_hash = BmffHash::new("jumbf manifest", "sha256");
    bmff_hash.add_mandatory_exclusions();

    println!("📋 Mandatory Exclusions:");
    for (i, excl) in bmff_hash.exclusions.iter().enumerate() {
        println!("  {}. {}", i + 1, excl.box_path);
        if let Some(ref data) = excl.data {
            for d in data {
                println!(
                    "     └─ Data at offset {}: {} bytes",
                    d.offset,
                    d.value.len()
                );
            }
        }
    }

    // Optional: Create Merkle tree for mdat boxes (for large files)
    let merkle_chunk_size_kb = 1024; // 1MB chunks
    let use_merkle = structure.total_size > 50 * 1024 * 1024; // Use for files > 50MB

    if use_merkle {
        println!("\n🌳 Creating Merkle Tree (file > 50MB):");
        println!("  Chunk size: {} KB", merkle_chunk_size_kb);

        // Add mdat exclusion when using Merkle
        bmff_hash.exclusions.push(ExclusionsMap {
            box_path: "/mdat".to_string(),
            data: None,
            subset: Some(vec![SubsetMap {
                offset: 16,
                length: 0,
            }]),
        });

        // Find mdat boxes
        let mut mdat_boxes = Vec::new();
        for (idx, segment) in structure.segments().iter().enumerate() {
            if let Some(ref path) = segment.path {
                if path == "mdat" {
                    mdat_boxes.push((idx, segment));
                }
            }
        }

        println!("  Found {} mdat box(es)", mdat_boxes.len());

        // Create Merkle map for each mdat box
        let mut merkle_maps = Vec::new();
        let mut source = std::fs::File::open(input)?;

        for (local_id, (idx, segment)) in mdat_boxes.iter().enumerate() {
            let mdat_size: u64 = segment.ranges.iter().map(|r| r.size).sum();
            let chunk_size = merkle_chunk_size_kb * 1024;
            let num_chunks = (mdat_size + chunk_size - 1) / chunk_size;

            println!(
                "  mdat[{}]: {:.2} MB → {} chunks",
                local_id,
                mdat_size as f64 / 1024.0 / 1024.0,
                num_chunks
            );

            let mut hashes = Vec::new();

            // Hash each chunk
            for chunk_idx in 0..num_chunks {
                let chunk_offset = segment.ranges[0].offset + (chunk_idx * chunk_size);
                let chunk_len = std::cmp::min(chunk_size, mdat_size - (chunk_idx * chunk_size));

                source.seek(SeekFrom::Start(chunk_offset))?;
                let mut buffer = vec![0u8; chunk_len as usize];
                source.read_exact(&mut buffer)?;

                let mut hasher = Sha256::new();
                hasher.update(&buffer);
                let hash = hasher.finalize().to_vec();

                hashes.push(hash);
            }

            merkle_maps.push(MerkleMap {
                unique_id: 0,
                local_id,
                count: hashes.len(),
                alg: Some("sha256".to_string()),
                fixed_block_size: Some(chunk_size),
                hashes,
            });
        }

        bmff_hash.merkle = Some(merkle_maps.clone());

        println!("  ✓ Merkle tree created with {} map(s)", merkle_maps.len());
    }

    // Set placeholder hash (will be replaced with actual hash during signing)
    bmff_hash.hash = Some(vec![0u8; 32]); // SHA-256 placeholder

    // Display the final structure
    println!("\n📄 BmffHash Structure:");
    println!("  name: {}", bmff_hash.name);
    println!("  alg: {}", bmff_hash.alg);
    println!(
        "  hash: {} bytes (placeholder)",
        bmff_hash.hash.as_ref().unwrap().len()
    );
    println!("  exclusions: {} items", bmff_hash.exclusions.len());
    if let Some(ref merkle) = bmff_hash.merkle {
        println!("  merkle: {} map(s)", merkle.len());
        for (i, m) in merkle.iter().enumerate() {
            println!(
                "    - map[{}]: {} hashes, chunk size: {} KB",
                i,
                m.count,
                m.fixed_block_size.unwrap_or(0) / 1024
            );
        }
    }

    println!("\n✅ C2PA BmffHash structure created!");
    println!("\n💡 Next steps:");
    println!("   1. This structure can be serialized into the C2PA manifest");
    println!("   2. The placeholder hash will be replaced with actual hash during signing");
    println!("   3. Merkle UUID boxes will be inserted after the last mdat box");
    println!("   4. Move this logic to c2pa-rs as generate_bmff_data_hash()");

    // Show what the actual hash would be (excluding the right boxes)
    println!("\n🔐 Computing actual hash (excluding specified boxes):");
    let mut hasher = Sha256::new();
    let mut bytes_hashed = 0u64;

    let mut source = std::fs::File::open(input)?;
    for segment in structure.segments() {
        let box_type = segment
            .path
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("unknown");

        // Check if this box is excluded
        let is_excluded = box_type == "ftyp"
            || box_type == "uuid/c2pa"
            || box_type == "uuid/xmp"
            || (use_merkle && box_type == "mdat");

        if is_excluded {
            println!("   SKIP: {}", box_type);
            continue;
        }

        println!("   HASH: {}", box_type);

        for range in &segment.ranges {
            source.seek(SeekFrom::Start(range.offset))?;
            let mut buffer = vec![0u8; range.size as usize];
            source.read_exact(&mut buffer)?;
            hasher.update(&buffer);
            bytes_hashed += range.size;
        }
    }

    let final_hash = hasher.finalize();
    println!(
        "\n   Bytes hashed: {:.2} MB",
        bytes_hashed as f64 / 1024.0 / 1024.0
    );
    println!(
        "   Hash: {}",
        final_hash
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    );

    Ok(())
}
