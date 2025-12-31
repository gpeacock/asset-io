//! Benchmark different hash algorithms for C2PA workflows
//!
//! Tests SHA-256, SHA-384, and SHA-512 to find the fastest option.
//!
//! Run: `cargo run --release --example hash_benchmark --features xmp,png,memory-mapped,hashing tests/fixtures/massive_test.png`

use asset_io::Asset;
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <image_file>", args[0]);
        return Ok(());
    }

    let source_path = &args[1];
    
    println!("=== C2PA Hash Algorithm Benchmark ===\n");
    println!("File: {}", source_path);
    
    // Get file size
    let metadata = std::fs::metadata(source_path)?;
    println!("Size: {:.2} MB ({} bytes)\n", 
             metadata.len() as f64 / 1_000_000.0,
             metadata.len());

    // Open with memory mapping for zero-copy
    let mut asset = unsafe { Asset::open_with_mmap(source_path)? };
    
    println!("Segments: {}", asset.structure().segments.len());
    println!("Total size: {} bytes\n", asset.structure().total_size);

    // Test each algorithm
    let algorithms = vec![
        ("SHA-256", "sha256"),
        ("SHA-384", "sha384"),
        ("SHA-512", "sha512"),
    ];

    let runs = 5;
    println!("Running {} iterations per algorithm...\n", runs);

    for (name, _alg) in algorithms {
        print!("{}: ", name);
        
        let mut times = Vec::new();
        
        for _ in 0..runs {
            let start = Instant::now();
            
            // Hash the entire file (simulating C2PA workflow without exclusions for simplicity)
            match name {
                "SHA-256" => {
                    let mut hasher = Sha256::new();
                    asset.hash_excluding_segments(&[], &mut hasher)?;
                    let _hash = hasher.finalize();
                }
                "SHA-384" => {
                    let mut hasher = Sha384::new();
                    asset.hash_excluding_segments(&[], &mut hasher)?;
                    let _hash = hasher.finalize();
                }
                "SHA-512" => {
                    let mut hasher = Sha512::new();
                    asset.hash_excluding_segments(&[], &mut hasher)?;
                    let _hash = hasher.finalize();
                }
                _ => unreachable!()
            }
            
            let elapsed = start.elapsed();
            times.push(elapsed);
            print!(".");
            std::io::Write::flush(&mut std::io::stdout())?;
        }
        
        println!();
        
        // Calculate statistics
        let total: std::time::Duration = times.iter().sum();
        let avg = total / runs as u32;
        let min = times.iter().min().unwrap();
        let max = times.iter().max().unwrap();
        
        let throughput = metadata.len() as f64 / avg.as_secs_f64() / 1_000_000.0;
        
        println!("  Min:        {:>6.0}ms", min.as_millis());
        println!("  Max:        {:>6.0}ms", max.as_millis());
        println!("  Avg:        {:>6.0}ms", avg.as_millis());
        println!("  Throughput: {:>6.0} MB/s", throughput);
        println!();
    }

    Ok(())
}

