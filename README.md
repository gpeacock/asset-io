# asset-io

High-performance, format-agnostic streaming I/O for media asset metadata (JUMBF, XMP, C2PA, EXIF, thumbnails).

## Features

- **Fast** - Single-pass parsing, optimized seeks, streaming writes
- **Memory Efficient** - Lazy loading, processes files larger than RAM
- **Format Agnostic** - Auto-detects JPEG, PNG, HEIC, AVIF, MP4, and more
- **Type Safe** - Full Rust type safety and error handling
- **Minimal Dependencies** - Core functionality with optional features
- **C2PA Ready** - Built-in support for streaming hash computation with exclusions

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
asset-io = { version = "0.1", features = ["jpeg"] }
```

### Basic Usage

```rust
use asset_io::{Asset, Updates};

fn main() -> asset_io::Result<()> {
    // Open any supported file - format is auto-detected
    let mut asset = Asset::open("image.jpg")?;
    
    // Read metadata
    if let Some(xmp) = asset.xmp()? {
        println!("XMP: {} bytes", xmp.len());
    }
    
    if let Some(jumbf) = asset.jumbf()? {
        println!("JUMBF/C2PA: {} bytes", jumbf.len());
    }
    
    // Modify and write - supports ANY combination of add/remove/replace
    let updates = Updates::new()
        .set_xmp(b"<new>metadata</new>".to_vec())
        .remove_jumbf();
    asset.write_to("output.jpg", &updates)?;
    
    Ok(())
}
```

### XMP Parsing with MiniXmp

```rust
use asset_io::{Asset, MiniXmp};

fn main() -> asset_io::Result<()> {
    let mut asset = Asset::open("photo.jpg")?;
    
    if let Some(xmp_bytes) = asset.xmp()? {
        let xmp = MiniXmp::new(String::from_utf8_lossy(&xmp_bytes).into_owned());
        
        // Read values
        if let Some(title) = xmp.get("dc:title") {
            println!("Title: {}", title);
        }
        
        // Modify values (returns new MiniXmp)
        let updated = xmp.set("dc:creator", "John Doe")?;
        
        // Get multiple values efficiently
        let values = updated.get_many(&["dc:title", "dc:creator", "dc:description"]);
    }
    Ok(())
}
```

### Streaming Processing (C2PA Hashing)

Process data during read or write with callbacks - ideal for computing hashes:

```rust
use asset_io::{Asset, Updates, SegmentKind, ExclusionMode, ProcessingOptions};

fn main() -> asset_io::Result<()> {
    let mut asset = Asset::open("image.jpg")?;
    
    // Configure processing to exclude JUMBF from hash (C2PA requirement)
    let updates = Updates::new()
        .set_jumbf(placeholder_jumbf)
        .exclude_from_processing(SegmentKind::Jumbf)
        .with_exclusion_mode(ExclusionMode::DataOnly);  // C2PA compliant
    
    // Write while computing hash in single pass
    let mut hasher = Sha256::new();
    asset.write_with_processing(
        output_file,
        &updates,
        |bytes| hasher.update(bytes),
    )?;
    
    let hash = hasher.finalize();
    Ok(())
}
```

### Embedded Thumbnails

```rust
use asset_io::Asset;

fn main() -> asset_io::Result<()> {
    let mut asset = Asset::open("photo.jpg")?;
    
    // Extract embedded thumbnail (from EXIF)
    if let Some(thumbnail) = asset.read_embedded_thumbnail()? {
        println!("Thumbnail: {} bytes, format: {:?}", 
                 thumbnail.data.len(), 
                 thumbnail.format);
        std::fs::write("thumb.jpg", &thumbnail.data)?;
    }
    Ok(())
}
```

## Feature Flags

```toml
[dependencies]
asset-io = { version = "0.1", features = ["jpeg", "png", "xmp"] }
```

| Feature | Description |
|---------|-------------|
| `jpeg` | JPEG format support (default) |
| `png` | PNG format support |
| `bmff` | HEIC/HEIF/AVIF/MP4/MOV support |
| `pdf` | Read-only PDF XMP/C2PA extraction, with optional pure-Rust Flate support |
| `xmp` | XMP parsing with MiniXmp |
| `exif` | EXIF/thumbnail extraction |
| `all-formats` | All format handlers |
| `test-utils` | Test fixtures and utilities |

### PDF (read-only)

Enable `pdf` (also included in `all-formats`) and use `Asset::open`,
`asset.xmp()`, and `asset.jumbf()` as above. PDF 1.0-1.7 and 2.0 headers are
recognized at byte zero. The reader follows classic tables, cross-reference
streams, and incremental `/Prev` chains, resolving the current catalog's
`/Metadata` and document-level `/AF` C2PA association. Cross-reference streams
remain compact tables; compressed object streams are decoded on demand, with
only one object stream cached during parsing. It does not load the document,
decode page content, or search for metadata markers. Unreferenced and superseded
metadata are not exposed.

Metadata streams, including indirect `/Length` values, are exposed as physical
byte ranges. Parsing skips their payloads; `xmp()` and `jumbf()` allocate only
when explicitly called, and decode Flate-compressed metadata at that point.
The `pdf` feature enables `flate2` with its pure-Rust backend (no native zlib).
FlateDecode and 8-bit single-component PNG predictors are supported.

Existing range/chunk APIs and optional memory mapping provide streaming or
borrowed access to **physical file bytes**. For compressed metadata these bytes
are encoded, not decoded XMP/JUMBF; the segment's `PdfFlate` metadata records the
encoding. Use `xmp()`/`jumbf()` to retrieve decoded payloads. Memory mapping
retains its existing unsafe API and file-stability requirements.

This is **not a general PDF parser, renderer, sanitizer, or C2PA validator**:

- Encryption, hybrid-reference files, external metadata streams, other filter
  types/chains, and other predictor layouts remain unsupported.
  Recognized unsupported features return `Error::UnsupportedFeature`, not an
  empty metadata result. Page/content streams are neither decoded nor executed.
- Only document-level C2PA stores associated through the catalog's `/AF` are
  discovered. Page/object-level manifests and attachment-only stores without
  that association are outside this first version. C2PA file specifications
  require `/AFRelationship /C2PA_Manifest` and `/Subtype /application#2Fc2pa`.
  Multiple document-level C2PA stores are rejected rather than concatenated.
- Parsing is bounded to 1,000,000 total cross-reference entries (at most 100,000
  classic entries), 128 revisions, 4,096 object resolutions,
  nesting/reference depth 32, 100,000 syntax nodes, 4,096-byte
  tokens/names, and 16 MiB of syntax work.
  - Structural streams have separate 8 MiB encoded and decoded limits.
  - Structural decoding has a cumulative 32 MiB input-plus-output work budget;
    repeatedly decoding different object streams also consumes this budget.
  - Retained compact cross-reference data is limited to 16 MiB.
  - Metadata input is limited to 256 MiB; Flate output (including predictor row
    bytes) is limited to 64 MiB per retrieval. Uncompressed retrieval retains
    the existing 256 MiB limit. Returning a `Vec` necessarily allocates the
    requested payload, but never the whole PDF.
  - Predictor rows are limited to 64 KiB; object streams to 100,000 entries.

  The final `startxref` and `%%EOF` must occur within the last 1,024 bytes.
  Limit violations and malformed/ambiguous structures return errors.
- Checksum errors, incomplete streams, and trailing compressed data are rejected.
  A narrow compatibility exception permits legacy **cross-reference** streams
  ending in a zlib sync-flush marker without the final zlib trailer, only when
  their decoded size exactly matches the declared table layout. This emits a
  `log::warn!` diagnostic. Object and metadata streams require full termination.
- All write operations, including in-place updates and writes with no metadata
  changes, fail explicitly. `write_to` rejects PDF before creating or truncating
  the output file.
- Generic hashing/exclusion APIs remain byte-range utilities, **not PDF C2PA
  validation**. PDF-specific revision/signature hashing rules are not implemented.

Run the synthetic payload-size benchmark with
`cargo bench --no-default-features --features pdf --bench pdf`.
It measures parsing separately from metadata materialization.

## Performance

Designed for high-throughput applications:

- **Parse**: ~10ms for a 22MB JPEG with C2PA data
- **Write**: Single sequential pass with optimized seeks
- **Memory**: Streams data directly, O(1) memory usage

### Streaming Architecture

```
Read:   [===Sequential Read===]  → callback(bytes)
                                     ↓
Write:  [===Sequential Write===] → callback(bytes) → hash/process
                                     ↓
Update: [Seek→][Patch]           → in-place JUMBF update
```

## Supported Formats

| Format | Parse | Write | XMP | JUMBF | EXIF | Thumbnail |
|--------|-------|-------|-----|-------|------|-----------|
| JPEG | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| PNG | ✅ | ✅ | ✅ | ✅ | - | - |
| HEIC/HEIF | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| AVIF | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| MP4/MOV | ✅ | ✅ | ✅ | ✅ | - | - |

## Examples

```bash
# Inspect file structure and metadata
cargo run --example inspect --features all-formats,xmp,exif -- image.jpg

# Test all metadata operation combinations
cargo run --example test_all_combinations --features jpeg,png,test-utils

# C2PA signing with new embeddable API (0.77.0+)
cargo run --example c2pa_embeddable --features all-formats,xmp -- input.jpg output.jpg

# Update XMP field in-place
cargo run --example update_xmp_field --features jpeg,xmp -- photo.jpg dc:title "New Title"

# Update JUMBF in-place
cargo run --example update_jumbf --features jpeg -- photo.jpg

# Hash performance benchmark
cargo run --release --example hash_benchmark --features jpeg
```

## API Overview

### Core Types

| Type | Description |
|------|-------------|
| `Asset` | Main entry point - open, read, write assets |
| `Updates` | Builder for metadata modifications |
| `Structure` | Parsed file structure with segment info |
| `MiniXmp` | Lightweight XMP parser/modifier (requires `xmp` feature) |
| `ExifInfo` | Parsed EXIF metadata (requires `exif` feature) |
| `Thumbnail` | Embedded thumbnail data |

### Key Methods

```rust
// Opening assets
Asset::open(path)?                    // From file path
Asset::from_source(reader)?           // From any Read+Seek

// Reading metadata
asset.xmp()?                          // Option<Vec<u8>>
asset.jumbf()?                        // Option<Vec<u8>>
asset.exif_info()?                    // Option<ExifInfo>
asset.read_embedded_thumbnail()?      // Option<Thumbnail>

// Writing
asset.write_to(path, &updates)?       // To new file
asset.write(&mut writer, &updates)?   // To any Read+Write+Seek (e.g. File or Cursor<Vec<u8>>)

// Streaming processing
asset.read_with_processing(callback, &options)?
asset.write_with_processing(&mut writer, &updates, callback)?   // writer: Read+Write+Seek

// In-place updates (when size permits)
asset.update_segment_in_place(asset_io::SegmentKind::Xmp, new_xmp)?
asset.update_segment_in_place(asset_io::SegmentKind::Jumbf, new_jumbf)?
```

## Use Cases

- **C2PA/Content Credentials** - Stream-hash-sign workflow in single pass
- **Photo Management** - Extract and modify EXIF/XMP metadata
- **Media Processing Pipelines** - High-throughput metadata handling
- **Forensics** - Inspect file structure and embedded data
- **Thumbnail Extraction** - Extract embedded previews

## Roadmap

- [x] JPEG format support
- [x] PNG format support  
- [x] BMFF format support (HEIC/HEIF/AVIF/MP4)
- [x] Format auto-detection
- [x] Streaming writes with seek optimization
- [x] Metadata add/remove/replace (all combinations)
- [x] MiniXmp parser
- [x] EXIF parsing (JPEG, HEIC/HEIF, AVIF)
- [x] Thumbnail extraction (JPEG, HEIC/HEIF, AVIF)
- [x] Streaming processing callbacks
- [x] BMFF thumbnail extraction
- [ ] Memory-mapped I/O option
- [ ] Async I/O support

## License

MIT OR Apache-2.0

## Contributing

Contributions welcome! Please open an issue or PR.
