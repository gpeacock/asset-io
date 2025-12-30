//! Test memory-mapped file access and zero-copy hashing

use asset_io::{Asset, ByteRange};

fn main() -> asset_io::Result<()> {
    #[cfg(all(feature = "test-utils", feature = "memory-mapped"))]
    {
        use asset_io::test_utils::{fixture_path, P1000708};

        let path = fixture_path(P1000708);
        println!("Testing memory-mapped file: {}", path.display());

        // Open with memory mapping
        let asset = unsafe { Asset::open_with_mmap(&path)? };
        println!("Memory-mapped {} bytes", asset.structure().total_size);
        println!("Found {} segments", asset.structure().segments.len());

        // Test zero-copy access to header
        let header_range = ByteRange::new(0, 2);
        if let Some(slice) = asset.structure().get_mmap_slice(header_range) {
            println!("Header: {:02X} {:02X} (zero-copy)", slice[0], slice[1]);
            assert_eq!(slice[0], 0xFF);
            assert_eq!(slice[1], 0xD8);
        }

        // Test zero-copy hashing
        let ranges = asset.structure().hashable_ranges(&["jumbf"]);
        println!("Hashable ranges: {}", ranges.len());

        let mut hash_state = 0u64;
        for range in ranges {
            if let Some(slice) = asset.structure().get_mmap_slice(range) {
                // Simple hash for testing
                for &byte in slice {
                    hash_state = hash_state.wrapping_mul(31).wrapping_add(byte as u64);
                }
            }
        }
        println!("Hash: {:016x}", hash_state);

        println!("✓ Memory-mapped access successful");
    }

    #[cfg(not(all(feature = "test-utils", feature = "memory-mapped")))]
    {
        println!("Skipped: requires test-utils and memory-mapped features");
    }

    Ok(())
}
