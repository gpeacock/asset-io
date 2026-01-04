# API Improvement: `Asset::write()` Now Returns `Structure`

## Change Summary

Made `Asset::write()` consistent with `Asset::write_with_processing()` by returning the destination `Structure` instead of `()`.

## Before

```rust
pub fn write<W: Write>(&mut self, writer: &mut W, updates: &Updates) -> Result<()>
pub fn write_to<P: AsRef<Path>>(&mut self, path: P, updates: &Updates) -> Result<()>
pub fn write_with_processing<W, F>(...) -> Result<Structure>  // Only this returned Structure
```

**Problem**: Inconsistent API - users needed to reopen/reparse the file to get the structure for `update_segment()`.

## After

```rust
pub fn write<W: Write>(&mut self, writer: &mut W, updates: &Updates) -> Result<Structure>
pub fn write_to<P: AsRef<Path>>(&mut self, path: P, updates: &Updates) -> Result<Structure>
pub fn write_with_processing<W, F>(...) -> Result<Structure>  // All consistent now!
```

**Benefit**: Consistent API - all write methods return the structure for immediate use.

---

## Impact on BMFF Workflow

### Before (with Asset::open):

```rust
// Write file
asset.write(&mut output_file, &updates)?;  // Returns ()
output_file.flush()?;

// Hash
output_file.seek(SeekFrom::Start(0))?;
bmff_hash.gen_hash_from_stream(&mut output_file)?;

// Update manifest
let update_asset = Asset::open(&output_path)?;  // ❌ Reopen file!
let structure = update_asset.structure();       // ❌ Re-parse structure!
structure.update_segment(&mut output_file, SegmentKind::Jumbf, final_manifest)?;
```

**Cost**: 
- Extra `Asset::open()` call
- Re-parses entire file structure
- Reads all box headers again
- Opens additional file handle (briefly)

---

### After (using returned Structure):

```rust
// Write file
let structure = asset.write(&mut output_file, &updates)?;  // ✅ Returns Structure!
output_file.flush()?;

// Hash  
output_file.seek(SeekFrom::Start(0))?;
bmff_hash.gen_hash_from_stream(&mut output_file)?;

// Update manifest
structure.update_segment(&mut output_file, SegmentKind::Jumbf, final_manifest)?;  // ✅ Direct use!
```

**Benefit**:
- No extra file operations
- No re-parsing
- No additional reads
- Cleaner, more efficient code

---

## Performance Improvement

### Operations Eliminated:
1. ❌ `Asset::open()` syscall
2. ❌ File structure parsing pass
3. ❌ Reading all BMFF box headers
4. ❌ Temporary file handle allocation

### Estimated Savings:
- **~1-2ms** for small files (few KB of headers)
- **~5-10ms** for complex files (many boxes/tracks)
- **More efficient** resource usage (fewer file handles, less memory)

---

## Implementation Details

The structure was already being computed internally during write via `calculate_updated_structure()` - we just weren't returning it! This change simply exposes what was already available.

```rust
pub fn write<W: Write>(&mut self, writer: &mut W, updates: &Updates) -> Result<Structure> {
    // Calculate destination structure (already happened internally!)
    let dest_structure = self
        .handler
        .calculate_updated_structure(&self.structure, updates)?;

    self.source.seek(SeekFrom::Start(0))?;
    self.handler
        .write(&self.structure, &mut self.source, writer, updates)?;

    Ok(dest_structure)  // Now return it!
}
```

---

## Migration Guide

### For users who ignored the return value:

**Before:**
```rust
asset.write(&mut output, &updates)?;
```

**After (no change needed, just ignore the return):**
```rust
asset.write(&mut output, &updates)?;  // Still works!
// Or explicitly:
let _structure = asset.write(&mut output, &updates)?;
```

### For users who needed the structure:

**Before:**
```rust
asset.write(&mut output, &updates)?;
let asset = Asset::open(&output_path)?;  // Had to reopen
let structure = asset.structure();
```

**After:**
```rust
let structure = asset.write(&mut output, &updates)?;  // Direct use!
```

---

## Test Results

✅ All 6 formats passing with the new API:
- sample1.png
- sample1.heic
- sample1.heif
- sample1.m4a
- sample1.avif
- Designer.jpeg

---

## Consistency Win

This change makes the entire `Asset` write API consistent:

| Method | Returns | Use Case |
|--------|---------|----------|
| `write()` | `Result<Structure>` | Basic write with updates |
| `write_to()` | `Result<Structure>` | Write to file path |
| `write_with_processing()` | `Result<Structure>` | Write with callback |

All three now return `Structure` for downstream use! 🎉
