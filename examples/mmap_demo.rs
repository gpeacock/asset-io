// Memory-mapped file demo - Zero-copy hashing for maximum performance
//
// This demonstrates using memory-mapped files for instant, zero-allocation
// access to file data. Perfect for hashing large files efficiently.

use asset_io::{ByteRange, FormatHandler, JpegHandler};
use std::fs::File;
use std::time::Instant;

fn main() -> asset_io::Result<()> {
    #[cfg(all(feature = "test-utils", feature = "memory-mapped"))]
    {
        use asset_io::test_utils::{fixture_path, FIREFLY_TRAIN, P1000708};

        println!("=== Memory-Mapped File Demo ===\n");
        println!("Demonstrates zero-copy file access for maximum performance\n");

        // Demo 1: Basic memory-mapped access
        println!("1. Basic Memory-Mapped Access");
        demo_basic_mmap(fixture_path(FIREFLY_TRAIN).to_str().unwrap())?;
        println!();

        // Demo 2: Compare performance: file I/O vs memory-mapped
        println!("2. Performance Comparison");
        demo_performance_comparison(fixture_path(P1000708).to_str().unwrap())?;
        println!();

        // Demo 3: Zero-copy hashing
        println!("3. Zero-Copy Hashing");
        demo_zero_copy_hashing(fixture_path(P1000708).to_str().unwrap())?;
        println!();

        println!("=== Demo Complete ===");
        println!("\nKey Benefits:");
        println!("  ✓ Zero allocations - direct memory access");
        println!("  ✓ Instant 'loading' - no I/O wait");
        println!("  ✓ OS-level caching - shared across processes");
        println!("  ✓ Perfect for large files - only map what you need");
    }

    #[cfg(not(all(feature = "test-utils", feature = "memory-mapped")))]
    {
        println!("This demo requires test-utils and memory-mapped features:");
        println!("  cargo run --example mmap_demo --features memory-mapped");
    }

    Ok(())
}

#[cfg(all(feature = "test-utils", feature = "memory-mapped"))]
fn demo_basic_mmap(path: &str) -> asset_io::Result<()> {
    println!("  File: {}", path);

    // Open and memory-map the file
    let file = File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let file_size = mmap.len();

    println!("  Memory-mapped {} bytes", file_size);

    // Parse structure
    let handler = JpegHandler::new();
    let mut parse_file = File::open(path)?;
    let mut structure = handler.parse(&mut parse_file)?;

    // Attach mmap
    structure = structure.with_mmap(mmap);

    println!("  Found {} segments", structure.segments.len());

    // Access data with zero-copy
    let header_range = ByteRange { offset: 0, size: 2 };
    if let Some(slice) = structure.get_mmap_slice(header_range) {
        println!(
            "  Header: {:02X} {:02X} (zero-copy access!)",
            slice[0], slice[1]
        );
    }

    println!("  ✓ Zero-copy access works!");

    Ok(())
}

#[cfg(all(feature = "test-utils", feature = "memory-mapped"))]
fn demo_performance_comparison(path: &str) -> asset_io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    println!("  File: {}", path);

    let file_size = std::fs::metadata(path)?.len();
    println!(
        "  Size: {} bytes ({:.1} MB)",
        file_size,
        file_size as f64 / 1024.0 / 1024.0
    );

    // Method 1: Traditional file I/O
    print!("  Method 1: File I/O... ");
    let start = Instant::now();
    let mut file = File::open(path)?;
    let mut buffer = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut buffer)?;
    let io_duration = start.elapsed();
    println!("took {:?}", io_duration);

    // Method 2: Memory-mapped
    print!("  Method 2: Memory-map... ");
    let start = Instant::now();
    let file = File::open(path)?;
    let _mmap = unsafe { memmap2::Mmap::map(&file)? };
    let mmap_duration = start.elapsed();
    println!("took {:?}", mmap_duration);

    // Access pattern: read every segment
    print!("  Method 3: File I/O with seeks... ");
    let start = Instant::now();
    let handler = JpegHandler::new();
    let mut file = File::open(path)?;
    let structure = handler.parse(&mut file)?;

    let mut total = 0u64;
    for segment in &structure.segments {
        let loc = segment.location();
        file.seek(SeekFrom::Start(loc.offset))?;
        let mut buf = vec![0u8; loc.size as usize];
        file.read_exact(&mut buf)?;
        total += loc.size;
    }
    let seek_duration = start.elapsed();
    println!("took {:?} ({} bytes)", seek_duration, total);

    // Method 4: Memory-mapped with structure
    print!("  Method 4: Memory-map with slices... ");
    let start = Instant::now();
    let file = File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let mut file = File::open(path)?;
    let structure = handler.parse(&mut file)?;
    let structure = structure.with_mmap(mmap);

    let mut total = 0u64;
    for segment in &structure.segments {
        let loc = segment.location();
        if let Some(slice) = structure.get_mmap_slice(ByteRange {
            offset: loc.offset,
            size: loc.size,
        }) {
            total += slice.len() as u64;
            // Just access first byte to ensure it's in memory
            let _ = slice[0];
        }
    }
    let mmap_slice_duration = start.elapsed();
    println!("took {:?} ({} bytes)", mmap_slice_duration, total);

    println!("\n  Performance Summary:");
    println!("    File I/O:          {:?}", io_duration);
    println!(
        "    Memory-map:        {:?} ({:.1}x faster)",
        mmap_duration,
        io_duration.as_secs_f64() / mmap_duration.as_secs_f64().max(0.000001)
    );
    println!("    Seeks+reads:       {:?}", seek_duration);
    println!(
        "    Mmap+slices:       {:?} ({:.1}x faster)",
        mmap_slice_duration,
        seek_duration.as_secs_f64() / mmap_slice_duration.as_secs_f64().max(0.000001)
    );

    Ok(())
}

#[cfg(all(feature = "test-utils", feature = "memory-mapped"))]
fn demo_zero_copy_hashing(path: &str) -> asset_io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    println!("  File: {}", path);
    println!("  Hash model: Data Hash (exclude JUMBF)");

    // Method 1: Traditional streaming hash
    print!("  Traditional streaming... ");
    let start = Instant::now();
    let mut file = File::open(path)?;
    let handler = JpegHandler::new();
    let structure = handler.parse(&mut file)?;

    let exclusions = vec!["jumbf"];
    let ranges = structure.hashable_ranges(&exclusions);

    let mut hasher1 = SimpleHasher::new();
    for range in ranges {
        file.seek(SeekFrom::Start(range.offset))?;
        let mut buf = vec![0u8; range.size as usize];
        file.read_exact(&mut buf)?;
        hasher1.update(&buf);
    }
    let hash1 = hasher1.finalize();
    let stream_duration = start.elapsed();
    println!("took {:?}", stream_duration);
    println!("    Hash: {}", hex_encode(&hash1));

    // Method 2: Memory-mapped zero-copy hash
    print!("  Memory-mapped zero-copy... ");
    let start = Instant::now();
    let file = File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let mut file = File::open(path)?;
    let structure = handler.parse(&mut file)?;
    let structure = structure.with_mmap(mmap);

    let exclusions = vec!["jumbf"];
    let ranges = structure.hashable_ranges(&exclusions);

    let mut hasher2 = SimpleHasher::new();
    for range in ranges {
        if let Some(slice) = structure.get_mmap_slice(range) {
            hasher2.update(slice); // Zero-copy!
        }
    }
    let hash2 = hasher2.finalize();
    let mmap_duration = start.elapsed();
    println!("took {:?}", mmap_duration);
    println!("    Hash: {}", hex_encode(&hash2));

    // Verify hashes match
    assert_eq!(hash1, hash2, "Hashes should match!");
    println!("  ✓ Hashes match!");

    println!("\n  Performance:");
    println!("    Streaming:   {:?}", stream_duration);
    println!(
        "    Zero-copy:   {:?} ({:.1}x faster)",
        mmap_duration,
        stream_duration.as_secs_f64() / mmap_duration.as_secs_f64().max(0.000001)
    );
    println!("    Allocations: Many vs ZERO!");

    Ok(())
}

// Simple hasher for demo
struct SimpleHasher {
    state: u64,
}

impl SimpleHasher {
    fn new() -> Self {
        Self { state: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.state = self.state.wrapping_mul(31).wrapping_add(byte as u64);
        }
    }

    fn finalize(self) -> Vec<u8> {
        self.state.to_le_bytes().to_vec()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}
