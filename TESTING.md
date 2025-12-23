# Testing Guide

This document explains the test fixture system in `jumbf-io` and how to run tests with different configurations.

## Test Fixture System

The library uses a flexible fixture system inspired by [c2pa-rs](https://github.com/contentauth/c2pa-rs) that supports:

1. **Committed fixtures** - A minimal set (8 files) committed to the repository
2. **Embedded fixtures** - Small files compiled into the test binary for fast CI
3. **Extended fixtures** - Optional large test sets in external directories

### Features

- **`test-utils`** - Test utilities API (enabled by default for development)
- **`embed-fixtures`** - Compiles fixtures into the binary for zero I/O overhead

## Running Tests

### Basic Tests (File-Based)

Run tests with fixtures loaded from `tests/fixtures/`:

```bash
cargo test
cargo run --example test_fixtures
cargo run --example test_xmp_extended
```

Note: `test-utils` is enabled by default, so examples and tests "just work"!

### Fast CI Tests (Embedded)

Compile fixtures into the binary for maximum speed:

```bash
cargo test --features embed-fixtures
cargo run --example test_fixtures --features embed-fixtures
```

When enabled, you'll see 📦 icons for embedded fixtures vs 📁 for file-based.

### Production Builds

For production builds, disable test utilities to reduce binary size:

```bash
cargo build --release --no-default-features --features jpeg
```

### Extended Test Set

For comprehensive testing with a larger fixture set:

```bash
# Set environment variable to extended fixtures directory
export JUMBF_TEST_FIXTURES=/path/to/extended/fixtures

# Run tests - will test both committed + extended fixtures
cargo run --example test_fixtures
```

This is useful for:
- Testing with real-world large files (>100MB)
- Testing edge cases not in the committed set
- CI jobs with access to shared fixture repositories
- Local development with proprietary test images

## Fixture Organization

### Committed Fixtures (`tests/fixtures/`)

3 files totaling ~1.1 MB:

- **Designer.jpeg** (127KB) - Small image with JUMBF only
- **FireflyTrain.jpg** (161KB) - Small image with both XMP and JUMBF
- **P1000708.jpg** (810KB) - Medium camera photo with XMP only

This minimal set covers the essential test cases while keeping the repository small.

### Extended Fixtures (Optional)

You can maintain a separate directory with hundreds of test images:

```
/Users/you/test-fixtures/
├── my_test_1.jpg
├── my_test_2.jpg
├── large_file_100mb.jpg
└── ...
```

Then point to it:

```bash
export JUMBF_TEST_FIXTURES=/Users/you/test-fixtures
```

The test system will automatically discover all JPEG files in this directory.

### Sharing Fixtures Across Projects

For organizations with multiple projects testing media files, you can:

1. **Git LFS Repository**: Store fixtures in a separate repo with Git LFS
2. **Shared Network Directory**: Mount a network drive with test fixtures
3. **CI Artifact Cache**: Download fixtures as part of CI setup

Example CI workflow:

```yaml
- name: Download Extended Fixtures
  run: |
    wget https://example.com/fixtures.tar.gz
    tar xzf fixtures.tar.gz
    echo "JUMBF_TEST_FIXTURES=$PWD/fixtures" >> $GITHUB_ENV

- name: Run Extended Tests (Fast)
  run: cargo test --features embed-fixtures
```

## Using Test Utilities in Code

The `test_utils` module provides a convenient API:

```rust
use jumbf_io::test_utils::*;

// Use predefined fixture constants
let path = fixture_path(FIREFLY_TRAIN);
println!("Testing: {}", path.display());

// Check if a fixture is embedded
if is_embedded(FIREFLY_TRAIN) {
    println!("Using embedded data (fast!)");
}

// Get test streams (works with embedded or file-based)
let (format, input, output) = create_test_streams(CAPTURE)?;
// Use the cursors for testing...

// List all available fixtures (committed + extended)
let all_fixtures = list_fixtures()?;
println!("Found {} fixtures", all_fixtures.len());

// Read fixture bytes directly
let data = fixture_bytes("my_test.jpg")?;
```

Available fixture constants:

- `DESIGNER`
- `FIREFLY_TRAIN`
- `P1000708`
- `ORIGINAL_ST_CAI`
- `IMG_0550`
- `CAPTURE`
- `DSC_0100`
- `L1000353`

## Adding New Fixtures

To add a fixture to the committed set:

1. Add the file to `tests/fixtures/`
2. Update `src/test_utils.rs`:

```rust
define_fixtures!(
    MY_NEW_TEST => ("my_new_test.jpg", "image/jpeg"),
    // ... existing fixtures
);
```

3. The fixture will automatically be:
   - Available via `fixture_path(MY_NEW_TEST)`
   - Embedded when `embed-fixtures` feature is enabled
   - Listed by `list_fixtures()`

## Future: Shared Fixture Crate

Eventually, we may extract this pattern into a shared crate:

```toml
[dev-dependencies]
test-fixtures = { git = "https://github.com/contentauth/test-fixtures" }
```

This would allow multiple projects to share the same test fixture infrastructure.

## Performance Comparison

| Mode | Fixture Load Time | CI Time | Binary Size |
|------|------------------|---------|-------------|
| File-based | ~50ms per file | Slower | Small |
| Embedded (small) | ~0ms | Fast | +5MB |
| Embedded (all) | ~0ms | Fastest | +39MB |
| Extended | Varies | Slowest | Small |

**Recommendations:**

- **Local development**: File-based (default) - `cargo run --example test_fixtures`
- **CI**: Embedded for speed - `cargo test --features embed-fixtures`
- **Extended testing**: Use `JUMBF_TEST_FIXTURES` for comprehensive tests
- **Production**: Disable test-utils - `cargo build --release --no-default-features --features jpeg`

