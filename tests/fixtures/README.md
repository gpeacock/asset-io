# Test Fixtures

This directory contains a minimal set of JPEG test files for the jumbf-io library.

## Committed Fixtures (3 files, ~1.1 MB)

The following fixtures are committed to the repository:

- **Designer.jpeg** (127KB) - Small image with JUMBF only
- **FireflyTrain.jpg** (161KB) - Small image with both XMP and JUMBF
- **P1000708.jpg** (810KB) - Medium camera photo with XMP only

These three files cover the core test cases:
- JUMBF-only metadata
- XMP-only metadata  
- Both XMP and JUMBF together

## Extended Fixtures (Optional)

For more comprehensive testing, you can use a larger fixture set by setting the `JUMBF_TEST_FIXTURES` environment variable to point to an extended directory:

```bash
export JUMBF_TEST_FIXTURES=/path/to/extended/fixtures
cargo run --example test_fixtures --features test-utils
```

This allows you to test against a broader set of images without committing large files to the repository.

## Embedding Fixtures

For fast CI tests and no file I/O overhead, you can compile small fixtures directly into the test binary:

```bash
cargo test --features embed-fixtures
```

This embeds the smallest fixtures (<1MB) into the binary while larger files still use file I/O.

## Adding New Fixtures

To add a new fixture:

1. Add the file to `tests/fixtures/`
2. Add it to the `define_fixtures!` macro in `tests/common/mod.rs`
3. Specify the format (e.g., "image/jpeg")

Example:
```rust
define_fixtures!(
    MY_NEW_TEST => ("my_test.jpg", "image/jpeg"),
    // ... existing fixtures
);
```

## Using in Tests

```rust
use jumbf_io::test_utils::*;

// Use predefined constants
let path = fixture_path(FIREFLY_TRAIN);

// Or get test streams (embedded or file-based)
let (format, input, output) = create_test_streams(CAPTURE)?;

// List all available fixtures
let all_fixtures = list_fixtures()?;
```
