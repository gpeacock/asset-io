/// BMFF v3 Hash Example
///
/// This demonstrates how to create a BMFF merkle hash externally using asset-io's API.
/// This is a prototype for what will eventually go into c2pa-rs.
///
/// Run with: cargo run --features bmff,hashing --example bmff_v3_hash
use asset_io::Asset;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug)]
struct BoxHash {
    box_type: String,
    offset: u64,
    size: u64,
    hash: Vec<u8>,
}

fn main() -> asset_io::Result<()> {
    let input = "/Users/gpeacock/Downloads/IMG_9250.mov";

    println!("🔐 BMFF v3 Hash (Merkle Tree) Example");
    println!("=====================================\n");

    // Parse the file to get structure
    let mut asset = Asset::open(input)?;
    let structure = asset.structure();

    println!("File: {}", input);
    println!(
        "Size: {:.2} MB",
        structure.total_size as f64 / 1024.0 / 1024.0
    );
    println!("Segments: {}\n", structure.segments().len());

    // Hash each top-level box individually (BMFF v3 approach)
    println!("📦 Hashing individual boxes:");
    println!(
        "{:<15} {:>10} {:>12} {}",
        "Box Type", "Offset", "Size", "Hash (first 16 bytes)"
    );
    println!("{}", "-".repeat(80));

    let mut box_hashes = Vec::new();
    let mut source = std::fs::File::open(input)?;

    for segment in structure.segments() {
        // Get box info from segment
        let box_type = segment
            .path
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Skip metadata boxes that should be excluded
        if box_type == "uuid/xmp" || box_type == "uuid/c2pa" {
            println!(
                "{:<15} {:>10} {:>12} {}",
                box_type, segment.ranges[0].offset, segment.ranges[0].size, "EXCLUDED"
            );
            continue;
        }

        // Hash this box
        let mut hasher = Sha256::new();
        for range in &segment.ranges {
            source.seek(SeekFrom::Start(range.offset))?;
            let mut buffer = vec![0u8; range.size as usize];
            source.read_exact(&mut buffer)?;
            hasher.update(&buffer);
        }

        let hash = hasher.finalize();

        println!(
            "{:<15} {:>10} {:>12} {}",
            box_type,
            segment.ranges[0].offset,
            segment.ranges[0].size,
            hash[0..16]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );

        box_hashes.push(BoxHash {
            box_type,
            offset: segment.ranges[0].offset,
            size: segment.ranges[0].size,
            hash: hash.to_vec(),
        });
    }

    // Create merkle tree by hashing all box hashes together
    println!("\n🌳 Creating Merkle Root:");
    let mut merkle_hasher = Sha256::new();
    for box_hash in &box_hashes {
        merkle_hasher.update(&box_hash.hash);
    }
    let merkle_root = merkle_hasher.finalize();

    println!(
        "   Merkle root: {}",
        merkle_root
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    );

    // Display what would go into the BmffMerkleMap
    println!("\n📋 BmffMerkleMap Structure (for C2PA):");
    println!("   Boxes hashed: {}", box_hashes.len());
    println!("   Excluded: uuid/xmp, uuid/c2pa");
    println!("   Algorithm: SHA-256");

    // Show JSON-like structure
    println!("\n📄 Example BmffMerkleMap entry:");
    println!("{{");
    println!("  \"uniqueId\": 0,");
    println!("  \"localId\": 0,");
    println!("  \"location\": 0,");
    println!("  \"hashes\": [");
    for (i, box_hash) in box_hashes.iter().enumerate() {
        let comma = if i < box_hashes.len() - 1 { "," } else { "" };
        println!(
            "    // {} at offset {}: {}{}",
            box_hash.box_type,
            box_hash.offset,
            box_hash.hash[0..8]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>(),
            comma
        );
    }
    println!("  ]");
    println!("}}");

    println!("\n✅ BMFF v3 hash prototype complete!");
    println!("\n💡 Next steps:");
    println!("   1. This demonstrates the external API approach");
    println!("   2. Can be moved to c2pa-rs as get_object_locations_bmff_merkle()");
    println!("   3. Needs ExclusionsMap integration for full C2PA support");

    Ok(())
}
