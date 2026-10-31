# Test Fixtures

This is the workspace's shared fixture corpus. Container-family integration
tests live in their owning crates under `crates/`, while facade cross-container
tests and C binding tests also consume these files. Keeping the corpus here
avoids duplicating binary assets; core tests use synthetic handlers instead.

This directory contains a minimal set of JPEG test files for the asset-io library.

## Committed Fixtures (3 files, ~1.1 MB)

The following fixtures are committed to the repository:

- **Designer.jpeg** (127KB) - Small image with JUMBF only
- **FireflyTrain.jpg** (161KB) - Small image with both XMP and JUMBF
- **P1000708.jpg** (810KB) - Medium camera photo with XMP only

These three files cover the core test cases:
- JUMBF-only metadata
- XMP-only metadata  
- Both XMP and JUMBF together

## Synthetic PDF Fixture

The synthetic `pdf_metadata.pdf` fixture contains an uncompressed XMP stream and
a document-level C2PA attachment association using classic cross-reference
tables. Its `manifest` payload is a placeholder, not a valid C2PA manifest. It is
used directly by the PDF integration tests and can seed the parsing fuzzer.

## TIFF Reference Fixture

`tiff_multipage.tif` is the unmodified `sdk/tests/fixtures/MultiPage.tif` from
[contentauth/c2pa-rs at fea8d1c5ac4f37cc2e258dc65a045ddb47c5b17b](https://github.com/contentauth/c2pa-rs/blob/fea8d1c5ac4f37cc2e258dc65a045ddb47c5b17b/sdk/tests/fixtures/MultiPage.tif),
used under that repository's MIT OR Apache-2.0 license. It exercises the TIFF
handler's real multi-page metadata lifecycle in addition to synthetic classic
TIFF, BigTIFF, and DNG cases.

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
use asset_io::test_utils::*;

// Use predefined constants
let path = fixture_path(FIREFLY_TRAIN);

// Or get test streams (embedded or file-based)
let (format, input, output) = create_test_streams(CAPTURE)?;

// List all available fixtures
let all_fixtures = list_fixtures()?;
```
