# Safety & Security

This document describes the security measures implemented in `asset-io` to safely handle untrusted input files.

## Overview

The library is designed to safely parse potentially malicious media files without:
- **Crashes or panics**: All errors are handled gracefully
- **Excessive memory allocation**: DOS attacks via huge allocations are prevented
- **Buffer overruns**: Rust's memory safety + explicit bounds checking
- **Integer overflows**: Checked arithmetic where needed

## Security Measures

### 1. Maximum Segment Size (DOS Prevention)

**Protection**: Prevents allocation-based DOS attacks

```rust
pub const MAX_SEGMENT_SIZE: u64 = 256 * 1024 * 1024; // 256 MB
```

**Location**: `src/segment.rs:18-27`, enforced in `LazyData::load()`

**Rationale**:
- Legitimate XMP: Usually < 1 MB
- Legitimate JUMBF: Usually < 10 MB
- 256 MB allows large legitimate content while preventing multi-GB allocations

**Example**: A malicious JPEG claiming a 2GB segment will be rejected before allocation.

### 2. PNG Chunk Length Validation

**Protection**: Prevents oversized PNG chunks

```rust
if chunk_len > 0x7FFFFFFF {
    return Err(Error::InvalidSegment {
        offset,
        reason: format!("Chunk length too large: {}", chunk_len),
    });
}
```

**Location**: `src/formats/png.rs:116-121`

**Rationale**: PNG chunks should never exceed 2GB. This is checked during parsing.

### 3. Extended XMP Size Limit

**Protection**: Prevents DOS via multi-segment XMP reassembly

```rust
const MAX_XMP_SIZE: u32 = 100 * 1024 * 1024; // 100 MB
```

**Location**: `src/formats/jpeg.rs:84-92`

**Rationale**: JPEG Extended XMP can span multiple segments. Malicious files could claim
huge total sizes. 100 MB is generous for XMP while preventing excessive allocation.

### 4. TIFF IFD Tag Count Limit

**Protection**: Prevents DOS via excessive EXIF tags

```rust
const MAX_IFD_TAGS: u16 = 1000;
```

**Location**: `src/tiff.rs:75-79`

**Rationale**: A legitimate EXIF IFD rarely has > 100 tags. The 1000 limit prevents
malicious files from claiming 65535 tags and causing excessive seeking/parsing.

### 5. TIFF Offset Validation

**Protection**: Prevents reading outside buffer bounds

```rust
// Validate IFD offset is within bounds
if ifd0_offset as usize >= exif_data.len() {
    return Ok(None); // Invalid offset
}
```

**Location**: `src/tiff.rs:109-112, 119-122`

**Rationale**: EXIF data contains offsets to IFDs. These must be validated before use
to prevent out-of-bounds reads.

### 6. Memory-Mapped Bounds Checking

**Protection**: Prevents panics on out-of-bounds mmap access

```rust
pub fn get_mmap_slice(&self, range: ByteRange) -> Option<&[u8]> {
    self.mmap.as_ref().and_then(|mmap| {
        let start = range.offset as usize;
        let end = start.checked_add(range.size as usize)?;  // Overflow check
        mmap.get(start..end)  // Returns None instead of panicking
    })
}
```

**Location**: `src/structure.rs:56-67`

**Rationale**: Uses `checked_add` for overflow protection and `slice.get()` instead of
indexing to return `None` rather than panicking on invalid ranges.

### 7. Integer Overflow Protection

**Protection**: Prevents wraparound in size calculations

**Examples**:
- `saturating_sub()` in JPEG XT header size calculation (`jpeg.rs:164`)
- `checked_add()` in memory-mapped slice bounds (`structure.rs:63`)
- All u64 offsets prevent 32-bit overflow issues

### 8. Graceful Error Handling

**Protection**: All parsing errors return `Result` types

**Rationale**: No `unwrap()` or `expect()` in parsing paths. All errors from untrusted
input are handled gracefully without panicking.

## Testing Strategy

### Current Tests

Basic validation tests in `tests/safety_test.rs` verify:
- Safety constants are defined correctly
- Bounds checking returns `None` appropriately
- Overflow checks work

### Recommended: Fuzzing

For comprehensive security testing, use `cargo-fuzz`:

```bash
# Install cargo-fuzz
cargo install cargo-fuzz

# Create fuzz targets
cargo fuzz init

# Fuzz JPEG parsing
cargo fuzz run jpeg_parse

# Fuzz PNG parsing  
cargo fuzz run png_parse
```

**Suggested fuzz targets**:
1. `jpeg_parse`: Feed random bytes to `JpegHandler::parse()`
2. `png_parse`: Feed random bytes to `PngHandler::parse()`
3. `xmp_extract`: Fuzz XMP extraction with malformed multi-segment data
4. `exif_thumbnail`: Fuzz EXIF parsing with malformed TIFF structures

## Known Limitations

1. **Memory-mapped safety**: Requires `unsafe` for `memmap2::Mmap::map()`, but this is
   a well-audited crate and all slice access is bounds-checked.

2. **File descriptor limits**: Large files with memory mapping can exhaust FD limits.
   This is an OS resource limit, not a security issue.

3. **Compression bombs**: The library doesn't decompress image data, so it's not
   vulnerable to decompression bombs. However, it will parse files with compressed
   data segments without validating the decompressed size.

## Security Best Practices

When using this library:

1. **Validate file sources**: Don't trust user-uploaded files
2. **Set resource limits**: Use OS-level limits (ulimit, cgroups) for defense in depth
3. **Isolate parsing**: Consider parsing untrusted files in sandboxed processes
4. **Monitor memory**: Watch for unusual memory growth patterns
5. **Update regularly**: Security fixes will be prioritized

## Reporting Security Issues

Please report security vulnerabilities privately to the maintainers before public disclosure.

## Audit Status

- ✅ No unsafe code except well-audited `memmap2`
- ✅ All parsing paths use graceful error handling
- ✅ DOS-prevention limits in place
- ✅ Bounds checking on all untrusted offsets/sizes
- ✅ Integer overflow protection
- ⏳ Fuzzing recommended before production use

Last updated: 2025-12-23


