# BMFF Support Implementation Summary

## Overview
Successfully added BMFF (ISO Base Media File Format) support to the asset-io library using `/Users/gpeacock/dev/c2pa-rs/sdk/src/asset_handlers/bmff_io.rs` as a reference.

## What Was Implemented

### 1. Media Types Added (src/media_type.rs)
Added 6 new BMFF-based media types:
- `MediaType::Heic` - HEIC image (HEVC/H.265 codec)
- `MediaType::Heif` - HEIF image  
- `MediaType::Avif` - AVIF image (AV1 codec)
- `MediaType::Mp4Video` - MP4 video
- `MediaType::Mp4Audio` - M4A audio
- `MediaType::QuickTime` - QuickTime MOV video

All map to `Container::Bmff` with appropriate MIME types and file extensions.

### 2. Dependencies Added (Cargo.toml)
- Added `atree = "0.5"` dependency for tree-based box hierarchy
- Created `bmff` feature flag: `bmff = ["byteorder", "atree"]`
- Added to `all-formats = ["jpeg", "png", "bmff"]`

### 3. BMFF I/O Implementation (src/formats/bmff_io.rs)
Created a new ~800 line module implementing `ContainerIO` trait:

**Key Features:**
- **Box Type System**: Macro-based enum for BMFF box types (ftyp, moov, uuid, etc.)
- **Tree-Based Parsing**: Uses `atree` crate to build hierarchical box structure
- **Media Type Detection**: Reads ftyp box major brand to determine specific media type
- **XMP Support**: Extracts XMP from UUID boxes with XMP_UUID
- **C2PA/JUMBF Support**: Extracts C2PA data from UUID boxes with C2PA_UUID
- **Box Header Parsing**: Handles standard and large box sizes

**Architecture:**
```rust
pub struct BmffIO;  // Zero-sized type (stateless)

impl ContainerIO for BmffIO {
    fn parse() -> Structure         // Single-pass structure discovery
    fn extract_xmp() -> Vec<u8>     // Extract XMP from UUID boxes
    fn extract_jumbf() -> Vec<u8>   // Extract C2PA/JUMBF from UUID boxes
    fn write()                       // TODO: Full implementation
    fn calculate_updated_structure() // TODO: Full implementation
}
```

**Internal Helpers:**
- `BoxHeaderLite` - Efficient box header reading/writing
- `BoxInfo` - Box metadata stored in tree nodes
- `build_bmff_tree()` - Recursive tree builder
- `detect_media_type_from_ftyp()` - Maps ftyp brand to MediaType

### 4. Registration (src/formats/mod.rs, src/lib.rs)
- Registered `BmffIO` in the `register_containers!` macro
- Added module: `#[cfg(feature = "bmff")] pub(crate) mod bmff_io;`
- Re-exported: `#[cfg(feature = "bmff")] pub use formats::bmff_io::BmffIO;`

### 5. Test Example (examples/test_bmff.rs)
Created a test example that:
- Opens HEIC, AVIF, and MP4 files
- Displays container, media type, size, and segment count
- Checks for XMP and JUMBF data
- Tests multiple fixture files

## Test Results

Successfully tested with multiple BMFF files:

```
📁 sample1.heic
  Container: Bmff
  Media Type: Heif
  Size: 293608 bytes
  ✗ No XMP/JUMBF

📁 sample1.avif  
  Container: Bmff
  Media Type: Avif
  Size: 97436 bytes
  ✗ No XMP/JUMBF

📁 video1.mp4
  Container: Bmff
  Media Type: Mp4Video
  Size: 828571 bytes
  ✓ XMP found: 4534 bytes
  ✓ JUMBF found: 30617 bytes
```

## What Works
✅ Container detection (from ftyp box magic bytes)
✅ Media type detection (from ftyp major brand)
✅ Box hierarchy parsing with tree structure
✅ XMP extraction from UUID boxes
✅ JUMBF/C2PA extraction from UUID boxes
✅ Integration with Asset API (auto-detection)
✅ Support for HEIC, HEIF, AVIF, MP4, M4A, MOV

## What's Not Yet Implemented
⚠️ **Write Support**: The `write()` method currently just copies the file
- Need to implement C2PA UUID box insertion/replacement
- Need to implement XMP UUID box insertion/replacement
- Need to adjust file offsets after modifications (stco, co64, iloc, tfhd, etc.)

⚠️ **Structure Calculation**: `calculate_updated_structure()` needs full implementation
- Required for VirtualAsset workflow
- Required for pre-calculating offsets for C2PA hashing

⚠️ **Thumbnail Extraction**: `extract_embedded_thumbnail()` not implemented
- BMFF can have thumbnails in 'thmb' item references
- Requires parsing meta/iinf/iloc boxes

## Usage

### Basic Usage
```rust
use asset_io::{Asset, Updates};

// Auto-detection works for BMFF files
let mut asset = Asset::open("video.mp4")?;
let xmp = asset.xmp()?;
let jumbf = asset.jumbf()?;

// Write (currently just copies)
asset.write_to("output.mp4", &Updates::default())?;
```

### Direct Handler Usage
```rust
use asset_io::{BmffIO, ContainerIO};
use std::fs::File;

let mut file = File::open("video.mp4")?;
let handler = BmffIO::new();
let structure = handler.parse(&mut file)?;
let xmp = handler.extract_xmp(&structure, &mut file)?;
```

## Build & Test

```bash
# Build with BMFF support
cargo build --features bmff

# Run test example
cargo run --example test_bmff --features bmff

# Build with all formats
cargo build --features all-formats
```

## Next Steps

To complete BMFF support, the reference implementation has these functions that should be adapted:

1. **Writing C2PA boxes**: `write_c2pa_box()` - Writes UUID box with C2PA data
2. **Offset adjustment**: `adjust_known_offsets()` - Patches stco, co64, iloc, tfhd, tfra, saio boxes
3. **Box rewriting**: Full implementation of `write()` method with offset tracking
4. **Structure calculation**: Implement `calculate_updated_structure()` for VirtualAsset
5. **Thumbnail support**: Parse meta/iloc boxes for embedded thumbnails

The reference implementation at `/Users/gpeacock/dev/c2pa-rs/sdk/src/asset_handlers/bmff_io.rs` has all of these implemented and can be adapted as needed.

## Files Modified

1. `Cargo.toml` - Added atree dependency and bmff feature
2. `src/media_type.rs` - Added 6 BMFF media types
3. `src/formats/mod.rs` - Registered bmff_io module
4. `src/formats/bmff_io.rs` - New 800-line implementation
5. `src/lib.rs` - Re-exported BmffIO
6. `src/segment.rs` - Added Clone derives for Segment and LazyData
7. `examples/test_bmff.rs` - New test example

