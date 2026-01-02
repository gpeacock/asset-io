//! Compare memory-mapped vs regular file I/O performance for C2PA hashing
//!
//! Tests the performance difference between:
//! 1. Memory-mapped I/O (zero-copy hashing directly from mmap)
//! 2. Regular file I/O (streaming with buffers)
//!
//! Run: `cargo run --release --example mmap_comparison --features xmp,png,memory-mapped tests/fixtures/massive_test.png`

use asset_io::{Asset, Updates};
use sha2::{Digest, Sha512};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <image_file>", args[0]);
        return Ok(());
    }

    let source_path = &args[1];

    println!("=== Memory-Mapped vs Regular I/O Comparison ===\n");
    println!("File: {}", source_path);

    let metadata = std::fs::metadata(source_path)?;
    println!(
        "Size: {:.2} MB ({} bytes)\n",
        metadata.len() as f64 / 1_000_000.0,
        metadata.len()
    );

    let runs = 5;
    println!("Running {} iterations per method...\n", runs);

    // Test 1: Memory-mapped I/O (zero-copy)
    println!("=== With Memory Mapping (zero-copy) ===");
    let mut mmap_times = Vec::new();

    for i in 0..runs {
        print!("Run {}: ", i + 1);

        let start = Instant::now();

        // Open with memory mapping
        let mut asset = unsafe { Asset::open_with_mmap(source_path)? };

        // Hash using zero-copy from mmap
        let updates = Updates::new();
        let mut hasher = Sha512::new();
        asset.read_with_processing(&updates, &mut |chunk| hasher.update(chunk))?;
        let _hash = hasher.finalize();

        let elapsed = start.elapsed();
        println!("{:.0}ms", elapsed.as_millis());
        mmap_times.push(elapsed);
    }

    let mmap_avg: std::time::Duration =
        mmap_times.iter().sum::<std::time::Duration>() / runs as u32;
    let mmap_throughput = metadata.len() as f64 / mmap_avg.as_secs_f64() / 1_000_000.0;

    println!("\nMemory-mapped results:");
    println!("  Avg time:   {:>6.0}ms", mmap_avg.as_millis());
    println!("  Throughput: {:>6.0} MB/s", mmap_throughput);
    println!();

    // Test 2: Regular file I/O (streaming)
    println!("=== Without Memory Mapping (streaming) ===");
    let mut regular_times = Vec::new();

    for i in 0..runs {
        print!("Run {}: ", i + 1);

        let start = Instant::now();

        // Open with regular file I/O (no mmap)
        let mut asset = Asset::open(source_path)?;

        // Hash using streaming (uses ProcessingOptions chunk size)
        let updates = Updates::new();
        let mut hasher = Sha512::new();
        asset.read_with_processing(&updates, &mut |chunk| hasher.update(chunk))?;
        let _hash = hasher.finalize();

        let elapsed = start.elapsed();
        println!("{:.0}ms", elapsed.as_millis());
        regular_times.push(elapsed);
    }

    let regular_avg: std::time::Duration =
        regular_times.iter().sum::<std::time::Duration>() / runs as u32;
    let regular_throughput = metadata.len() as f64 / regular_avg.as_secs_f64() / 1_000_000.0;

    println!("\nRegular I/O results:");
    println!("  Avg time:   {:>6.0}ms", regular_avg.as_millis());
    println!("  Throughput: {:>6.0} MB/s", regular_throughput);
    println!();

    // Comparison
    println!("=== Comparison ===");
    let speedup = regular_avg.as_secs_f64() / mmap_avg.as_secs_f64();
    let time_saved = regular_avg.as_millis() as i64 - mmap_avg.as_millis() as i64;

    println!("Memory-mapped is {:.2}x faster", speedup);
    println!("Time saved: {}ms", time_saved);
    println!(
        "Throughput gain: {:.0} MB/s",
        mmap_throughput - regular_throughput
    );

    if speedup > 1.5 {
        println!("\n✅ Memory mapping provides significant performance benefit!");
    } else if speedup > 1.1 {
        println!("\n✅ Memory mapping provides moderate performance benefit.");
    } else {
        println!("\n⚠️  Memory mapping provides minimal benefit for this file size.");
    }

    Ok(())
}
