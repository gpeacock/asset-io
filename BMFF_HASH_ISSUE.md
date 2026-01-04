# BMFF Hash Mismatch Issue - Root Cause Analysis

## Problem
All BMFF format files (HEIC, HEIF, M4A, AVIF) fail C2PA verification with `assertion.bmffHash.mismatch` error when using the asset-io `c2pa.rs` example.

## Root Cause
**asset-io is computing BMFF V1 hashes, but c2pa-rs verifier expects BMFF V2 hashes.**

### BMFF V1 vs V2 Hashing

#### BMFF V1 (What asset-io currently does):
- Hashes the entire file content
- Excludes only the mandatory boxes: `/ftyp`, `/uuid[c2pa]`, `/mfra`
- Simple byte-level exclusion during streaming write

#### BMFF V2 (What c2pa-rs expects):
- For top-level boxes, hashes only their **8-byte offset values** (not their content)
- Still excludes mandatory boxes entirely
- Significantly faster for large files with big `mdat` boxes
- Requires parsing the BMFF structure to identify top-level boxes

## Technical Details

### How c2pa-rs implements BMFF V2:

1. **In `generate_bmff_data_hash_for_stream` (store.rs:2276)**:
   - Creates BmffHash with exclusions for `/ftyp`, `/uuid[c2pa]`, `/mfra`
   - Sets `bmff_version` to 2 for non-update manifests (line 3200)

2. **In `bmff_to_jumbf_exclusions` (bmff_io.rs:472)**:
   - Converts BMFF xpath-style exclusions to flat `HashRange` objects
   - **Critical**: For BMFF V2, adds special hash ranges for top-level box offsets (lines 621-627):
     ```rust
     if bmff_v2 {
         for tl_start in tl_offsets {
             let mut exclusion = HashRange::new(tl_start, 1u64);
             exclusion.set_bmff_offset(tl_start);  // Special marker!
             exclusions.push(exclusion);
         }
     }
     ```

3. **In `hash_stream_by_alg` (hash_utils.rs:192)**:
   - When a HashRange has `bmff_offset` set (lines 245-247, 375-377):
     ```rust
     if bmff_v2_starts.contains(start) && end == start {
         hasher_enum.update(&start.to_be_bytes());  // Hash the OFFSET, not the data!
         continue;
     }
     ```
   - This replaces hashing the box content with just hashing its 8-byte offset value

### What asset-io currently does:

In `bmff_io.rs::write_with_processor` (lines 1246-1446):
- Excludes `/ftyp` (lines 1346-1350)
- Excludes C2PA UUID box if `should_exclude_jumbf` (lines 1372-1377)
- Excludes `/mfra` boxes (lines 1432-1435)
- **But**: Hashes all other boxes normally (V1 style)
- **Missing**: The V2 offset-only hashing for top-level boxes

## Solution Options

### Option 1: Implement BMFF V2 hashing in asset-io (Recommended)
**Pros:**
- Faster hashing for large files
- Matches c2pa-rs default behavior
- Industry standard for C2PA

**Cons:**
- More complex implementation
- Requires tracking top-level box offsets during streaming write
- Need to inject offset values into hash stream

**Implementation approach:**
1. During BMFF parsing/writing, track all top-level box offsets
2. Modify `ProcessingWriter` to support "offset-only" mode
3. When writing top-level boxes (except excluded ones), hash their 8-byte offset instead of content
4. Update c2pa example to use V2-style BmffHash creation

### Option 2: Force BMFF V1 in the example
**Pros:**
- Simpler, current implementation already works for V1
- No changes to asset-io needed

**Cons:**
- Slower for large files
- Non-standard (c2pa-rs defaults to V2)
- Would need to force c2pa Builder to use V1

**Implementation approach:**
1. Create BmffHash with `set_bmff_version(1)`
2. Ensure c2pa-rs respects the version (it should during verification)

### Option 3: Hybrid - Support both V1 and V2
**Pros:**
- Maximum flexibility
- Can choose based on file size

**Cons:**
- Most complex
- Need to maintain both code paths

## Verification

The debug output from our failing tests confirms this:
```
DEBUG: JUMBF segment has 2 range(s)
  Range 0: offset=69, size=17373
  Range 1: offset=24, size=17418
```

C2PA verification fails because:
- We computed hash over actual file bytes (V1 style)
- c2pa-rs verifier recomputes hash over offsets (V2 style)
- Hashes don't match → `assertion.bmffHash.mismatch`

## Recommendation

Implement **Option 1: BMFF V2 hashing** because:
1. It's the c2pa-rs default and industry standard
2. Performance benefits for large video files
3. Aligns with C2PA spec best practices

The implementation would need:
- Modify `write_with_processor` to track top-level box positions
- Create a mechanism to inject offset bytes into the hash stream
- Update the c2pa example to properly configure V2 hashing
