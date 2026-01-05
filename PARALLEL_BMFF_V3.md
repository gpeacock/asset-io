# Parallel BMFF V3 Hashing - Implementation Notes

## Summary

**BMFF V3 (c2pa.hash.bmff.v3) enables parallel hashing!** This document describes the current status and path forward for leveraging asset-io's parallel infrastructure.

## Current Status ✅

### What's Working
- ✅ BMFF V3 Merkle tree support enabled (via `merkle_tree_chunk_size_in_kb` setting)
- ✅ All BMFF files (HEIC, AVIF, M4A) signing and verifying correctly
- ✅ `assertion.bmffHash.match` - no hash mismatches
- ✅ Parallel hashing infrastructure tested and working (3.45x speedup on small files)

### Current Performance
- **Sequential hashing**: Using `c2pa-rs`'s `BmffHash::gen_hash_from_stream()`
- **asset-io (V3)**: 66.27s on 6.3GB file
- **c2patool**: 22.84s on 6.3GB file  
- **Result**: Currently 2.9x slower (but hash verification passes!)

## The V3 Advantage

### V2 vs V3
| Feature | V2 | V3 |
|---------|----|----|
| **Method** | Hash 8-byte offsets of top-level boxes | Hash actual data chunks |
| **Parallel** | ❌ No - sequential offsets required | ✅ Yes - independent chunks |
| **Structure** | Flat hash | Merkle tree |
| **Best For** | Simple assets | Large files, streaming |

### Why V3 Enables Parallelization
- **V2**: Must hash box offsets sequentially (need final positions)
- **V3**: Hashes actual data chunks which can be processed independently

## asset-io Parallel Infrastructure

We have all the pieces ready:

```rust
// 1. Parallel hash with memory-mapped I/O
let chunk_hashes = asset.parallel_hash_mmap::<Sha256>(&updates)?;

// 2. Build Merkle tree root  
let merkle_root = merkle_root::<Sha256>(&chunk_hashes);

// 3. Create V3 MerkleMap structure
let merkle_map = MerkleMap {
    unique_id: 0,
    local_id: 0,
    count: chunk_hashes.len(),
    alg: Some("sha256".to_string()),
    hashes: VecByteBuf(vec![ByteBuf::from(merkle_root.to_vec())]),
    fixed_block_size: Some(1024 * 1024),  // 1MB chunks
    variable_block_sizes: None,
};

// 4. Set in BmffHash
bmff_hash.set_merkle(vec![merkle_map]);
```

## Implementation Challenges

### What We Need to Handle
1. **BMFF box parsing** - Find mdat boxes to hash
2. **C2PA exclusions** - Exclude ftyp, mfra, C2PA UUID boxes
3. **Merkle proof UUIDs** - Embed proof hashes for large files
4. **API integration** - Work with c2pa-rs Builder/Signer APIs

### Current Blocker
The `c2pa::Builder` API for creating manifests with pre-computed hashes requires:
- `data_hashed_placeholder()` instead of `unsigned_manifest_placeholder()`
- `sign_data_hashed_embeddable()` instead of `sign_manifest()`
- Different signing workflow

## Path Forward

### Option 1: Use c2pa-rs's V3 API (Current Approach)
**Pros:**
- ✅ Working now
- ✅ Fully compliant
- ✅ c2pa-rs handles all complexity

**Cons:**
- ❌ Sequential hashing (slow)
- ❌ Can't leverage our parallel infrastructure

### Option 2: Implement V3 in asset-io (Future)
**Pros:**
- ✅ 3-10x faster (parallel hashing)
- ✅ Leverage mmap, rayon, chunk processing
- ✅ Single-pass write + hash

**Cons:**
- ❌ Complex: BMFF parsing, exclusions, proof UUIDs
- ❌ Must match c2pa-rs's V3 structure exactly
- ❌ Significant development effort

### Option 3: Hybrid Approach (Recommended Next Step)
1. **Keep current implementation as fallback**
2. **Add parallel V3 path when features enabled**:
   ```rust
   #[cfg(all(feature = "parallel", feature = "memory-mapped"))]
   {
       // Use asset-io parallel hashing
       let chunk_hashes = asset.parallel_hash_mmap::<Sha256>(&updates)?;
       let merkle_root = merkle_root::<Sha256>(&chunk_hashes);
       // Create V3 MerkleMap...
   }
   #[cfg(not(all(feature = "parallel", feature = "memory-mapped")))]
   {
       // Fallback to c2pa-rs sequential
       bmff_hash.gen_hash_from_stream(output)?;
   }
   ```
3. **Use c2pa Builder's data-hashed API** for pre-computed hashes

## Benchmarking Results

### Parallel Hashing (parallel_hash example on sample1.heic)
- **Sequential**: 1.20 ms
- **Parallel (with file handles)**: 347 µs (3.45x faster)
- **Parallel (with mmap)**: 431 µs (2.78x faster)

### Full C2PA Signing (tearsofsteel_4k.mov - 6.3GB)
- **asset-io (V3 sequential)**: 66.27s
- **c2patool**: 22.84s
- **Theoretical with parallel**: ~10-20s (estimated 3-6x speedup)

## Next Steps

1. ✅ **DONE**: Verify V3 Merkle support works
2. ✅ **DONE**: Verify parallel hashing infrastructure works
3. ⏭️ **TODO**: Update c2pa example to use `data_hashed_placeholder` API
4. ⏭️ **TODO**: Integrate parallel hashing with V3 Merkle creation
5. ⏭️ **TODO**: Benchmark on large files (expect 3-6x speedup)
6. ⏭️ **TODO**: Handle Merkle proof UUID boxes for very large files

## Conclusion

We have **working V3 support** that's fully compliant and verified. The parallel infrastructure is ready and tested. The remaining work is integrating them using c2pa-rs's data-hashed API to unlock the full performance potential.

**Expected outcome**: 3-6x faster BMFF signing on large files while maintaining full C2PA compliance.
