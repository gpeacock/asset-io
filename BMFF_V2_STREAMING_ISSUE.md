# BMFF V2 Streaming Incompatibility

## Problem

BMFF V2 hashing is **fundamentally incompatible** with single-pass write-and-hash streaming.

## Why V2 Doesn't Work for Streaming

### BMFF V2 Requirements:
- Hash 8-byte offset values of top-level boxes (not their content)
- Example: `moov` box at offset 12345 → hash `0x0000000000003039`

### Streaming Write Problem:
1. We hash data AS we write it
2. Box offsets in the output depend on what we've written so far
3. When we write XMP/C2PA UUID boxes, all subsequent boxes shift
4. We can't know the FINAL offsets until AFTER writing completes
5. But we need those offsets IN the hash we compute DURING writing

### c2pa-rs Approach (Two-Pass):
```
1. Write entire file to intermediate stream (without C2PA manifest)
2. Read that stream and compute hash with final offsets  
3. Create manifest with that hash
4. Embed manifest into file
```

### Our Approach (Single-Pass - Incompatible with V2):
```
1. Write and hash simultaneously
2. Need offsets DURING write (impossible - they're not final yet!)
3. Update manifest in-place after
```

## Solution Options

### Option A: Use BMFF V1 for Streaming (Recommended)
- V1 hashes actual bytes (just excluding certain boxes)
- Compatible with single-pass write-and-hash
- Slower for large files but works correctly
- **Need to tell c2pa-rs to use V1** by setting `bmff_version = 1`

###Option B: Give Up Single-Pass for BMFF
- Write file first
- Compute hash second (with V2 offsets from structure)
- Loses performance benefit of streaming
- Would match c2pa-rs exactly

### Option C: Hybrid
- Use V1 for streaming mode
- Provide separate V2 API for non-streaming
- More complex but flexible

## Recommended Fix

Modify the c2pa example to create BMFF V1 hashes for streaming:

```rust
fn create_dummy_bmff_hash() -> BmffHash {
    let mut bmff_hash = BmffHash::new("jumbf manifest", "sha256", None);
    bmff_hash.set_bmff_version(1); // Force V1 for streaming compatibility
    
    // Add exclusions...
    bmff_hash
}
```

This will:
- ✅ Work with single-pass streaming
- ✅ Verify correctly with c2pa-rs
- ❌ Be slower for large files (hashes all bytes, not just offsets)
- ✅ Still much faster than traditional approach (no reopen/reread)

## Why This Matters

The current implementation tries to do V2 hashing during streaming, which:
- Hashes wrong offset values (from source file, not destination)  
- Or hashes at wrong times (before final positions known)
- Results in hash mismatches during verification
