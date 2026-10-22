# PDF Implementation Notes

Enable `pdf` (also included in `all-formats`). A PDF is found by content or by the
`pdf` extension; read it with `Structure::xmp` and `Structure::store`, and write it with
`Structure::write`, as for any container (see the [README](README.md)).
This is **not a general PDF parser, renderer, sanitizer, or C2PA validator**.

## Reading and Metadata Discovery

PDF 1.0-1.7 and 2.0 headers are recognized at byte zero. The reader follows
classic tables, cross-reference streams, and incremental `/Prev` chains,
resolving the current catalog's `/Metadata` and document-level `/AF` C2PA
association. Cross-reference streams remain compact tables; compressed object
streams are decoded on demand, with only one object stream cached during
parsing. It does not load the document, decode page content, or search for
metadata markers. Unreferenced and superseded metadata are not exposed.

Each metadata stream, including those with an indirect `/Length`, is a segment holding
the stream's payload as it is in the file, and the entity for XMP or the C2PA store. The
segments between them are other bytes, so the structure tiles the file. Parsing skips the
payloads; `Structure::xmp()` and `Structure::store()` allocate only when called, and
decode Flate-compressed metadata at that point. A stream with no payload holds nothing to
expose, so it reads as absent.
The `pdf` feature enables `flate2` with its pure-Rust backend (no native zlib).
FlateDecode and 8-bit single-component PNG predictors are supported.

`Structure::read_entity`, `read_range`, a `Sink`, and optional memory mapping
(`slice_entity`) give streaming or borrowed access to **physical file bytes**. For
compressed metadata these are the encoded bytes, which is what a hash covers; the
structure's attachment records the encoding, and `xmp()` and `store()` decode it through
the handler's `decode_entity`. Memory mapping retains its existing unsafe API and
file-stability requirements.

Only document-level C2PA stores associated through the catalog's `/AF` are
discovered. Page/object-level manifests and attachment-only stores without that
association are outside this first version. C2PA file specifications require
`/AFRelationship /C2PA_Manifest` and `/Subtype /application#2Fc2pa`.
Multiple document-level C2PA stores are rejected rather than concatenated.

Encryption, hybrid-reference files, external metadata streams, other filter
types/chains, and other predictor layouts remain unsupported. Recognized
unsupported features return `Error::UnsupportedFeature`, not an empty metadata
result. Page/content streams are neither decoded nor executed.

## Resource Limits and Decompression Safety

Parsing is bounded to:

| Resource | Limit |
|----------|-------|
| Total cross-reference entries | 1,000,000 |
| Classic cross-reference entries | 100,000 |
| Revisions | 128 |
| Object resolutions | 4,096 |
| Nesting/reference depth | 32 |
| Syntax nodes | 100,000 |
| Tokens/names | 4,096 bytes |
| Syntax work | 16 MiB |
| Each structural stream, encoded and decoded | 8 MiB each |
| Cumulative structural input-plus-output decoding work | 32 MiB |
| Retained compact cross-reference data | 16 MiB |
| Metadata input / uncompressed retrieval | 256 MiB |
| Flate metadata output, including predictor row bytes | 64 MiB per retrieval |
| Predictor row size | 64 KiB |
| Object stream header entries | 100,000 |

Repeatedly decoding different object streams also consumes the cumulative
structural decoding budget. Returning a `Vec` necessarily allocates the
requested payload, but never the whole PDF. Retrieving compressed metadata reads the
encoded payload into memory (up to 256 MiB) before inflating it (up to 64 MiB).

The final `startxref` and `%%EOF` must occur within the last 1,024 bytes.
Limit violations and malformed/ambiguous structures return errors.

Checksum errors, incomplete streams, and trailing compressed data are rejected.
A narrow compatibility exception permits legacy **cross-reference** streams
ending in a zlib sync-flush marker without the final zlib trailer, only when
their decoded size exactly matches the declared table layout. Object and metadata
streams require full termination.

## Incremental Writing

A write is a `WritePlan`: the original bytes, copied unchanged, then an appended
revision. `set_xmp`, `remove_xmp`, `set_jumbf`, and `remove_jumbf` update the current
catalog. Newly written metadata streams are uncompressed. A no-change write is a
byte-for-byte copy. Because the plan is data, the output structure is known before
anything is written (`Structure::plan`), and it can be run by any driver. A stream the
catalog no longer points at is ordinary bytes in the output, so the planned structure
merges it into the bytes around it, as parsing the output does.

Catalog and trailer values, unrelated associated files, and unrelated named
attachments are preserved. Strings retain their byte values, although their
representation in rewritten dictionaries may change to hexadecimal.

C2PA embedding creates a file specification referenced by both catalog `/AF`
and `/Names /EmbeddedFiles`. The first writer supports flat, sorted name trees
(including indirect dictionaries/arrays), not `/Kids` trees. Existing active
C2PA attachments must already be registered in that tree; annotation-only or
unregistered associations are not writable. Attachment-name collisions are
errors, not overwrites.

Parsing keeps a bounded editing context in the structure's attachment, not
document/page payloads. Preflight conservatively reserves the existing syntax, object,
cross-reference, and revision budgets for a subsequent parse. Appended classic
cross-reference offsets must fit ten decimal digits. Unsupported writes fail
before any destination bytes are changed. Ordinary I/O or processor errors can
leave a caller-provided stream partially written. A sink that stops the write
with an error does the same.

### Forms, Signatures, and Certification

Metadata edits and in-place patches reject non-null catalog `/Perms`
(permissions/certification), XFA forms, and populated signature fields.
Ordinary unsigned AcroForms, including empty signature fields, are preserved.

Field inspection follows `/Fields` and `/Kids`, including inherited `/FT`
and `/V`, with at most 32 levels and 4,096 visited fields under the shared
parser budgets. Malformed or over-budget trees block editing, not metadata
retrieval. This detects common signing structures; it is not signature validation.

### Destination Files

`Structure::write` takes any `Write` for its output; it does not create, truncate, sync or
replace files. To replace a file safely (including the source, or a hard link to it), write
to a temporary file in the same directory and rename it over the destination. The
output stream must be distinct from the source, and the source bytes must remain stable
during use. The source's length is checked against the parse, and a write that fails
before output (an unsupported document, a limit) changes nothing.

### Exact-Length Finalization and Removal

`Structure::patch_exact` allows **exact-length, uncompressed** XMP/C2PA payload
replacement only (the handler's `check_patch` refuses anything else, and a document that
cannot be edited, before a byte is written). Reserve the
final manifest's exact size, stream/hash the output, then patch only those
payload bytes. No zero-padding, PDF signature creation, manifest generation,
or C2PA cryptographic validation is performed. A real manifest must come from
the caller; arbitrary placeholder bytes are not a valid C2PA manifest.

Removal is **logical, not secure erasure**: historical metadata and attachments
remain in the original prefix. Page/object-level associations are not edited.

Generic hashing/exclusion APIs remain byte-range utilities, **not PDF C2PA
validation**. PDF-specific revision/signature hashing rules are not implemented.

## Benchmark

Run the synthetic payload-size benchmark with:

```sh
cargo bench --no-default-features --features pdf --bench pdf
```

It measures parsing, planning an edit and writing it, each separately from metadata
materialization.
