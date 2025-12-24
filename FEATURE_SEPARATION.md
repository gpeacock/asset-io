# Feature Separation: `exif` Feature

## Summary

We've separated EXIF parsing and embedded thumbnail extraction into a dedicated `exif` feature, while making the thumbnail generation infrastructure always available (no longer behind a feature flag).

- **`exif` feature**: EXIF metadata parsing and **embedded thumbnail extraction**
- **Thumbnail infrastructure**: Always available - `ThumbnailGenerator` trait, `ThumbnailOptions`, etc.

## Rationale

### Before
Previously, the `thumbnails` feature controlled both:
1. Embedded thumbnail extraction (reading pre-rendered thumbnails from EXIF)
2. Thumbnail generation infrastructure

This was confusing because:
- The `thumbnails` feature didn't actually generate thumbnails (it was just infrastructure)
- Embedded thumbnail extraction is essentially EXIF parsing, not thumbnail generation
- Users who wanted EXIF thumbnails were forced to enable the `thumbnails` feature
- The infrastructure had zero dependencies and minimal overhead

### After
Now we have clear separation:

**`exif` feature:**
- Parses EXIF metadata in JPEG (APP1 marker) and PNG (eXIf chunk)
- Extracts embedded thumbnail location from EXIF IFD1
- Provides `Asset::embedded_thumbnail()` method
- Minimal overhead: just TIFF parsing logic (~5KB)

**Thumbnail infrastructure (always available):**
- Provides `ThumbnailGenerator` trait for external implementations
- Defines `ThumbnailOptions` types for configuration
- Infrastructure for thumbnail generation (not the actual generation)
- **Zero dependencies** - just trait and type definitions
- Intended for integration with crates like `image`, `turbojpeg`, etc.

## API Changes

### New: `Asset::embedded_thumbnail()`
```rust
#[cfg(feature = "exif")]
pub fn embedded_thumbnail(&mut self) -> Result<Option<EmbeddedThumbnail>>
```

Extracts pre-rendered thumbnail embedded in EXIF metadata. This is the **fastest** path for getting a thumbnail:
- No decoding needed
- Direct memory access
- Typically 160x120 JPEG for cameras

### Removed: `Structure::embedded_thumbnail()`
Moved to `Asset` to follow the pattern where `Asset` delegates format-specific operations to handlers.

### Removed: `FormatHandler::generate_thumbnail()`
This was a placeholder that never actually generated thumbnails. External crates should implement `ThumbnailGenerator` instead.

### Added: `FormatHandler::extract_embedded_thumbnail()`
```rust
#[cfg(feature = "exif")]
fn extract_embedded_thumbnail<R: Read + Seek>(
    &self,
    structure: &Structure,
    reader: &mut R,
) -> Result<Option<EmbeddedThumbnail>>;
```

Format-specific method for extracting embedded thumbnails.

## Segment Changes

The `Segment::Exif` variant's `thumbnail` field now uses the `exif` feature flag:

```rust
Exif {
    offset: u64,
    size: u64,
    #[cfg(feature = "exif")]
    thumbnail: Option<EmbeddedThumbnail>,
}
```

Without the `exif` feature, EXIF segments are still detected and tracked, but thumbnail extraction is disabled.

## Feature Flag Matrix

| Feature    | EXIF Detection | Thumbnail Extraction | Thumbnail Infrastructure |
|------------|----------------|----------------------|--------------------------|
| (none)     | ✓              | ✗                    | ✓                        |
| `exif`     | ✓              | ✓                    | ✓                        |

**Note:** Thumbnail generation infrastructure (`ThumbnailGenerator` trait, `ThumbnailOptions`, etc.) is always available with zero dependencies.

## Migration Guide

### If you were using `thumbnails` for EXIF thumbnails:
```rust
// Before (with --features thumbnails)
let thumb = structure.embedded_thumbnail()?;

// After (with --features exif)
let thumb = asset.embedded_thumbnail()?;
```

### If you were implementing thumbnail generation:
The infrastructure is now always available - no feature flag needed!

```rust
// Now: Always available
use asset_io::ThumbnailGenerator;

impl ThumbnailGenerator for MyGenerator {
    fn generate(&self, data: &[u8], format: Option<Format>) -> Result<Vec<u8>> {
        // Your implementation
    }
}
```

## Cargo.toml Features

```toml
[features]
exif = []  # EXIF parsing and embedded thumbnail extraction
```

Enable `exif` if you want to extract pre-rendered thumbnails from EXIF.
The thumbnail generation infrastructure is always available.

## Benefits

1. **Clearer intent**: `exif` is about reading metadata, thumbnail infrastructure is always available
2. **Lower overhead**: Users who only want EXIF data don't need thumbnail generation infrastructure (already zero overhead)
3. **Better modularity**: EXIF parsing is separate from the thumbnail generation API
4. **Simpler API**: No need to enable `thumbnails` feature just to use the infrastructure
5. **Future-proof**: Can add more EXIF features (GPS, camera settings, etc.) under the `exif` flag
