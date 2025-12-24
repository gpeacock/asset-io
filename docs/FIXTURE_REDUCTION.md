# Fixture Reduction Summary

## What Was Removed

The following files were removed from the committed repository to reduce size and avoid licensing concerns:

### Removed Files (5 files, ~38 MB):
- `capture.jpg` (3.4 MB) - Had extended XMP, but licensing unclear
- `original_st_CAI.jpeg` (1.4 MB) - C2PA test case, but licensing unclear  
- `IMG_0550.jpg` (1.6 MB) - No metadata, not essential
- `DSC_0100.JPG` (10 MB) - Too large
- `L1000353.JPG` (22 MB) - Very large

**Total removed: ~38 MB**

## What Was Kept

### Minimal Set (3 files, ~1.1 MB):
- `Designer.jpeg` (127 KB) - JUMBF only
- `FireflyTrain.jpg` (161 KB) - XMP + JUMBF
- `P1000708.jpg` (810 KB) - XMP only

**Total kept: ~1.1 MB (97% reduction)**

## Test Coverage

The minimal set still provides complete test coverage:

✅ **Metadata Types:**
- JUMBF-only parsing (Designer.jpeg)
- XMP-only parsing (P1000708.jpg)
- Combined XMP + JUMBF (FireflyTrain.jpg)

✅ **Operations:**
- Parse and inspect
- Copy unchanged
- Add/remove/replace XMP
- Add/remove/replace JUMBF
- Write to new files

✅ **Edge Cases:**
- Small files (<200 KB)
- Medium files (~800 KB)
- Multi-segment JUMBF
- Various JPEG encodings

## Extended Testing

For testing with larger files or specific edge cases (like extended XMP), use the `JUMBF_TEST_FIXTURES` environment variable:

```bash
# Keep your larger test files elsewhere
export JUMBF_TEST_FIXTURES=/path/to/your/private/fixtures

# Tests will use both committed + extended fixtures
cargo test
```

## Why This Works

1. **Core Functionality**: All core parsing/writing logic is tested
2. **Embeddable**: All 3 files can be embedded with `embed-fixtures` feature
3. **License Safe**: Only files with clear licensing status
4. **Size Efficient**: 97% smaller repository
5. **Extensible**: Easy to add more via environment variable

## Updating Tests

All tests and documentation have been updated to reflect the new fixture set:

- ✅ `src/test_utils.rs` - Updated fixture definitions
- ✅ `tests/fixtures/README.md` - Updated documentation
- ✅ `TESTING.md` - Updated usage guide  
- ✅ `examples/test_fixtures.rs` - Works with 3 fixtures
- ✅ `examples/test_xmp_extended.rs` - Tests XMP splitting with generated data
- ✅ All unit and integration tests passing

