# Refactoring: Format-Specific Metadata Extraction

## Problem

The original implementation had format-specific metadata extraction logic in `FileStructure`:

```rust
// BAD: Format-specific logic in generic structure code
pub fn jumbf<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
    // Check if JPEG format (has JPEG XT headers)
    #[cfg(feature = "jpeg")]
    let is_jpeg = matches!(self.format, crate::Format::Jpeg);
    
    // JPEG-specific: skip JPEG XT header (8 bytes)
    // PNG-specific: no header to skip
    // Future formats: more conditions...
}

pub fn xmp<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
    // Extended XMP assembly (JPEG-specific)
    // Sort parts by chunk_offset, validate GUIDs...
    // PNG doesn't have extended XMP!
}
```

This approach doesn't scale - every new format would add more conditional logic.

## Solution

Moved format-specific extraction logic into each format handler:

### 1. Added extraction methods to `FormatHandler` trait

```rust
pub trait FormatHandler: Send + Sync {
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<FileStructure>;
    fn write<R: Read + Seek, W: Write>(...) -> Result<()>;
    
    // NEW: Format-specific metadata extraction
    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>>;
    
    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>>;
}
```

### 2. Implemented in JPEG handler (`src/formats/jpeg.rs`)

```rust
impl JpegHandler {
    /// Extract XMP from JPEG (handles extended XMP with multi-segment assembly)
    pub fn extract_xmp_impl<R: Read + Seek>(...) -> Result<Option<Vec<u8>>> {
        // JPEG-specific logic:
        // - Read main XMP from APP1 marker
        // - If extended_parts present:
        //   - Sort by chunk_offset
        //   - Validate GUIDs match
        //   - Assemble into complete XMP
        ...
    }
    
    /// Extract JUMBF from JPEG (handles JPEG XT headers & multi-segment assembly)
    pub fn extract_jumbf_impl<R: Read + Seek>(...) -> Result<Option<Vec<u8>>> {
        // JPEG-specific logic:
        // - Strip 8-byte JPEG XT headers (CI + En + Z)
        // - Handle multi-segment assembly
        // - Skip repeated LBox/TBox in continuation segments
        ...
    }
}
```

### 3. Implemented in PNG handler (`src/formats/png.rs`)

```rust
impl PngHandler {
    /// Extract XMP from PNG (simple iTXt chunk, no extended XMP)
    pub fn extract_xmp_impl<R: Read + Seek>(...) -> Result<Option<Vec<u8>>> {
        // PNG-specific logic:
        // - Read directly from iTXt chunk
        // - No extended XMP (PNG doesn't support it)
        ...
    }
    
    /// Extract JUMBF from PNG (direct data from caBX chunks)
    pub fn extract_jumbf_impl<R: Read + Seek>(...) -> Result<Option<Vec<u8>>> {
        // PNG-specific logic:
        // - Read directly from caBX chunks
        // - No headers to strip (PNG uses simple chunk format)
        ...
    }
}
```

### 4. Simplified `FileStructure` to delegate

```rust
pub fn xmp<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
    if self.xmp_index.is_none() {
        return Ok(None);
    }

    // Delegate to format-specific handler
    match self.format {
        Format::Jpeg => JpegHandler::extract_xmp_impl(self, reader),
        Format::Png => PngHandler::extract_xmp_impl(self, reader),
    }
}

pub fn jumbf<R: Read + Seek>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
    if self.jumbf_indices.is_empty() {
        return Ok(None);
    }

    // Delegate to format-specific handler
    match self.format {
        Format::Jpeg => JpegHandler::extract_jumbf_impl(self, reader),
        Format::Png => PngHandler::extract_jumbf_impl(self, reader),
    }
}
```

## Benefits

1. **Separation of Concerns**: Format-specific logic lives with format implementations
2. **Scalability**: Adding new formats (HEIF, WebP, AVIF, etc.) doesn't pollute `FileStructure`
3. **Maintainability**: JPEG XT headers, PNG chunks, BMFF boxes - each handled by experts in that format
4. **Testability**: Can unit-test format-specific extraction independently
5. **Documentation**: Each handler documents its own metadata conventions

## Format-Specific Details

### JPEG
**XMP:**
- **Storage**: APP1 markers with signature `http://ns.adobe.com/xap/1.0/\0`
- **Extended XMP**: Yes (for XMP > 64KB)
  - Multi-segment with GUIDs, offsets, total_size
  - Main XMP has pointer via `xmpNote:HasExtendedXMP`
  - Extended parts assembled by chunk_offset
- **Assembly**: Complex multi-part reassembly with validation

**JUMBF:**
- **Storage**: APP11 markers with JPEG XT headers
- **Multi-segment**: Yes (for JUMBF > 64KB)
- **Headers**: 8-byte JPEG XT header (CI + En + Z) per segment
- **Assembly**: Strip headers, concatenate data from all segments

### PNG
**XMP:**
- **Storage**: iTXt chunks with keyword `XML:com.adobe.xmp\0`
- **Extended XMP**: No (PNG doesn't support it)
- **Assembly**: Simple single-chunk read

**JUMBF:**
- **Storage**: caBX chunks (C2PA Box)
- **Multi-segment**: No (PNG chunks can be larger)
- **Headers**: None (direct JUMBF data)
- **Assembly**: Simple read from chunk data

### Future Formats
- **HEIF/HEIC**: ISO BMFF boxes, likely no extended XMP
- **WebP**: RIFF chunks, similar to PNG
- **AVIF**: ISO BMFF boxes with AV1 encoding

Each will have its own `extract_xmp_impl` and `extract_jumbf_impl` with format-appropriate logic.

## Migration Guide

If you were directly using `FileStructure::xmp()` or `FileStructure::jumbf()`, no changes needed - the API is the same.

If you were implementing custom format handlers, add both methods:

```rust
impl FormatHandler for MyHandler {
    fn parse<R: Read + Seek>(&self, reader: &mut R) -> Result<FileStructure> { ... }
    fn write<R: Read + Seek, W: Write>(...) -> Result<()> { ... }
    
    // NEW: Required methods
    fn extract_xmp<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        // Your format-specific XMP extraction logic
        ...
    }
    
    fn extract_jumbf<R: Read + Seek>(
        &self,
        structure: &FileStructure,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        // Your format-specific JUMBF extraction logic
        ...
    }
}
```

## Testing

All 39 tests pass after refactoring:
- ✅ JPEG extended XMP assembly
- ✅ JPEG multi-segment JUMBF assembly  
- ✅ PNG single-chunk XMP reading
- ✅ PNG single-chunk JUMBF reading
- ✅ Round-trip preservation
- ✅ Metadata modifications

The refactoring is **behavior-preserving** - no functional changes, only architectural improvements.

## Key Insight

**Extended XMP is JPEG-specific!** PNG doesn't support multi-chunk XMP assembly. This was a critical finding during the refactoring - the original code in `FileStructure` was doing extended XMP assembly for all formats, but only JPEG uses this mechanism.

