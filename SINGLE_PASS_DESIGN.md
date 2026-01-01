# True Single-Pass Optimization Design

## Current Implementation Analysis

The current `write_with_processing` does 2 passes:
1. **Write pass**: Call `handler.write()` to write the full file
2. **Process pass**: Re-read the file to process chunks

## Problem

Looking at container handlers' `write` methods (e.g., `png_io.rs`):

```rust
fn write<R: Read + Seek, W: Write>(&self, structure: &Structure, source: &mut R, writer: &mut W, updates: &Updates) -> Result<()> {
    // ... lots of writer.write_all() calls ...
    writer.write_all(PNG_SIGNATURE)?;
    writer.write_all(&buffer)?;
    Self::write_chunk(writer, C2PA, new_jumbf)?;
    // ... etc
}
```

Each `write` call goes directly to the output stream. We need to intercept these writes to process the data.

## Solution Approaches

### Option 1: Processor Wrapper (Recommended)

Create a wrapper around the `Write` trait that intercepts `write_all` calls and processes data before forwarding to the real writer.

```rust
struct ProcessingWriter<W: Write, F: FnMut(&[u8])> {
    writer: W,
    processor: F,
    exclude_mode: bool,  // If true, don't process current writes
}

impl<W: Write, F: FnMut(&[u8])> Write for ProcessingWriter<W, F> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.exclude_mode {
            (self.processor)(buf);
        }
        self.writer.write(buf)
    }
    
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        if !self.exclude_mode {
            (self.processor)(buf);
        }
        self.writer.write_all(buf)
    }
    
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write, F: FnMut(&[u8])> ProcessingWriter<W, F> {
    fn set_exclude_mode(&mut self, exclude: bool) {
        self.exclude_mode = exclude;
    }
}
```

**Usage in handler's write method:**

```rust
// Before writing JUMBF
processing_writer.set_exclude_mode(true);
Self::write_chunk(&mut processing_writer, C2PA, new_jumbf)?;
processing_writer.set_exclude_mode(false);
```

**Pros:**
- ✅ Truly single-pass - no re-reading
- ✅ Minimal changes to handler code
- ✅ Zero-copy - process data as it's written
- ✅ Works with any writer (File, Vec, etc.)

**Cons:**
- ❌ Requires handlers to know when to exclude
- ❌ Needs coordination between writer and handler

### Option 2: Modify ContainerIO Trait

Add an optional processor parameter to the `write` method:

```rust
trait ContainerIO {
    fn write<R: Read + Seek, W: Write>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
        processor: Option<&mut SegmentProcessor>,  // NEW
    ) -> Result<()>;
}

struct SegmentProcessor {
    callback: Box<dyn FnMut(&[u8])>,
    exclude_kinds: HashSet<SegmentKind>,
    current_kind: SegmentKind,
}
```

**Pros:**
- ✅ Explicit in the API
- ✅ Handlers control when to process
- ✅ Type-safe

**Cons:**
- ❌ Breaking API change
- ❌ Requires updating all container handlers
- ❌ More complex handler implementations

### Option 3: Hybrid Approach (Best)

Combine both approaches:
1. Create `ProcessingWriter` wrapper
2. Keep `ContainerIO::write` signature unchanged
3. Add a new `write_with_processor` method to handlers (optional)

```rust
trait ContainerIO {
    // Existing method - unchanged
    fn write<R: Read + Seek, W: Write>(...) -> Result<()>;
    
    // New method - optional, for single-pass optimization
    fn write_with_processor<R: Read + Seek, W: Write, F: FnMut(&[u8])>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
        exclude_segments: &[SegmentKind],
        mut processor: F,
    ) -> Result<()> {
        // Default implementation: wrap writer
        let mut processing_writer = ProcessingWriter::new(writer, |data| processor(data));
        self.write(structure, source, &mut processing_writer, updates)
        // Note: This default still can't exclude segments intelligently
    }
}
```

Then, container handlers can **optionally** override `write_with_processor` to:
1. Use `ProcessingWriter`
2. Call `set_exclude_mode(true)` before writing excluded segments
3. Call `set_exclude_mode(false)` after

**Pros:**
- ✅ Backward compatible (default implementation available)
- ✅ Handlers can opt-in to optimization
- ✅ Graceful degradation (falls back to 2-pass if not implemented)
- ✅ Clean API

**Cons:**
- ⚠️ Requires explicit handler updates for full benefit
- ⚠️ Default implementation can't intelligently exclude segments

## Implementation Plan

### Phase 1: Add ProcessingWriter

```rust
// In src/lib.rs or new src/processing_writer.rs
pub(crate) struct ProcessingWriter<W: Write, F: FnMut(&[u8])> {
    writer: W,
    processor: F,
    exclude_mode: bool,
}
```

### Phase 2: Update ContainerIO Trait

```rust
// In src/formats/mod.rs
pub trait ContainerIO {
    // ... existing methods ...
    
    /// Write with processor callback for single-pass processing
    /// 
    /// Default implementation wraps the writer but cannot intelligently
    /// exclude segments. Handlers should override this for optimal performance.
    fn write_with_processor<R: Read + Seek, W: Write, F: FnMut(&[u8])>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
        exclude_segments: &[SegmentKind],
        processor: F,
    ) -> Result<()> {
        // Default: just wrap and process everything
        let mut processing_writer = ProcessingWriter::new(writer, processor);
        self.write(structure, source, &mut processing_writer, updates)?;
        Ok(())
    }
}
```

### Phase 3: Update Asset::write_with_processing

```rust
impl<R: Read + Seek> Asset<R> {
    pub fn write_with_processing<W, F>(
        &mut self,
        writer: &mut W,
        updates: &Updates,
        chunk_size: usize,
        exclude_segments: &[SegmentKind],
        processor: &mut F,
    ) -> Result<Structure>
    where
        W: Write + Seek,  // Remove Read requirement!
        F: FnMut(&[u8]),
    {
        // Calculate destination structure
        let dest_structure = self
            .handler
            .calculate_updated_structure(&self.structure, updates)?;

        // Use new write_with_processor method
        self.source.seek(SeekFrom::Start(0))?;
        self.handler.write_with_processor(
            &self.structure,
            &mut self.source,
            writer,
            updates,
            exclude_segments,
            processor,
        )?;

        Ok(dest_structure)
    }
}
```

### Phase 4: Implement write_with_processor in Handlers

Update each handler (JpegIO, PngIO, BmffIO) to override `write_with_processor`:

```rust
impl ContainerIO for PngIO {
    // ... existing methods ...
    
    fn write_with_processor<R: Read + Seek, W: Write, F: FnMut(&[u8])>(
        &self,
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
        exclude_segments: &[SegmentKind],
        processor: F,
    ) -> Result<()> {
        let mut processing_writer = ProcessingWriter::new(writer, processor);
        
        // ... same as write() but with exclude_mode control ...
        
        // Before writing JUMBF
        if exclude_segments.contains(&SegmentKind::Jumbf) {
            processing_writer.set_exclude_mode(true);
        }
        Self::write_chunk(&mut processing_writer, C2PA, new_jumbf)?;
        processing_writer.set_exclude_mode(false);
        
        // ... rest of write logic ...
        
        Ok(())
    }
}
```

## Performance Expectations

### Current (2-pass):
- Write: 1 full pass (source → destination)
- Read-process: 1 full pass (destination → processor)
- Total: **2 full I/O operations**

### Optimized (true single-pass):
- Write-process: 1 pass (source → destination + processor)
- Total: **1 full I/O operation**

**Expected improvement:**
- 50% reduction in I/O operations
- ~2x faster for I/O-bound workloads
- Even bigger gains for network-mounted files

## Testing Strategy

1. **Backward compatibility**: Ensure default implementation works
2. **Correctness**: Verify excluded segments are actually excluded
3. **Performance**: Benchmark single-pass vs 2-pass
4. **All containers**: Test JPEG, PNG, BMFF

## Migration Path

**Users don't need to change anything!** The API stays the same:

```rust
let structure = asset.write_with_processing(
    &mut output,
    &updates,
    8192,
    &[SegmentKind::Jumbf],
    &mut |chunk| hasher.update(chunk),
)?;
```

Handlers that implement `write_with_processor` get automatic performance boost.
Handlers that don't still work (fall back to default implementation).

## Next Steps

1. Implement `ProcessingWriter`
2. Add `write_with_processor` to `ContainerIO` trait
3. Update `Asset::write_with_processing` to use new method
4. Implement for JpegIO (most common use case)
5. Benchmark and validate
6. Implement for PngIO and BmffIO
7. Update documentation
