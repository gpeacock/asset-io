# TEMPORARY C2PA Example - Quick Reference

## What Was Created

This isolated C2PA integration example allows testing `asset-io` with C2PA workflows without affecting the main library.

### Files Added
```
examples/
├── c2pa_integration.rs    # Main example code
└── C2PA_README.md        # Documentation
```

### Cargo.toml Changes
```toml
# In [dev-dependencies]:
# c2pa = { version = "0.34", optional = true }  # (commented out by default)

# In [features]:
c2pa-test = []  # Optional feature to enable C2PA testing

# New [[example]] section:
[[example]]
name = "c2pa_integration"
required-features = ["xmp"]
```

## Running the Example

```bash
# Basic test (no C2PA dependencies):
cargo run --example c2pa_integration --features xmp

# With C2PA enabled (uncomment c2pa in Cargo.toml first):
cargo run --example c2pa_integration --features xmp,c2pa-test
```

## What It Tests

✅ **Currently Working:**
- Reading file structure (JPEG/PNG segments)
- Detecting JUMBF/C2PA segments
- Extracting XMP metadata
- Extracting EXIF data
- Segment location mapping

🚧 **TODO (when c2pa dependency is enabled):**
- C2PA Builder integration (creating manifests)
- C2PA Reader integration (reading/validating manifests)
- Converting XMP → JSON-LD for C2PA claims
- Updating metadata after C2PA signing

## Complete Removal Instructions

When testing is done:

```bash
# 1. Delete files
rm examples/c2pa_integration.rs
rm examples/C2PA_README.md
rm examples/C2PA_REMOVAL.md

# 2. Edit Cargo.toml - remove:
#    - [[example]] section for c2pa_integration
#    - c2pa from [dev-dependencies] (if uncommented)
#    - c2pa-test from [features]

# 3. Done! Library is clean again.
```

## Why This Design

- **Isolated**: C2PA deps are optional dev-dependencies (don't affect library users)
- **Removable**: Everything is clearly marked TEMPORARY
- **Testable**: Works without C2PA to test asset-io integration
- **Extensible**: Easy to add real C2PA code when ready

## Integration Points

`asset-io` provides these capabilities for C2PA workflows:

1. **Segment Navigation**: Locate where JUMBF data lives
2. **Metadata Extraction**: Get XMP/EXIF for claims/assertions
3. **Structure Analysis**: Understand file layout for insertion
4. **Low-level Access**: Read specific byte ranges efficiently
5. **Format Agnostic**: Works with JPEG, PNG, and future formats

Perfect for building C2PA tools that need fine-grained file access!

