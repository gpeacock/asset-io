//! Benchmark for parallel chunk hashing
//!
//! Demonstrates using rayon with read_chunks() for parallel hashing of large files.
//!
//! Run with: cargo run --release --example parallel_hash --features "all-formats,parallel,memory-mapped" -- <file>

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
    
    // Show fragment info for BMFF files
    #[cfg(feature = "bmff")]
    if structure.container == asset_io::ContainerKind::Bmff {
        // Re-open to get fragments (BMFF-specific)
        if let Ok(mut file) = std::fs::File::open(&input) {
            if let Ok(fragments) = asset_io::BmffIO::fragments(&mut file) {
                if !fragments.is_empty() {
                    println!("Fragments: {} (fragmented BMFF)", fragments.len());
                    if fragments.len() <= 5 {
                        for frag in &fragments {
                            println!(
                                "  Fragment {}: moof@{} ({}), mdat@{} ({} data)",
                                frag.index,
                                frag.moof_offset,
                                frag.moof_size,
                                frag.mdat_offset,
                                frag.data_size()
                            );
                        }
                    } else {
                        let first = &fragments[0];
                        let last = &fragments[fragments.len() - 1];
                        println!(
                            "  First: moof@{}, mdat {} bytes",
                            first.moof_offset,
                            first.data_size()
                        );
                        println!(
                            "  Last:  moof@{}, mdat {} bytes",
                            last.moof_offset,
                            last.data_size()
                        );
                    }
                } else {
                    println!("Fragments: none (not fragmented)");
                }
            }
        }
    }
    println!();

    // Configure updates with 1MB chunks
    let updates = Updates::new()
        .with_chunk_size(1024 * 1024) // 1MB chunks for parallel processing
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

    // Sequential hash using read_with_processing
    println!("--- Sequential Hash (read_with_processing) ---");
    let sequential_time;
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
        sequential_time = elapsed;

        println!("  Hash: {:x}", hash);
        println!("  Bytes: {} ({:.2} GB)", bytes, bytes as f64 / 1_073_741_824.0);
        println!("  Time: {:?}", elapsed);
        println!(
            "  Throughput: {:.2} GB/s",
            (bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
        );
    }
    
    // Overlapped I/O hash using read_with_processing_overlapped
    println!("\n--- Overlapped I/O Hash (read_with_processing_overlapped) ---");
    {
        use std::sync::{Arc, Mutex};
        
        let mut asset = Asset::open(&input)?;
        let start = Instant::now();
        let hasher = Arc::new(Mutex::new(sha2::Sha256::new()));
        let bytes = Arc::new(Mutex::new(0u64));
        
        let hasher_clone = hasher.clone();
        let bytes_clone = bytes.clone();
        
        asset.read_with_processing_overlapped(&updates, move |chunk| {
            let mut h = hasher_clone.lock().unwrap();
            sha2::Digest::update(&mut *h, chunk);
            *bytes_clone.lock().unwrap() += chunk.len() as u64;
        })?;

        let hash = sha2::Digest::finalize(Arc::try_unwrap(hasher).unwrap().into_inner().unwrap());
        let elapsed = start.elapsed();
        let total_bytes = *bytes.lock().unwrap();

        println!("  Hash: {:x}", hash);
        println!("  Bytes: {} ({:.2} GB)", total_bytes, total_bytes as f64 / 1_073_741_824.0);
        println!("  Time: {:?}", elapsed);
        println!(
            "  Throughput: {:.2} GB/s",
            (total_bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
        );
        
        let speedup = sequential_time.as_secs_f64() / elapsed.as_secs_f64();
        println!("  Speedup: {:.2}x vs sequential", speedup);
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

        // Parallel hash with file handle factory (true parallel I/O without mmap)
        println!("\n--- Parallel Hash with Factory (parallel_hash_with) ---");
        {
            let asset = Asset::open(&input)?;
            let input_path = input.clone();
            
            let start = Instant::now();
            let hashes = asset.parallel_hash_with::<Sha256, _, _>(
                &updates,
                || std::fs::File::open(&input_path),
            )?;
            let hash_time = start.elapsed();
            
            let merkle_start = Instant::now();
            let root3 = merkle_root::<Sha256>(&hashes);
            let merkle_time = merkle_start.elapsed();
            
            let elapsed = start.elapsed();
            
            println!("  Merkle root: {:02x?}...", &root3[..8]);
            println!("  Chunks: {}", hashes.len());
            println!("  Hash time: {:?}", hash_time);
            println!("  Merkle time: {:?}", merkle_time);
            println!("  Total time: {:?}", elapsed);
            println!(
                "  Throughput: {:.2} GB/s",
                (total_bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
            );
            
            let speedup = sequential_time.as_secs_f64() / elapsed.as_secs_f64();
            println!("  Speedup: {:.2}x vs sequential", speedup);
        }
        
        // Memory-mapped parallel hash (true parallel I/O)
        #[cfg(feature = "memory-mapped")]
        {
            println!("\n--- Memory-Mapped Parallel Hash (parallel_hash_mmap) ---");
            
            let start = Instant::now();
            let asset = unsafe { Asset::open_with_mmap(&input)? };
            let open_time = start.elapsed();
            
            let hash_start = Instant::now();
            let hashes = asset.parallel_hash_mmap::<Sha256>(&updates)?;
            let hash_time = hash_start.elapsed();
            
            let merkle_start = Instant::now();
            let root3 = merkle_root::<Sha256>(&hashes);
            let merkle_time = merkle_start.elapsed();
            
            let elapsed = start.elapsed();
            
            println!("  Merkle root: {:02x?}...", &root3[..8]);
            println!("  Chunks: {}", hashes.len());
            println!("  Open time: {:?}", open_time);
            println!("  Hash time: {:?}", hash_time);
            println!("  Merkle time: {:?}", merkle_time);
            println!("  Total time: {:?}", elapsed);
            println!(
                "  Throughput: {:.2} GB/s",
                (total_bytes as f64 / 1_073_741_824.0) / elapsed.as_secs_f64()
            );
            
            let speedup = sequential_time.as_secs_f64() / elapsed.as_secs_f64();
            println!("  Speedup: {:.2}x vs sequential", speedup);
        }
    }

    #[cfg(not(feature = "parallel"))]
    {
        println!("\n[Parallel hashing requires --features parallel]");
    }

    Ok(())
}
