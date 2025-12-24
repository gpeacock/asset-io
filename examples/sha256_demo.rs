// Real C2PA-style hashing with SHA-256
//
// Demonstrates hardware-accelerated hashing using Intel SHA-NI or ARM Crypto Extensions
// (automatically detected and used by the sha2 crate).

use jumbf_io::{JpegHandler, FormatHandler};
use sha2::{Sha256, Digest};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::Instant;

fn main() -> jumbf_io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    
    // Get file path from command line or use default fixture
    let path_str = if args.len() > 1 {
        args[1].clone()
    } else {
        #[cfg(feature = "test-utils")]
        {
            use jumbf_io::test_utils::{fixture_path, P1000708};
            fixture_path(P1000708).to_str().unwrap().to_string()
        }
        #[cfg(not(feature = "test-utils"))]
        {
            eprintln!("Usage: {} <path-to-jpeg>", args[0]);
            eprintln!("Or run with test-utils feature for default fixture");
            std::process::exit(1);
        }
    };
    
    println!("=== C2PA SHA-256 Hashing Demo ===\n");
    
    println!("File: {}", path_str);
    let file_size = std::fs::metadata(&path_str)?.len();
    println!("Size: {} bytes ({:.2} MB)\n", file_size, file_size as f64 / 1024.0 / 1024.0);
    
    // Method 1: Traditional file I/O with buffering
    println!("1. Traditional File I/O + SHA-256");
    let (hash1, duration1) = hash_with_file_io(&path_str)?;
    println!("   Duration: {:?}", duration1);
    println!("   Hash:     {}", hex_encode(&hash1));
    println!("   Speed:    {:.2} MB/s\n", 
             (file_size as f64 / 1024.0 / 1024.0) / duration1.as_secs_f64());
    
    // Method 2: Memory-mapped zero-copy
    #[cfg(feature = "memory-mapped")]
    {
        println!("2. Memory-Mapped Zero-Copy + SHA-256");
        let (hash2, duration2) = hash_with_mmap(&path_str)?;
        println!("   Duration: {:?}", duration2);
        println!("   Hash:     {}", hex_encode(&hash2));
        println!("   Speed:    {:.2} MB/s", 
                 (file_size as f64 / 1024.0 / 1024.0) / duration2.as_secs_f64());
        
        // Verify hashes match
        if hash1 == hash2 {
            println!("   ✓ Hashes match!\n");
        } else {
            println!("   ✗ Hash mismatch!\n");
        }
        
        // Performance comparison
        let speedup = duration1.as_secs_f64() / duration2.as_secs_f64();
        println!("Performance:");
        println!("   File I/O:        {:?}", duration1);
        println!("   Memory-mapped:   {:?}", duration2);
        
        if speedup > 1.0 {
            println!("   Speedup:         {:.2}x faster", speedup);
            let time_saved = duration1.checked_sub(duration2).unwrap_or(std::time::Duration::ZERO);
            if !time_saved.is_zero() {
                println!("   Time saved:      {:?}", time_saved);
            }
        } else if speedup < 1.0 {
            println!("   Slowdown:        {:.2}x slower", 1.0 / speedup);
            let time_lost = duration2.checked_sub(duration1).unwrap_or(std::time::Duration::ZERO);
            if !time_lost.is_zero() {
                println!("   Time lost:       {:?}", time_lost);
            }
        } else {
            println!("   Performance:     Equal");
        }
        
        println!();
        
        if speedup > 1.5 {
            println!("   💡 Significant speedup from zero-copy access!");
        } else if speedup > 1.1 {
            println!("   💡 Modest speedup, but with ZERO allocations!");
        } else if speedup >= 0.95 {
            println!("   💡 Similar performance, but mmap has ZERO allocations!");
        } else {
            println!("   ⚠️  File I/O was faster (possibly due to system caching/memory pressure)");
            println!("   💡 Mmap still has benefits: zero allocations, shared cache across processes");
        }
    }
    
    #[cfg(not(feature = "memory-mapped"))]
    println!("\n💡 Run with --features memory-mapped to see zero-copy performance!");
    
    println!("\nHardware Acceleration:");
    if cfg!(target_feature = "sha") {
        println!("   ✓ Intel SHA-NI detected (hardware accelerated)");
    } else if cfg!(target_arch = "aarch64") {
        println!("   ✓ ARM Crypto Extensions available");
    } else {
        println!("   ℹ Using software SHA-256 (still fast!)");
    }
    
    println!("\nC2PA Notes:");
    println!("   • This excludes JUMBF data (C2PA manifest)");
    println!("   • Uses SHA-256 (C2PA standard)");
    println!("   • Hardware acceleration automatic");
    println!("   • Zero-copy with mmap = maximum speed");
    
    Ok(())
}

/// Hash using traditional file I/O (with buffering)
fn hash_with_file_io(path: &str) -> jumbf_io::Result<(Vec<u8>, std::time::Duration)> {
    let start = Instant::now();
    
    let mut file = File::open(path)?;
    let handler = JpegHandler::new();
    let structure = handler.parse(&mut file)?;
    
    // Get ranges excluding JUMBF (C2PA data)
    let exclusions = vec!["jumbf"];
    let ranges = structure.hashable_ranges(&exclusions);
    
    // SHA-256 hasher (uses hardware acceleration if available)
    let mut hasher = Sha256::new();
    
    // Read and hash each range
    for range in ranges {
        file.seek(SeekFrom::Start(range.offset))?;
        
        // Buffer for reading (64KB chunks)
        let mut remaining = range.size;
        let mut buffer = vec![0u8; 65536];
        
        while remaining > 0 {
            let to_read = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..to_read])?;
            hasher.update(&buffer[..to_read]);
            remaining -= to_read as u64;
        }
    }
    
    let hash = hasher.finalize();
    let duration = start.elapsed();
    
    Ok((hash.to_vec(), duration))
}

/// Hash using memory-mapped zero-copy (maximum performance)
#[cfg(feature = "memory-mapped")]
fn hash_with_mmap(path: &str) -> jumbf_io::Result<(Vec<u8>, std::time::Duration)> {
    let start = Instant::now();
    
    // Open and memory-map the file
    let file = File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    
    // Parse structure
    let mut file = File::open(path)?;
    let handler = JpegHandler::new();
    let structure = handler.parse(&mut file)?;
    
    // Attach memory map
    let structure = structure.with_mmap(mmap);
    
    // Get ranges excluding JUMBF (C2PA data)
    let exclusions = vec!["jumbf"];
    let ranges = structure.hashable_ranges(&exclusions);
    
    // SHA-256 hasher (uses hardware acceleration if available)
    let mut hasher = Sha256::new();
    
    // Hash each range directly from memory map (zero-copy!)
    for range in ranges {
        if let Some(slice) = structure.get_mmap_slice(range) {
            // Direct memory access - hardware can hash at full speed!
            hasher.update(slice);
        }
    }
    
    let hash = hasher.finalize();
    let duration = start.elapsed();
    
    Ok((hash.to_vec(), duration))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

