# asset-io

High-performance, format-agnostic streaming I/O for media asset metadata (JUMBF, XMP, C2PA, thumbnails).

## Features

- 🚀 **Blazing Fast** - Single-pass parsing, optimized seeks, streaming writes
- 💾 **Memory Efficient** - Lazy loading, processes files larger than RAM
- 🔍 **Format Agnostic** - Auto-detects JPEG, PNG, MP4, and more
- 🛡️ **Type Safe** - Full Rust type safety and error handling
- 📦 **Zero Dependencies** - Minimal dependency footprint (435 KB)
- 🖼️ **Thumbnail Interface** - Pluggable thumbnail generation without decoder bloat

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
asset-io = "0.1"
```

### Format-Agnostic API (Recommended)

The simplest way to use this library - automatically detects file formats:

```rust
use asset_io::{Asset, Updates, XmpUpdate, JumbfUpdate};

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
    let updates = Updates {
        xmp: XmpUpdate::Set(b"<new>metadata</new>".to_vec()),
        jumbf: JumbfUpdate::Remove,
        ..Default::default()
    };
    asset.write_to("output.jpg", &updates)?;
    
    Ok(())
}
```

### Format-Specific API

For more control over the parsing and writing process:

```rust
use asset_io::{JpegHandler, FormatHandler, Updates};
use std::fs::File;

fn main() -> asset_io::Result<()> {
    let mut file = File::open("image.jpg")?;
    let handler = JpegHandler::new();
    
    // Single-pass parse
    let mut structure = handler.parse(&mut file)?;
    
    // Lazy load metadata
    if let Some(xmp) = structure.xmp(&mut file)? {
        println!("XMP: {} bytes", xmp.len());
    }
    
    // Write with updates
    let updates = Updates::default();
    let mut output = File::create("output.jpg")?;
    handler.write(&structure, &mut file, &mut output, &updates)?;
    
    Ok(())
}
```

## Performance

Designed for high-throughput applications:

- **Parse**: ~10ms for a 22MB JPEG with C2PA data
- **Write**: Single sequential pass with optimized seeks
- **Memory**: Streams data directly, O(1) memory usage

### Example Performance (22MB JPEG with 651KB JUMBF)

| Operation | Time | Seeks | Memory |
|-----------|------|-------|---------|
| Parse | 10ms | 0 | ~1KB |
| Read XMP | <1μs | 0 | 2KB |
| Read JUMBF | <200μs | 0 | 651KB |
| Write (copy) | ~20ms | 1-2 | ~8KB |

## Supported Formats

| Format | Parse | Write | XMP | JUMBF |
|--------|-------|-------|-----|-------|
| JPEG | ✅ | ✅ | ✅ | ✅ |
| PNG | ✅ | ✅ | ✅ | ✅ |
| MP4/MOV | 🚧 | 🚧 | 🚧 | 🚧 |

## Architecture

### Design Principles

1. **Streaming First** - Never load entire files into memory
2. **Lazy Loading** - Only read data when accessed
3. **Zero Seeks** - Optimize for sequential I/O when possible
4. **Format Agnostic** - Unified API across all formats

### I/O Pattern

```
Parse:  [===Sequential Read===]           (10ms)
         ↓
Write:  [Seek→][===Sequential Write===]   (20ms)
         ↓
Output: Valid file with updated metadata
```

## Examples

Run the included examples:

```bash
# Inspect file structure
cargo run --example inspect image.jpg

# Test all metadata operation combinations
cargo run --example test_all_combinations

# Format-agnostic API demo
cargo run --example asset_demo image.jpg

# API quick reference (see all supported operations)
cargo run --example api_quick_reference

# Thumbnail generation interface demo
cargo run --example thumbnail_demo --features memory-mapped

# Hardware-accelerated SHA-256 hashing
cargo run --example sha256_demo --features memory-mapped
```

See `OPERATIONS.md` for complete API documentation.

## Documentation

- [docs/OPERATIONS.md](./docs/OPERATIONS.md) - Complete API reference for all operations
- [docs/TESTING.md](./docs/TESTING.md) - Comprehensive testing guide
- [docs/HARDWARE_HASHING.md](./docs/HARDWARE_HASHING.md) - Hardware-accelerated hashing details
- [docs/THUMBNAILS.md](./docs/THUMBNAILS.md) - Thumbnail generation interface guide
- [docs/XMP_EXTENDED.md](./docs/XMP_EXTENDED.md) - XMP Extended implementation details

## Use Cases

- **C2PA/Content Credentials** - Read and write provenance data
- **Photo Management** - Extract and modify EXIF/XMP metadata
- **Media Processing Pipelines** - High-throughput metadata handling
- **Forensics** - Inspect file structure and embedded data

## Roadmap

- [x] JPEG format support
- [x] Format auto-detection
- [x] Streaming writes with seek optimization
- [x] Metadata add/remove/replace (all combinations)
- [ ] PNG format support
- [ ] MP4/MOV format support
- [ ] Memory-mapped I/O option
- [ ] Async I/O support

## License

MIT OR Apache-2.0

## Contributing

Contributions welcome! Please open an issue or PR.

