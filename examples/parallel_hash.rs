//! Benchmark for parallel chunk hashing
//!
//! Demonstrates using rayon with read_chunks() for parallel hashing of large files.
//!
//! Run with: cargo run --release --example parallel_hash --features "all-formats,parallel" -- <file>

use asset_io::{Asset, ExclusionMode, SegmentKind, Updates};
use std::time::Instant;

#[cfg(feature = "parallel")]
use {
    asset_io::merkle_root,
    rayon::prelude::*,
    sha2::{Digest, Sha256},
};

fn main() -> asset_io::Result<()> {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/fixtures/P1000708.jpg".to_string());

    println!("=== Parallel Hash Benchmark ===\n");
    println!("File: {}", input);

    let mut asset = Asset::open(&input)?;
    let structure = asset.structure();

    println!("Container: {:?}", structure.container);
    println!(
        "Size: {} bytes ({:.2} GB)",
        structure.total_size,
        structure.total_size as f64 / 1_073_741_824.0
    );
    println!("Segments: {}", structure.segments.len());
    println!();

    // Configure updates with 1MB chunks
    let updates = Updates::new()
        .with_chunk_size(1024 * 1024) // 1MB chunks for parallel processing
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

    // Sequential hash using read_with_processing
    println!("--- Sequential Hash (read_with_processing) ---");
    {
        let start = Instant::now();
        let mut hasher = sha2::Sha256::new();
        let mut bytes = 0u64;

        asset.read_with_processing(&updates, &mut |chunk| {
            sha2::Digest::update(&mut hasher, chunk);
            bytes += chunk.len() as u64;
        })?;

        let hash = sha2::Digest::finalize(hasher);
        let elapsed = start.elapsed();

        println!("  Hash: {:x}", hash);
        println!("  Bytes: {} ({:.2} GB)", bytes, bytes as f64 / 1_073_741_824.0);
        println!("  Time: {:?}", elapsed);
        println!(
            "  Throughput: {:.2} GB/s",
            (bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
        );
    }

    // Parallel hash using read_chunks + rayon
    #[cfg(feature = "parallel")]
    {
        println!("\n--- Parallel Hash (read_chunks + rayon) ---");

        // Re-open to reset position
        let mut asset = Asset::open(&input)?;

        let start = Instant::now();

        // Read chunks (sequential I/O)
        let io_start = Instant::now();
        let chunks = asset.read_chunks(&updates)?;
        let io_time = io_start.elapsed();

        let total_bytes: u64 = chunks.iter().map(|c| c.data.len() as u64).sum();
        let included_chunks: Vec<_> = chunks.iter().filter(|c| !c.excluded).collect();

        println!("  Chunks: {} total, {} included", chunks.len(), included_chunks.len());
        println!("  I/O time: {:?}", io_time);

        // Hash in parallel (parallel CPU)
        let hash_start = Instant::now();
        let chunk_hashes: Vec<[u8; 32]> = included_chunks
            .par_iter()
            .map(|c| {
                let mut hasher = Sha256::new();
                hasher.update(&c.data);
                hasher.finalize().into()
            })
            .collect();
        let hash_time = hash_start.elapsed();

        // Compute Merkle root
        let merkle_start = Instant::now();
        let root = merkle_root::<Sha256>(&chunk_hashes);
        let merkle_time = merkle_start.elapsed();

        let elapsed = start.elapsed();

        println!("  Merkle root: {:02x?}...", &root[..8]);
        println!("  Hash time: {:?}", hash_time);
        println!("  Merkle time: {:?}", merkle_time);
        println!("  Total time: {:?}", elapsed);
        println!(
            "  Throughput: {:.2} GB/s",
            (total_bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
        );

        // Compare with built-in parallel_hash
        println!("\n--- Built-in parallel_hash method ---");
        let mut asset = Asset::open(&input)?;
        let start = Instant::now();
        let hashes = asset.parallel_hash::<Sha256>(&updates)?;
        let root2 = merkle_root::<Sha256>(&hashes);
        let elapsed = start.elapsed();

        println!("  Merkle root: {:02x?}...", &root2[..8]);
        println!("  Time: {:?}", elapsed);
        println!(
            "  Throughput: {:.2} GB/s",
            (total_bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
        );
    }

    #[cfg(not(feature = "parallel"))]
    {
        println!("\n[Parallel hashing requires --features parallel]");
    }

    Ok(())
}
