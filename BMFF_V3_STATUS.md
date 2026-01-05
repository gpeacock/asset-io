# BMFF V3 Implementation Status

## ✅ Completed

### 1. Track mdat boxes as ImageData segments
- Added mdat box parsing in `bmff_io.rs` `parse_impl()` 
- mdat boxes now tracked as `SegmentKind::ImageData`
- Includes header size calculation (8 or 16 bytes for large size)
- Only stores DATA portion (skips mdat header)

### 2. Fixed ImageData handling in write logic  
- `calculate_updated_structure()` now filters for metadata-only segments
- Fixed `ftyp_end` calculation to use earliest metadata segment, not first segment
- Fixed `metadata_end_in_source` to only consider XMP/JUMBF/EXIF, not ImageData
- Write operations work correctly with ImageData segments present

### 3. Added `parallel_hash_segments()` method
- New method in `src/asset.rs` for hashing specific segment types
- Uses memory-mapped I/O with rayon for zero-copy parallel hashing
- Properly handles virtual continuous data across multiple ranges
- Splits data into fixed-size chunks (e.g., 1MB for V3)
- Returns vector of chunk hashes in order

### 4. Updated c2pa example for V3
- `sign_with_bmff_hash_parallel_v3()` function uses true V3 workflow
- Sets `fixed_block_size: Some(1024 * 1024)` in MerkleMap (CRITICAL for V3!)
- Calls `parallel_hash_segments()` to hash only mdat boxes
- Builds proper Merkle tree using `merkle_root()` function
- Sequential fallback still available for non-parallel builds

## ❌ Known Issues

### 1. Hardcoded output path in parallel workflow
**Problem**: Line 115 in `examples/c2pa.rs` hardcodes output path:
```rust
let output_path = std::path::Path::new("target/output_c2pa.heif");
```

**Impact**: Only works for HEIF files, breaks for M4A/MOV/etc.

**Fix needed**: Pass output path as parameter or infer from output writer

### 2. Output file mdat segments not found
**Problem**: After writing output file and re-opening with mmap, `parallel_hash_segments()` finds 0 or wrong mdat segments.

**Possible causes**:
- Wrong file path being opened (see issue #1)
- mdat boxes not being copied to output correctly
- Parse not finding mdat boxes in output

**Evidence**:
- Input `sample1.heic`: 1 ImageData segment at offset 542, size 293KB
- Output `target/output_c2pa.heif`: Only shows JUMBF segment, no ImageData
- File sizes: Input 287KB → Output 304KB (mdat data IS there, just not parsed)

**Debug needed**:
1. Verify correct output path is opened for mmap
2. Check if parse finds mdat in freshly written output file
3. Add logging to show what segments are found after re-open

### 3. Hash mismatch with c2patool
**Problem**: Parallel V3 generates different hash than c2patool expects

**Current behavior**:
- Sequential: `Hash: [68, 05, 41, 8f, ...]` → ✅ Validates
- Parallel V3: `Merkle root: [a0, 9c, b2, 69, ...]` → ❌ Hash mismatch

**Analysis**:
- Sequential is doing V2 hashing (whole file) labeled as V3
- Parallel is doing true V3 (mdat only, chunked)
- c2patool validates sequential because both use same algorithm
- c2patool doesn't validate parallel because hash is different

**Root cause**: Sequential `gen_hash_from_stream()` doesn't actually do V3 Merkle hashing, even when V3 is configured. It still hashes the entire file once.

**Path forward**: Once issues #1 and #2 are fixed:
- Test with larger files (>1MB mdat) to get multiple chunks
- Verify chunk count matches expectations
- May need to investigate c2patool's V3 verification logic
- Possibly need c2pa-rs V3 verification support

## Test Results

### Sequential V3 (using gen_hash_from_stream)
```bash
$ cargo run --example c2pa --features all-formats,xmp tests/fixtures/sample1.heic
✅ Success! (but actually doing V2 hash, just labeled V3)
```

### Parallel V3 (using parallel_hash_segments)
```bash
$ cargo run --example c2pa --features all-formats,parallel,memory-mapped,xmp tests/fixtures/sample1.heic
❌ Hash mismatch: assertion.bmffHash.mismatch
```

## Architecture

### V3 Workflow
```
1. Create BmffHash with dummy Merkle map (for size reservation)
2. Write file with placeholder JUMBF
3. Close output, reopen with mmap
4. Parse output to find mdat boxes → ImageData segments
5. parallel_hash_segments(ImageData, 1MB) → chunk hashes
6. merkle_root(chunk_hashes) → Merkle root hash
7. Update BmffHash with real Merkle map + root hash
8. Sign manifest
9. Update JUMBF in-place
```

### Key V3 Requirements (per C2PA spec)
- Hash ONLY mdat box content (not entire file)
- Use fixed-size chunks (e.g., 1MB)
- Build Merkle tree from chunk hashes
- Set `fixed_block_size` in MerkleMap
- Store Merkle root as the hash

### What Makes This "True V3"
- ✅ Hashes only mdat boxes (media data)
- ✅ Uses fixed-size chunks (1MB)
- ✅ Builds Merkle tree structure
- ✅ Sets fixed_block_size in MerkleMap
- ✅ Can be parallelized (multiple chunks)
- ❌ Not yet validated by c2patool (pending fixes)

## Next Steps

1. **Fix hardcoded output path**
   - Pass output path as parameter to `sign_with_bmff_hash_parallel_v3`
   - Or infer from media type

2. **Debug mdat segment discovery in output**
   - Add logging in `parallel_hash_segments` to show what segments are found
   - Verify output file parse finds mdat boxes
   - Confirm correct file is being opened

3. **Test with larger files**
   - Use M4A (3.9MB mdat) to get multiple chunks
   - Verify chunk count is correct (should be ~4 chunks for 3.9MB / 1MB)
   - Check Merkle tree has multiple leaves

4. **Investigate c2patool V3 verification**
   - Understand why hash mismatch occurs
   - May need to wait for c2pa-rs V3 verification support
   - Or adjust our hashing to match expected behavior

## References

- BMFF V3 spec: C2PA 2.1+ Section on Merkle tree hashing
- Implementation doc: `BMFF_V3_IMPLEMENTATION.md`
- Parallel infrastructure doc: `PARALLEL_BMFF_V3.md`
