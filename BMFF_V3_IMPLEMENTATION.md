# BMFF V3 Merkle Tree Implementation Status

## Current Status

### ✅ What Works
1. **Sequential V3** - Works but creates 1-leaf Merkle (essentially V2)
2. **Parallel infrastructure** - Fixed chunking logic for virtual continuous data
3. **Integration** - c2pa-rs `asset-io-integration` branch APIs working

### ❌ What's Missing for True V3
**Proper fixed-block Merkle hashing** - Need to hash only mdat box content, not entire file

## How C2PA V3 Fixed-Block Hashing Works

From c2pa-rs code analysis (`bmff_hash.rs` lines 1296-1313, 1469-1488):

```rust
// For each mdat box:
let mut block_start = mdat_box_offset + 16;  // Skip mdat header (8-16 bytes)
let mut bytes_left = mdat_box_size - 16;

while bytes_left > 0 {
    let leaf_length = min(bytes_left, fixed_block_size);  // e.g. 1MB
    let hash = hash_block(block_start, leaf_length);
    merkle_leaves.push(hash);
    
    bytes_left -= leaf_length;
    block_start += leaf_length;
}

// Build Merkle tree from leaves
let merkle_root = build_merkle_tree(merkle_leaves);
```

### Key Points:
1. **Only mdat boxes are hashed** - Not ftyp, moov, meta, uuid, etc.
2. **Fixed-size chunks** - 1MB blocks within mdat data
3. **Merkle tree** - Built from chunk hashes
4. **MerkleMap fields**:
   - `fixed_block_size: Some(1024 * 1024)`
   - `count: num_leaves`
   - `hashes`: Vec containing Merkle tree hashes

## Current Implementation Issues

### Our Sequential Version
```rust
// Creates V3 label but does V2-style hashing
bmff_hash.gen_hash_from_stream(output)?;
// Result: 1-leaf Merkle = entire file hash (minus exclusions)
// Missing: fixed_block_size in MerkleMap
```

### Our Parallel Version  
```rust
// Hashes entire file (minus JUMBF) in 1MB chunks
let chunk_hashes = output_asset.parallel_hash_mmap::<Sha256>(&hash_updates)?;
// Problem: Hashing wrong data (should be mdat only)
// Also: Need to track mdat box locations
```

## Path to Full V3 Implementation

### Option 1: Track mdat Boxes (Proper Solution)
1. **Update bmff_io.rs** to track mdat boxes as `SegmentKind::ImageData`
2. **Filter for mdat segments** when hashing
3. **Hash only mdat content** (skip 8-16 byte headers)
4. **Build Merkle tree** from resulting chunks

```rust
// Pseudo-code:
let mdat_segments = structure.segments
    .iter()
    .filter(|s| s.is_type(SegmentKind::ImageData));

for mdat in mdat_segments {
    let data_start = mdat.offset + 16;  // Skip mdat header
    let data_size = mdat.size - 16;
    
    // Hash in 1MB chunks within this mdat
    let mdat_chunks = hash_range_in_chunks(data_start, data_size, 1MB);
    all_chunks.extend(mdat_chunks);
}

let merkle_root = merkle_root(&all_chunks);
```

### Option 2: Parse Output for mdat (Quick Hack)
1. **After writing**, parse BMFF structure to find mdat boxes
2. **Use existing parallel_hash_mmap** with calculated mdat ranges
3. **Set fixed_block_size** in MerkleMap

### Option 3: Match Current c2pa-rs Behavior (Easiest)
Keep sequential version as-is (1-leaf Merkle). It verifies correctly and is simpler.

## Recommendation

**For now:** Use sequential version (works, verifies)

**For full V3:** Implement Option 1:
- Add mdat tracking to bmff_io
- Update parallel_hash_mmap to support "hash only these segments"
- Enables true multi-chunk Merkle for large files

## Benefits of True V3

1. **Parallel verification** - Can verify chunks in parallel
2. **Streaming** - Don't need entire file in memory
3. **Incremental** - Can verify partial downloads
4. **Performance** - Faster for large video files

## File Changes Needed

### src/containers/bmff_io.rs
```rust
// When parsing structure, identify mdat boxes
if box_type == "mdat" {
    structure.add_segment(Segment::new(
        box_offset,
        box_size,
        SegmentKind::ImageData,  // Mark as ImageData
        Some("mdat".to_string())
    ));
}
```

### src/asset.rs
```rust
// Add method to hash specific segment types
pub fn parallel_hash_segments<H>(
    &self,
    segment_kind: SegmentKind,
    chunk_size: usize,
    skip_header_bytes: u64,  // Skip mdat header
) -> Result<Vec<[u8; 32]>>
```

### examples/c2pa.rs
```rust
// Use new API for V3
let chunk_hashes = output_asset.parallel_hash_segments::<Sha256>(
    SegmentKind::ImageData,  // mdat boxes only
    1024 * 1024,             // 1MB chunks
    16,                       // Skip mdat header
)?;
```

## Testing

Once implemented, verify with:
```bash
# Create with V3
cargo run --example c2pa --features all-formats,parallel,memory-mapped,xmp large_file.heic

# Verify structure
c2patool target/output_c2pa.heif --detailed | grep -A 20 "bmffHash"

# Should show:
# - fixedBlockSize: 1048576
# - count: N (multiple leaves for large files)
# - hash validation passes
```

## Current Workaround

Sequential version works for now:
- ✅ Creates valid C2PA manifests
- ✅ Passes verification
- ✅ Labeled as V3
- ❌ Only 1 Merkle leaf (not true multi-chunk)
- ❌ No parallel verification benefits

Good enough for small files, but true V3 needed for large video files where parallel verification matters.
