# C2PA Embeddable API Example

## Overview

The new `c2pa_embeddable.rs` example demonstrates the c2pa-rs 0.77.0+ embeddable signing API, which provides explicit control over the C2PA signing workflow with clean separation between asset I/O (asset-io) and signing logic (c2pa-rs).

## Key Benefits

### **Clean Architecture**
- **asset-io**: Handles all format-specific I/O and embedding
- **c2pa-rs**: Handles signing logic and hash binding
- **Raw JUMBF**: Crosses the boundary in both directions (no format coupling)

### **Format-Agnostic**
Works with any supported format:
- JPEG (DataHash)
- PNG (DataHash)
- MP4/MOV (BmffHash)
- HEIC/HEIF/AVIF (BmffHash)

### **Explicit Control**
You control each step:
1. Create placeholder
2. Embed placeholder
3. Compute hash
4. Sign manifest
5. Patch in place

### **Efficient Updates**
- Placeholder-based workflow enables in-place patching
- Only JUMBF bytes are updated (not entire file)
- For 6.7GB video: 17KB update vs 6.7GB rewrite

## Workflow Steps

### Step 1: Detect Format
```rust
let mut asset = Asset::open("photo.mp4")?;
let native_format = asset.media_type().to_mime();  // "video/mp4", etc.
```

### Step 2: Check Placeholder Requirement
```rust
let needs_placeholder = builder.needs_placeholder(native_format);
// true for BMFF, true for DataHash, false for BoxHash
```

### Step 3: Create Raw JUMBF Placeholder
```rust
let placeholder_jumbf = builder.placeholder("application/c2pa")?;
// Returns raw JUMBF bytes (not format-wrapped)
```

### Step 4: Embed with asset-io
```rust
let updates = Updates::new().set_jumbf(placeholder_jumbf.clone());
let structure = asset.write(&mut output_file, &updates)?;
// asset-io wraps JUMBF appropriately (JPEG APP11, PNG chunk, BMFF uuid, etc.)
```

### Step 5: Set Exclusions (DataHash only)
```rust
// Use asset-io's helper which handles format-specific details (PNG CRC, etc.)
let (offset, size) = exclusion_range_for_segment(&structure, SegmentKind::Jumbf)?;
builder.set_data_hash_exclusions(vec![HashRange::new(offset, size)])?;
// BMFF auto-excludes C2PA uuid boxes
```

### Step 6: Compute Hash
```rust
builder.update_hash_from_stream(native_format, &mut output_file)?;
// SDK chooses: BmffHash, DataHash, or BoxHash
```

### Step 7: Sign and Get Padded JUMBF
```rust
let signed_jumbf = builder.sign_embeddable("application/c2pa")?;
// Returns raw JUMBF, zero-padded to match placeholder size
```

### Step 8: Patch In Place
```rust
structure.update_segment(&mut output_file, SegmentKind::Jumbf, signed_jumbf)?;
// asset-io updates just the JUMBF data
```

## Usage

```bash
# Sign a JPEG
cargo run --example c2pa_embeddable --features jpeg,xmp -- input.jpg output.jpg

# Sign a video (requires bmff feature)
cargo run --example c2pa_embeddable --features bmff,xmp -- video.mp4 signed.mp4

# Sign any format
cargo run --example c2pa_embeddable --features all-formats,xmp -- file.heic signed.heic
```

## Comparison with Classic API

### Classic Approach (`c2pa.rs`)
```rust
// SDK controls everything
let signed_bytes = builder.sign(signer, format, &mut source, &mut dest)?;
```

**Pros:**
- Simple, one call
- Works for basic cases

**Cons:**
- SDK controls I/O pipeline
- May re-read large files
- No in-place patching
- Format-specific code in SDK

### Embeddable Approach (`c2pa_embeddable.rs`)
```rust
// You control each step
let placeholder = builder.placeholder("application/c2pa")?;
asset.write(&mut output, &Updates::new().set_jumbf(placeholder))?;
builder.update_hash_from_stream(format, &mut output)?;
let signed = builder.sign_embeddable("application/c2pa")?;
structure.update_segment(&mut output, SegmentKind::Jumbf, signed)?;
```

**Pros:**
- Full control over I/O
- Clean separation of concerns
- In-place updates
- Format-agnostic boundary
- Single-pass operations

**Cons:**
- More steps
- Slightly more complex

## Performance

For large files (> 1GB), the embeddable API with in-place updates provides significant benefits:

| File Size | Full Rewrite | In-Place Update | Savings |
|-----------|--------------|-----------------|---------|
| 6.7GB MOV | 21.5s        | 9.1s            | 2.35x   |
| 100MB JPG | 1.2s         | 0.3s            | 4x      |

The savings increase with file size since only the ~17KB JUMBF is updated, not the entire file.

## Code Example Output

```
🚀 C2PA Embeddable API Example
   Input:  photo.jpg
   Output: signed.jpg

📂 Opening asset...
   Format: image/jpeg
   Container: Jpeg

📝 Creating manifest...
   ✅ Manifest configured

🔍 Workflow detection:
   Needs placeholder: true
   Mode: Placeholder-based (BMFF or DataHash)

📦 Creating placeholder...
   Size: 17273 bytes (raw JUMBF)

✍️  Writing asset with placeholder...
   ✅ File written with embedded placeholder

🎯 Setting exclusion ranges for DataHash...
   Range: offset=34481, size=17273

🔢 Computing hash...
   ✅ Hash computed and added to manifest

🔏 Signing manifest...
   Size: 17273 bytes (padded to match placeholder)

✏️  Patching signed manifest in place...
   ✅ Manifest updated in place

💾 Saved: signed.jpg

🔍 Verifying signature...
   ✅ Signature valid!

✨ Success!
```

## Key Takeaways

1. **asset-io handles all format-specific I/O** - No format knowledge in application code
2. **Raw JUMBF crosses the boundary** - Clean interface, no format coupling
3. **SDK handles hash binding** - Automatic BmffHash/DataHash/BoxHash selection
4. **In-place updates** - Efficient patching for placeholder-based workflows
5. **Format-agnostic** - Same code works for JPEG, PNG, MP4, HEIC, etc.

## Related Examples

- `c2pa.rs` - Classic API with full SDK control
- `c2pa_streaming.rs` - Streaming hash computation
- `parallel_hash.rs` - Parallel hashing for large files

## Documentation

- [C2PA Embeddable API Docs](../c2pa-rs/docs/embeddable-api.md)
- [asset-io README](README.md)
