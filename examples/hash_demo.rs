// Example demonstrating efficient hashing for C2PA
//
// This shows how to hash assets without loading entire segments into memory,
// supporting multiple C2PA hashing models.

use asset_io::{Asset, DEFAULT_CHUNK_SIZE};

fn main() -> asset_io::Result<()> {
    #[cfg(feature = "test-utils")]
    {
        use asset_io::test_utils::{fixture_path, FIREFLY_TRAIN};

        println!("=== C2PA Hashing Demo ===\n");

        let path = fixture_path(FIREFLY_TRAIN);
        println!("Using fixture: {}\n", path.display());

        // Demo 1: Data Hash - Hash entire file except C2PA segment
        println!("1. Data Hash Model (exclude C2PA segment)");
        println!("   Hashes entire asset except C2PA/JUMBF data");
        let hash1 = compute_data_hash(path.to_str().unwrap())?;
        println!("   Hash: {}\n", hex_encode(&hash1));

        // Demo 2: Box Hash - Hash specific segments by name
        println!("2. Box Hash Model (hash XMP and image data only)");
        println!("   Hashes only specific segments");
        let hash2 = compute_box_hash(path.to_str().unwrap())?;
        println!("   Hash: {}\n", hex_encode(&hash2));

        // Demo 3: Image Data Only
        println!("3. Image Data Hash");
        println!("   Hashes only the compressed image data");
        let hash3 = compute_image_hash(path.to_str().unwrap())?;
        println!("   Hash: {}\n", hex_encode(&hash3));

        // Demo 4: Streaming with explicit chunk size
        println!("4. Chunked Streaming (8KB chunks)");
        println!("   Demonstrates memory-efficient hashing");
        let hash4 = compute_chunked_hash(path.to_str().unwrap(), 8192)?;
        println!("   Hash: {}\n", hex_encode(&hash4));

        println!("=== All Hashing Methods Complete ===");
        println!("\nKey Features:");
        println!(
            "  ✓ Constant memory usage (only {:.1}KB buffers)",
            DEFAULT_CHUNK_SIZE as f64 / 1024.0
        );
        println!("  ✓ Streaming for large segments");
        println!("  ✓ Flexible exclusion patterns");
        println!("  ✓ Support for all C2PA hash models");
    }

    #[cfg(not(feature = "test-utils"))]
    {
        println!("This example requires the test-utils feature:");
        println!("  cargo run --example hash_demo");
    }

    Ok(())
}

/// Example 1: Data Hash - hash entire file except C2PA segment
///
/// This is the most common C2PA hash model - it hashes everything in the asset
/// except the C2PA manifest itself (which contains the hash).
fn compute_data_hash(path: &str) -> asset_io::Result<Vec<u8>> {
    let mut asset = Asset::open(path)?;

    // Get ranges excluding JUMBF (C2PA) segments
    let exclusions = vec!["jumbf"];
    let ranges = asset.structure().hashable_ranges(&exclusions);

    println!("   Hashing {} range(s):", ranges.len());
    for (i, range) in ranges.iter().enumerate() {
        println!(
            "     Range {}: offset={}, size={} bytes",
            i + 1,
            range.offset,
            range.size
        );
    }

    // Use a simple hasher (in real C2PA, this would be SHA-256, SHA-384, etc.)
    let mut hasher = SimpleHasher::new();

    // Now iterate and hash
    for range in ranges {
        // Stream the range in chunks (never loads entire range into memory!)
        let mut chunked = asset.read_range_chunked(range, DEFAULT_CHUNK_SIZE)?;

        while let Some(chunk) = chunked.next() {
            let chunk = chunk?;
            hasher.update(&chunk);
        }
    }

    Ok(hasher.finalize())
}

/// Example 2: Box Hash - hash specific segments by name
///
/// This model hashes specific segments (boxes in BMFF terminology), excluding
/// metadata segments.
fn compute_box_hash(path: &str) -> asset_io::Result<Vec<u8>> {
    let mut asset = Asset::open(path)?;

    // Exclude metadata segments
    let exclusions = vec!["jumbf", "xmp"];
    let segments: Vec<_> = asset
        .structure()
        .segments_excluding(&exclusions)
        .into_iter()
        .map(|(idx, seg, loc)| (idx, seg.path().to_string(), loc))
        .collect();

    println!("   Hashing {} segment(s):", segments.len());

    let mut hasher = SimpleHasher::new();

    for (index, path_str, loc) in segments {
        println!("     Segment {}: {} ({} bytes)", index, path_str, loc.size);

        // Stream each segment in chunks
        let mut chunked = asset.read_segment_chunked(index, DEFAULT_CHUNK_SIZE)?;

        while let Some(chunk) = chunked.next() {
            let chunk = chunk?;
            hasher.update(&chunk);
        }
    }

    Ok(hasher.finalize())
}

/// Example 3: Hash only image data
///
/// This demonstrates hashing a specific type of segment.
fn compute_image_hash(path: &str) -> asset_io::Result<Vec<u8>> {
    let mut asset = Asset::open(path)?;

    let image_segments: Vec<_> = asset
        .structure()
        .segments_by_path("image_data")
        .into_iter()
        .map(|(idx, _)| idx)
        .collect();

    println!("   Found {} image data segment(s)", image_segments.len());

    let mut hasher = SimpleHasher::new();

    for index in image_segments {
        let loc = asset.structure().segments[index].location();
        println!("     Hashing segment {} ({} bytes)", index, loc.size);

        // For small segments, we could load directly, but chunked is safer
        let mut chunked = asset.read_segment_chunked(index, DEFAULT_CHUNK_SIZE)?;

        while let Some(chunk) = chunked.next() {
            let chunk = chunk?;
            hasher.update(&chunk);
        }
    }

    Ok(hasher.finalize())
}

/// Example 4: Demonstrate custom chunk size
fn compute_chunked_hash(path: &str, chunk_size: usize) -> asset_io::Result<Vec<u8>> {
    let mut asset = Asset::open(path)?;

    println!("   Using {}KB chunk size", chunk_size / 1024);

    let exclusions = vec!["jumbf"];
    let ranges = asset.structure().hashable_ranges(&exclusions);

    let mut hasher = SimpleHasher::new();
    let mut total_bytes = 0u64;

    for range in ranges {
        let mut chunked = asset.read_range_chunked(range, chunk_size)?;

        while let Some(chunk) = chunked.next() {
            let chunk = chunk?;
            total_bytes += chunk.len() as u64;
            hasher.update(&chunk);
        }
    }

    println!(
        "   Processed {} bytes in {}KB chunks",
        total_bytes,
        chunk_size / 1024
    );

    Ok(hasher.finalize())
}

// ============================================================================
// Simple hasher implementation (for demo purposes)
// In real C2PA, you'd use SHA-256, SHA-384, etc.
// ============================================================================

struct SimpleHasher {
    state: u64,
}

impl SimpleHasher {
    fn new() -> Self {
        Self { state: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            // Simple hash (not cryptographic - just for demo!)
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
