# C2PA Integration Example (TEMPORARY)

**⚠️ THIS IS A TEMPORARY EXAMPLE FOR TESTING ONLY**

This example will be removed after validation. It is intentionally isolated to make removal easy.

## Purpose

Test how `asset-io` integrates with the C2PA crate's embeddable APIs:
- Reading file segments for C2PA JUMBF data
- Extracting metadata (XMP/EXIF) for C2PA claims
- Understanding how asset-io can help with C2PA workflows

## Running

Basic test (no C2PA dependencies):
```bash
cargo run --example c2pa_integration --features xmp
```

With C2PA testing enabled (uncomment c2pa in Cargo.toml first):
```bash
cargo run --example c2pa_integration --features xmp,c2pa-test
```

## Requirements

You need a test JPEG file at one of these locations:
- `tests/assets/test.jpg`
- `tests/assets/sample.jpg`
- `sample.jpg`

## Removal Instructions

When testing is complete, remove this example by:

1. **Delete this folder**: `rm -rf examples/c2pa_integration/`
2. **Delete the example file**: `rm examples/c2pa_integration.rs`
3. **Edit `Cargo.toml`**:
   - Remove the `[[example]]` section for `c2pa_integration`
   - Optionally remove `c2pa` from `[dev-dependencies]`
   - Remove the `c2pa-test` feature

That's it - the library will be back to its clean state with no C2PA dependencies.

## What This Tests

- ✅ Reading JPEG/PNG structure with asset-io
- ✅ Extracting XMP metadata for C2PA claims
- ✅ Extracting EXIF data
- ✅ Locating JUMBF segments (C2PA data)
- ✅ Understanding segment layout for JUMBF insertion
- 🚧 C2PA Builder integration (TODO)
- 🚧 C2PA Reader integration (TODO)
- 🚧 XMP/EXIF updates after C2PA signing (TODO)

