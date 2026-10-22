# Fuzzing for asset-io

This directory contains fuzzing infrastructure for `asset-io` using `cargo-fuzz` (libFuzzer).

## Quick Start

```bash
# Build all fuzz targets
./fuzz.sh build

# Run the parsing fuzzer for 5 minutes
./fuzz.sh parse 300

# Run all fuzzers for 2 minutes each
./fuzz.sh all 120
```

## Fuzz Targets

### `fuzz_parse` - General File Parsing
Tests the core parsing logic across all supported formats (JPEG, PNG, BMFF containers).
This is the most important target as it exercises the attack surface of untrusted file input.

**What it tests:**
- Auto-detection of file formats
- Structure parsing
- Segment discovery
- XMP/JUMBF location detection
- Thumbnail extraction
- Metadata access

**Corpus location:** `fuzz/corpus/fuzz_parse/`

### `fuzz_write` - Write Operations
Tests file writing with various update configurations.

**What it tests:**
- Streaming write operations
- Segment updates (add/remove/modify)
- XMP modifications
- JUMBF modifications
- Structure preservation

**Corpus location:** `fuzz/corpus/fuzz_write/`

### `fuzz_xmp` - XMP Metadata Handling
Tests XMP parsing and modification logic.

**What it tests:**
- XML/RDF parsing
- XMP property access
- XMP property modification
- Batch updates
- Edge cases (empty keys, long strings, etc.)

**Corpus location:** `fuzz/corpus/fuzz_xmp/`

## Manual Usage

```bash
# Install cargo-fuzz (if not already installed)
cargo install cargo-fuzz

# Run a specific target
cargo +nightly fuzz run fuzz_parse

# Run with custom duration (5 minutes)
cargo +nightly fuzz run fuzz_parse -- -max_total_time=300

# Run with multiple workers (parallel fuzzing)
cargo +nightly fuzz run fuzz_parse -- -workers=4

# Run with a specific seed input
cargo +nightly fuzz run fuzz_parse fuzz/corpus/fuzz_parse/sample1.png
```

## Adding Corpus Seeds

Good corpus seeds help fuzzing find issues faster:

```bash
# Add JPEG files
cp my_test.jpg fuzz/corpus/fuzz_parse/

# Add HEIC files
cp my_photo.heic fuzz/corpus/fuzz_parse/

# Add XMP samples
cp my_xmp.xml fuzz/corpus/fuzz_xmp/
```

## Crashes and Artifacts

When fuzzing finds an issue, artifacts are saved to:
- `fuzz/artifacts/fuzz_parse/` - Parse crashes
- `fuzz/artifacts/fuzz_write/` - Write crashes  
- `fuzz/artifacts/fuzz_xmp/` - XMP crashes

To reproduce a crash:

```bash
cargo +nightly fuzz run fuzz_parse fuzz/artifacts/fuzz_parse/crash-xyz
```

## CI Integration

For continuous fuzzing, consider:

1. **OSS-Fuzz** (recommended for open source projects)
   - Automatic fuzzing infrastructure
   - Continuous fuzzing on Google's infrastructure
   - Automatic bug filing

2. **GitHub Actions**
   - Run fuzzers for a fixed time on each PR
   - Example workflow included in `.github/workflows/fuzz.yml`

3. **Nightly Runs**
   - Schedule longer fuzzing runs (hours/overnight)
   - Upload artifacts on failure

## Performance Tips

```bash
# Use multiple workers for parallel fuzzing
cargo +nightly fuzz run fuzz_parse -- -workers=8

# Limit memory per worker (prevent OOM)
cargo +nightly fuzz run fuzz_parse -- -rss_limit_mb=2048

# Use a dictionary for faster discovery
cargo +nightly fuzz run fuzz_parse -- -dict=fuzz/bmff.dict
```

## What We're Testing For

Fuzzing helps catch:

- ✅ **Panics** - Any `unwrap()`, `expect()`, or bounds violations
- ✅ **Integer overflows** - In size calculations or arithmetic
- ✅ **Buffer overruns** - Reading/writing past buffer end
- ✅ **Infinite loops** - Malformed structures causing parsing loops
- ✅ **OOM (Out of Memory)** - Malicious sizes causing huge allocations
- ✅ **Logic errors** - Edge cases in format-specific handling
- ✅ **Memory safety** - Use-after-free, double-free (via sanitizers)

## Expected Results

With our current hardening:
- ✅ No panics (we removed all `unwrap()` calls)
- ✅ All errors handled via `Result<T>`
- ✅ Bounds checking on all array accesses
- ⚠️ May find edge cases in format-specific logic

## Current Status

- **Last Run:** Not yet run
- **Coverage:** TBD
- **Known Issues:** None

## Resources

- [cargo-fuzz book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
- [libFuzzer options](https://llvm.org/docs/LibFuzzer.html#options)
- [Rust Fuzz Trophy Case](https://github.com/rust-fuzz/trophy-case)
