# JPEG JUMBF Parser Fix

## Problem

The c2pa example was failing with `JumbfNotFound` error. Investigation revealed multiple issues in how we handle JPEG XT format JUMBF:

1. **Parser incorrectly positioned ranges**: When parsing JPEG XT format JUMBF, the parser was pointing ranges to the payload start (after `FF EB` + length), but NOT skipping the JPEG XT header. This meant ranges pointed to the "JP" header bytes, not the actual JUMBF data.

2. **Extraction double-skipped headers**: The extraction code was written for the old parser behavior, so it tried to skip JPEG XT headers again, causing double-skipping and returning truncated data.

3. **Continuation segments not handled**: JPEG XT continuation segments (sequence number > 1) repeat the first 8 bytes of the JUMBF superbox (LBox+TBox) after the JPEG XT header. The parser was only skipping 8 bytes for ALL segments, not 16 bytes for continuations.

## Root Cause

The fundamental issue: **c2pa-rs provides raw JUMBF boxes** (not JPEG XT wrapped, not APP11 wrapped). Our code must:
1. **Write**: Wrap raw JUMBF in JPEG XT format with proper headers
2. **Parse**: Skip JPEG XT headers to extract pure JUMBF data  
3. **Extract**: Return pure JUMBF data (what c2pa-rs gave us)

## Solution

### Part 1: Fix Parser Range Calculation

Updated the parser to correctly skip JPEG XT headers when creating byte ranges:

```rust
let (jumbf_data_offset, jumbf_data_size) = if is_jpeg_xt {
    const JPEG_XT_HEADER_SIZE: u64 = 8;
    const REPEATED_LBOX_TBOX_SIZE: u64 = 8;
    
    // Extract sequence number to detect continuation
    let seq_num = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    
    // Continuation segments have extra overhead (JP header + repeated LBox/TBox)
    let overhead = if seq_num > 1 {
        JPEG_XT_HEADER_SIZE + REPEATED_LBOX_TBOX_SIZE  // 16 bytes
    } else {
        JPEG_XT_HEADER_SIZE  // 8 bytes
    };
    
    (data_start + overhead, data_size - overhead)
} else {
    (data_start, data_size)
};
```

### Part 2: Simplify Extraction

Since the parser now provides ranges that point directly to JUMBF data, extraction just reads the bytes:

```rust
pub fn extract_jumbf_impl<R: Read + Seek>(
    structure: &crate::structure::Structure,
    source: &mut R,
) -> Result<Option<Vec<u8>>> {
    let mut result = Vec::new();

    for &index in structure.jumbf_indices() {
        let segment = &structure.segments()[index];
        for range in &segment.ranges {
            source.seek(SeekFrom::Start(range.offset))?;
            let mut buf = vec![0u8; range.size as usize];
            source.read_exact(&mut buf)?;
            result.extend_from_slice(&buf);
        }
    }

    Ok(if result.is_empty() { None } else { Some(result) })
}
```

### Part 3: Keep Existing JUMBF

For round-tripping existing JUMBF, we:
1. Extract the raw JUMBF data (parser already skipped headers)
2. Pass it to `write_jumbf_segments` which wraps it in JPEG XT format
3. This ensures consistent formatting regardless of original chunking

```rust
crate::MetadataUpdate::Keep => {
    if let Some(source_seg) = structure.segments.iter().find(|s| s.is_jumbf()) {
        // Read JUMBF data (ranges point to pure JUMBF, no headers)
        let total_size: u64 = source_seg.ranges.iter().map(|r| r.size).sum();
        let mut jumbf_data = vec![0u8; total_size as usize];
        // ... read from ranges ...
        
        // Re-write with proper JPEG XT formatting
        write_jumbf_segments(writer, &jumbf_data)?;
    }
}
```

## JPEG XT Continuation Format

For reference, JPEG XT continuation segments have this structure:
- `FF EB` - APP11 marker
- Length (2 bytes)
- `4A 50 02 11` - "JP" + En
- Sequence number (4 bytes, > 1 for continuations)
- **Repeated LBox+TBox (8 bytes)** - first 8 bytes of JUMBF superbox
- Continuation data

## Test Coverage

All 7 tests in `test_all_combinations` pass:
- ✅ `test_add_jumbf_to_file_without_jumbf` - Adding new JUMBF works
- ✅ `test_keep_existing_jumbf` - Round-tripping multi-segment JUMBF preserves data exactly
- ✅ `test_add_both_xmp_and_jumbf` - Adding both metadata types works
- ✅ `test_add_xmp_to_file_without_xmp` - XMP operations still work
- ✅ `test_remove_xmp` - Removal operations work
- ✅ `test_replace_xmp` - Replacement operations work
- ✅ `test_round_trip_no_changes` - No-op writes preserve files

The `c2pa` example now successfully:
- Writes placeholder manifest with JPEG XT wrapping
- Parses output to get correct byte ranges for overwriting
- Allows c2pa Reader to validate the final manifest

## Verification

Before fix:
```bash
$ cargo run --example c2pa --features jpeg,xmp,hashing earth_apollo17.jpg
Error: JumbfNotFound
```

After fix:
```bash
$ cargo run --example c2pa --features jpeg,xmp,hashing earth_apollo17.jpg
(Success - exit code 0)
```

Verification of JPEG XT format in output:
```bash
$ xxd -l 32 -s 28796 target/output_c2pa.jpg
0000707c: ffeb 4397 4a50 0211 0000 0001 0000 438d  ..C.JP........C.
0000708c: 6a75 6d62 0000 001e 6a75 6d64 6332 7061  jumb....jumdc2pa
                  ^^^^-- "JP" header preserved
                              ^^^^^^^^-- JUMBF starts after
```

## Key Insight

The solution required understanding the complete data flow:
1. c2pa-rs gives us **raw JUMBF boxes**
2. We write them wrapped in **JPEG XT format** (with "JP" headers)
3. We parse to create ranges pointing **after the JP headers** (to pure JUMBF)
4. We extract by reading **at the range offsets** (pure JUMBF)
5. c2pa-rs overwrites **at the range offsets** (preserving JP headers)

This ensures byte-for-byte compatibility with c2pa-rs expectations.

