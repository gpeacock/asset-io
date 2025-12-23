# jumbf-io

High-performance streaming JUMBF and XMP I/O for media files.

## Features

- **Single-pass parsing** - Discover file structure in one read
- **Lazy loading** - Only load data when accessed
- **Streaming writes** - Never load entire file into memory
- **Zero-copy where possible** - Memory-mapped file support
- **Format agnostic** - Easy to add new formats
- **Hash-friendly** - Calculate hashes without loading data

## Supported Formats

- ✅ JPEG (initial implementation)
- 🚧 PNG (planned)
- 🚧 BMFF/MP4 (planned)
- 🚧 WebP (planned)

## Design Goals

1. **Performance** - Minimize memory usage and maximize speed
2. **Streaming** - Support files of any size
3. **Flexibility** - Easy to extend with new formats
4. **Safety** - Memory-safe Rust with minimal unsafe code

## Usage

```rust
use jumbf_io::{FormatHandler, JpegHandler, Updates};
use std::fs::File;

// Parse file structure
let mut file = File::open("image.jpg")?;
let handler = JpegHandler::new();
let mut structure = handler.parse(&mut file)?;

// Access data lazily
if let Some(xmp) = structure.xmp(&mut file)? {
    println!("Found XMP: {} bytes", xmp.len());
}

// Write with updates in single streaming pass
let updates = Updates {
    new_xmp: Some(updated_xmp),
    new_jumbf: Some(updated_jumbf),
    ..Default::default()
};

let mut output = File::create("output.jpg")?;
handler.write(&structure, &mut file, &mut output, &updates)?;
```

## Architecture

### Core Abstractions

- `FormatHandler` - Trait for format-specific implementations
- `FileStructure` - Represents discovered file structure
- `Segment` - Individual parts of a file (XMP, JUMBF, image data, etc.)
- `LazyData` - Data that's only loaded when accessed

### Single-Pass Design

The parser makes a single pass through the file, recording offsets and sizes
without loading data. Data is only loaded when explicitly accessed or when
writing updates.

### Memory Efficiency

- Segments track locations, not data
- Large data (like image data) is never loaded unless needed
- Streaming copies avoid buffering
- Optional memory-mapped file support for zero-copy reads

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

