# asset-io

High-performance, format-agnostic streaming I/O for media asset metadata (JUMBF, XMP, C2PA, EXIF, thumbnails).

## Features

- 🚀 **Blazing Fast** - Single-pass parsing, optimized seeks, streaming writes
- 💾 **Memory Efficient** - Lazy loading, processes files larger than RAM
- 🔍 **Format Agnostic** - Auto-detects JPEG, PNG, HEIC, AVIF, MP4, and more
- 🛡️ **Type Safe** - Full Rust type safety and error handling
- 📦 **Minimal Dependencies** - Core functionality with optional features
- 🔐 **C2PA Ready** - Built-in support for streaming hash computation with exclusions

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
| `xmp` | XMP parsing with MiniXmp |
| `exif` | EXIF/thumbnail extraction |
| `all-formats` | All format handlers |
| `test-utils` | Test fixtures and utilities |

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

| Format | Parse | Write | XMP | JUMBF | EXIF |
|--------|-------|-------|-----|-------|------|
| JPEG | ✅ | ✅ | ✅ | ✅ | ✅ |
| PNG | ✅ | ✅ | ✅ | ✅ | - |
| HEIC/HEIF | ✅ | ✅ | ✅ | ✅ | - |
| AVIF | ✅ | ✅ | ✅ | ✅ | - |
| MP4/MOV | ✅ | ✅ | ✅ | ✅ | - |

## Examples

```bash
# Inspect file structure and metadata
cargo run --example inspect --features all-formats,xmp,exif -- image.jpg

# Test all metadata operation combinations
cargo run --example test_all_combinations --features jpeg,png,test-utils

# C2PA signing workflow demo
cargo run --example c2pa --features jpeg,png

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
| `MiniXmp` | Lightweight XMP parser/modifier |
| `ProcessingWriter` | Write wrapper with byte callbacks |

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
asset.write(&mut writer, &updates)?   // To any Write+Seek

// Streaming processing
asset.read_with_processing(callback, &options)?
asset.write_with_processing(writer, &updates, callback)?

// In-place updates (when size permits)
asset.update_xmp_in_place(new_xmp)?
asset.update_jumbf_in_place(new_jumbf)?
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
- [x] EXIF parsing and thumbnail extraction
- [x] Streaming processing callbacks
- [x] BMFF thumbnail extraction
- [ ] Memory-mapped I/O option
- [ ] Async I/O support

## License

MIT OR Apache-2.0

## Contributing

Contributions welcome! Please open an issue or PR.
