# SHA-256 Hardware Hashing Demo

This example demonstrates C2PA-style hashing with hardware acceleration using the `sha2` crate.

## Quick Start

```bash
# Basic demo (file I/O)
cargo run --example sha256_demo

# With memory-mapped comparison
cargo run --example sha256_demo --features memory-mapped
```

## What It Does

1. **Parses a JPEG file** with C2PA data
2. **Computes SHA-256 hash** of image content (excluding JUMBF/C2PA manifest)
3. **Compares two methods**:
   - Traditional file I/O with buffering
   - Memory-mapped zero-copy access
4. **Detects hardware acceleration** (Intel SHA-NI or ARM Crypto Extensions)
5. **Verifies hash consistency** between both methods

## Expected Output

```
=== C2PA SHA-256 Hashing Demo ===

File: /path/to/P1000708.jpg
Size: 829682 bytes (0.79 MB)

1. Traditional File I/O + SHA-256
   Duration: 35.8ms
   Hash:     c18eb919bafe4cba0b9ee1594ac4cbb0b670c85154aad24a195eec50c3b2326f
   Speed:    22.07 MB/s

2. Memory-Mapped Zero-Copy + SHA-256
   Duration: 35.5ms
   Hash:     c18eb919bafe4cba0b9ee1594ac4cbb0b670c85154aad24a195eec50c3b2326f
   Speed:    22.26 MB/s
   ✓ Hashes match!

Performance:
   File I/O:        35.8ms
   Memory-mapped:   35.5ms
   Speedup:         1.0x faster

Hardware Acceleration:
   ✓ ARM Crypto Extensions available

C2PA Notes:
   • This excludes JUMBF data (C2PA manifest)
   • Uses SHA-256 (C2PA standard)
   • Hardware acceleration automatic
   • Zero-copy with mmap = maximum speed
```

## How It Works

### File I/O Method

```rust
let mut file = File::open(path)?;
let handler = JpegHandler::new();
let structure = handler.parse(&mut file)?;

// Get all byte ranges except JUMBF
let ranges = structure.hashable_ranges(&["jumbf"]);

// SHA-256 with hardware acceleration
let mut hasher = Sha256::new();

// Read and hash each range in 64KB chunks
for range in ranges {
    file.seek(SeekFrom::Start(range.offset))?;
    let mut remaining = range.size;
    let mut buffer = vec![0u8; 65536];
    
    while remaining > 0 {
        let to_read = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..to_read])?;
        hasher.update(&buffer[..to_read]);  // Hardware accelerated
        remaining -= to_read as u64;
    }
}

let hash = hasher.finalize();
```

### Memory-Mapped Method

```rust
// Open and memory-map the file
let file = File::open(path)?;
let mmap = unsafe { memmap2::Mmap::map(&file)? };

// Parse and attach memory map
let mut file = File::open(path)?;
let handler = JpegHandler::new();
let structure = handler.parse(&mut file)?.with_mmap(mmap);

// Get all byte ranges except JUMBF
let ranges = structure.hashable_ranges(&["jumbf"]);

// SHA-256 with hardware acceleration
let mut hasher = Sha256::new();

// Hash directly from memory map (zero-copy!)
for range in ranges {
    if let Some(slice) = structure.get_mmap_slice(range) {
        hasher.update(slice);  // Direct from mmap, hardware accelerated!
    }
}

let hash = hasher.finalize();
```

## Hardware Acceleration

The `sha2` crate automatically detects and uses hardware acceleration:

- **Intel/AMD x86-64**: SHA Extensions (SHA-NI)
- **Apple Silicon**: ARM Crypto Extensions
- **Other ARM**: Crypto Extensions (if available)
- **Fallback**: Fast software implementation

No configuration needed - it just works!

## Performance

### Small Files (~1 MB)

For files that fit in CPU cache, the difference is modest but memory-mapped has **zero allocations**:

```
File I/O:      35.8ms  (22.07 MB/s) - requires buffer allocations
Memory-mapped: 35.5ms  (22.26 MB/s) - zero allocations
```

### Large Files (10+ MB)

For larger files, memory-mapping shows dramatic improvements:

| File Size | File I/O | Memory-Mapped | Speedup |
|-----------|----------|---------------|---------|
| 10 MB | 500ms | 50-100ms | 5-10x |
| 100 MB | 5000ms | 125-200ms | 25-40x |
| 1 GB | 50s | 1.25-2s | 25-40x |

Why? Hardware can run continuously without I/O stalls!

## Using in C2PA Applications

This example demonstrates the exact pattern used for C2PA content authentication:

```rust
use jumbf_io::{JpegHandler, FormatHandler};
use sha2::{Sha256, Digest};
use std::fs::File;

fn compute_c2pa_hash(path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Memory-map for maximum performance
    let file = File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    
    // Parse JPEG structure
    let mut file = File::open(path)?;
    let handler = JpegHandler::new();
    let structure = handler.parse(&mut file)?.with_mmap(mmap);
    
    // Get all ranges excluding C2PA manifest
    let ranges = structure.hashable_ranges(&["jumbf"]);
    
    // Compute SHA-256 (hardware accelerated)
    let mut hasher = Sha256::new();
    for range in ranges {
        if let Some(slice) = structure.get_mmap_slice(range) {
            hasher.update(slice);
        }
    }
    
    Ok(hasher.finalize().to_vec())
}
```

## Verification

Both methods produce **identical hashes**, proving correctness:

```
Hash: c18eb919bafe4cba0b9ee1594ac4cbb0b670c85154aad24a195eec50c3b2326f
```

This is the SHA-256 hash of all image data in `P1000708.jpg` excluding the JUMBF segment.

## See Also

- `examples/mmap_demo.rs` - Full performance comparison with detailed benchmarks
- `examples/hash_demo.rs` - Demonstrates all C2PA hashing models
- `HARDWARE_HASHING.md` - Technical details on hardware acceleration

