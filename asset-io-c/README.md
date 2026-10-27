# asset-io application C bindings (ABI 1)

This **separate, standalone Cargo package** uses the `asset-io` facade and keeps
its own JPEG/PNG defaults; the Rust facade has no default handlers.
It produces `libasset_io_c.{dylib,so,a}` (Windows: DLL/import library)
and an `rlib`. Applications in C/C++, Swift or Python can use the C ABI. This is
**not** a Rust handler/plugin ABI. There is no dynamic handler loading, registration
of foreign handlers, or registry sharing between separately loaded libraries.

## Reproducible builds

From the repository root (Rust 1.96+):

```sh
# No built-in handlers:
cargo build --locked --manifest-path asset-io-c/Cargo.toml --no-default-features
# Default JPEG and PNG:
cargo build --locked --manifest-path asset-io-c/Cargo.toml
# All formats, optional core inspectors:
cargo build --locked --manifest-path asset-io-c/Cargo.toml \
  --features all-formats,xmp,exif
```

Individual `jpeg`, `png`, `bmff`, `riff`, `svg`, `pdf`, `tiff`, `c2pa`, `xmp`, `exif`,
`memory-mapped`, `flate` and `brotli` (transfer-encoding decoders) switches forward
through the facade without enabling additional handlers.
Inspector switches compile core functionality; inspector-specific methods are
not currently exported. Query the registry rather than assuming feature presence.
The no-default library can construct registries but cannot parse any format.

`include/asset_io.h` is checked in and usable without Rust tooling by consumers.
`build.rs` uses pinned cbindgen 0.29.2 and verifies that the header matches source;
it writes normal generated output only under Cargo's build directory.
After changing declarations, regenerate and commit the header:

```sh
ASSET_IO_UPDATE_HEADER=1 cargo build --locked --manifest-path asset-io-c/Cargo.toml \
  --target-dir asset-io-c/target
cargo test --locked --manifest-path asset-io-c/Cargo.toml
cc -std=c11 -Wall -Wextra -Werror -Iasset-io-c/include \
  asset-io-c/examples/smoke.c -Lasset-io-c/target/debug -lasset_io_c \
  -Wl,-rpath,"$PWD/asset-io-c/target/debug" -o asset-io-c/target/c-smoke
asset-io-c/target/c-smoke
```

The explicit `--target-dir` makes the linker example independent of global Cargo
target-directory settings. The linker example works on macOS/Linux. Windows consumers must supply their
platform's DLL search path/import library configuration.

## Errors, ownership and safety contract

Pointer-returning operations return NULL on failure. Status operations return 0
on success, -1 on error. `asset_io_last_error()` returns a newly owned buffer of
UTF-8 `Variant: details`, or NULL if no error has occurred on this thread.
Errors remain until replaced by another error; success does **not** clear them.
Read the error immediately after a failure. Error access itself does not clear it.
Rust unwinding panics are caught at operation boundaries; allocator aborts,
`panic=abort`, invalid C memory, and exceptions unwinding through C callbacks
cannot be recovered.

All returned handles, including error and JSON buffers, must be freed exactly once
using **this originating library's** `asset_io_free`. Do not use C `free`, dereference
opaque handles, or pass them to another copy of the library. cimpl 0.4.1 tracks
types, live borrows, and freed IDs: null/wrong-type/stale handles fail rather than
becoming Rust references. Freeing NULL succeeds. Handle copies do not add owners.

`asset_io_buffer_data` borrows contiguous bytes until that buffer is freed; it
requires a writable `size_t*`. Empty successful buffers have length zero and a
non-NULL pointer (never dereference it). Buffers are immutable. Never free a buffer
concurrently with reading its borrowed storage. All non-handle C pointers must
point to valid, appropriately sized memory; the library can validate null/length
and UTF-8 but cannot prove arbitrary addresses valid. Zero-length input may be NULL.
Format strings are NUL-terminated UTF-8, or NULL for detection.

Registry, structure and buffer operations borrow immutable handles. Asset I/O and
update mutations take exclusive tracked borrows. Concurrent incompatible borrows
fail. Callbacks must not reenter I/O on their active asset or mutate
handles used by the outer call; borrow conflicts are errors. As in cimpl, freeing
an actively borrowed handle invalidates its ID immediately but defers destruction
until the outer operation releases its guard. It does not cancel the operation:
the callback context must still remain live until that operation returns.
Do not free handles from callbacks unless you deliberately want this behavior.
Do not use handles
across `fork`. External synchronization is required for caller-owned memory.

Memory parsing copies input once, allowing caller input to be released immediately.
Callback parsing **never** copies the whole source. The callback function, context,
and underlying immutable input must remain valid until the asset is freed.
Operations invoke callbacks synchronously on the calling thread, not a worker;
if an asset moves between threads its context must support this. A read-at callback
may return short positive reads (retried by core readers), negative for failure;
zero before the advertised size is unexpected EOF. It must never return more than
requested and must initialize every byte it reports. No callbacks occur after
free or after an operation returns, except during subsequent operations on the asset.

Streaming output is delivered once in ordered tagged chunks, with no output-sized
allocation. The callback must consume/hash/copy the entire borrowed chunk before
returning 0; any nonzero result fails the operation. Partial output must be discarded
on failure. The chunk's pointer is invalid after the callback. Use `asset_io_plan`
first when hashing needs complete output geometry. All offsets start at output
byte zero. The output stream must not overwrite the callback source during writing.
`asset_io_read_stream` provides the same tagged callback for source bytes, for
read-side hashing, using bounded memory. Its offsets/segment IDs describe the
original source, not planned output. `asset_io_read_range` copies an arbitrary
validated source range into an owned buffer, subject to the core's 256 MiB limit.

## Coverage and inspection schema

The header exports registry creation/format aliases, explicit/detected memory or
read-at parsing, tagged source streaming, bounded range reads, independent structure snapshots,
item extraction (`asset_io_item_bytes`: `decoded=0` gives the stored bytes, which
are what a hash covers; `decoded=1` gives the content with the item's `transfer`
undone),
logical XMP/JUMBF extraction, JUMBF UUID extraction, XMP/JUMBF keep/set/remove,
type-scoped JUMBF removal, remove-all, BMFF update-manifest hint, output planning,
memory writing and tagged streaming writing.

`asset_io_structure_json` returns UTF-8 JSON, **not NUL terminated**. Copy the bytes
or parse with a length-aware JSON API. Schema version `schema: 1` contains:

* `container`, `media_type`, `size`; nullable `trailing` range.
* `segments`: declaration/file order; `kind` is the exact core string (`preamble`,
  `metadata`, `media_data`, `other`); `range`, `regions`, `subdivides`, lossless
  `name`, and nullable container-defined `box_name`.
* Names: `{type:"none"}`, `{type:"marker",value:u8}`,
  `{type:"fourcc",bytes:[u8;4]}`, `{type:"id",value:u64}`. Future core variants
  are explicitly tagged `future` with their debug representation, never flattened.
* `items`: declaration order (indices used for extraction); `kind` (`xmp`, `jumbf`,
  `exif`, `embedded_asset`, `other`), `category` (`metadata`, `asset`, `other`),
  `encoding` (`xml`, `tiff`, `jumbf`, `iso_bmff`, `asset`, `unknown`); `transfer`,
  how the stored bytes wrap the content: `{type:T}` with `T` one of `identity`,
  `base64`, `zlib`, `deflate`, `gzip`, `brotli`, `unknown`; `zlib` adds
  `png_predictor_columns` (u32 or null) and `unknown` adds `name`, the format's
  own label (future kinds keep the `type` field). Decoding with `decoded=1` needs
  the library's `flate`/`brotli` features and fails with `UnsupportedFeature`
  otherwise; nullable
  `label`, `media_type`, `jumbf_type` (16-byte array), `native_type` (a `name`),
  `native_id` and `uri`; `relations` (`kind`, `native` as a 4-byte array, and the
  `target` item's `native_id`); stored `size`; and `extents` with owning `segment`
  index and `range`.
* `subassets`: `range`, `[segment_start,segment_end)` indices, nullable
  `media_type`, `primary`.
* `regions`: all stored regions in order, with `segment`, `role`, `range`.
  Roles are integer 0=header, 1=payload, 2=trailer.
* Every range is `{offset:u64,length:u64}`. JSON consumers must preserve 64-bit
  integers (do not round through IEEE-754 doubles). Unknown fields may be added.

Snapshots retain no source and survive freeing assets/updates/registries.
Planned snapshots describe future output, not readable source bytes.
UUIDs absent in the snapshot can be queried with `asset_io_item_jumbf_type`
(reads at most the core's 32-byte prefix).

Set replaces the first matching item and drops other matching items as defined
by the core. A typed JUMBF superbox automatically scopes a set to its type in
containers that distinguish types; an untyped value targets any JUMBF.
Removal without a type removes every JUMBF type. `remove_all` resets the builder
to the core's remove-all state, including container-specific metadata defaults;
later explicit setters override the corresponding metadata choice.
Missing logical metadata is `NotFound`; multiple unrelated stores remain
`Ambiguous`, not silently selected. Empty stored payload is not missing metadata.

Not exported: custom handler registration/traits, futures/tasks/execution stepping,
borrowed write-plan steps/relocation internals, lazy hierarchy walking/subdivision,
read-side sink filtering, in-place patching, Rust extension
options (e.g. BMFF merkle options), inspector-specific EXIF/XMP/thumbnail helpers,
or direct memory-mapped-file constructors. The forwarding switches alone do not
imply these APIs exist. Core limits/errors and each container's support/refusal
semantics are preserved; there is no fallback implementation for unsupported work.
