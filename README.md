# asset-io

Streaming, container-agnostic access to the structure and metadata of media files.

A parsed file is a `Structure`: a gapless tiling of `Segment`s (header, payload, trailer) plus `Item`s, the logical units the format defines (XMP, JUMBF, EXIF, embedded files), each assembled from one or more `Extent`s. It holds geometry only. It does not hash, sign or interpret C2PA; callers decide what each byte range means.

## Concepts

- A **segment** is a unit of the tiling: what a writer copies, replaces or moves. Its `SegmentKind` is its dominant purpose: `Preamble`, `Metadata`, `MediaData` or `Other`. Its **role**s split it into framing (header, trailer) and payload.
- An **item** has an `ItemKind` (the specification that defines it: `Xmp`, `Jumbf`, `Exif`, `EmbeddedAsset`, `Other`), an `Encoding` (`Xml`, `Tiff`, `Jumbf`, `IsoBmff`, `Asset`, `Unknown`) and a category derived from its kind (metadata or asset). Its native identifiers are kept as they are: `native_type()` (a HEIF item type such as `jpeg` or `hvc1`), `native_id()` (a HEIF `item_ID`), `uri()`, `media_type()`, `jumbf_type()`, and a container-defined `label()` (`manifest`, `update`, `merkle`, ...).
- A **relation** is a reference from one item to another that keeps its native type (`thmb`, `auxl`, `cdsc`, `dimg`), with a common `RelationKind`.
- An item's **transfer encoding** (`TransferEncoding`: `Identity`, `Base64`, `Zlib`, `Deflate`, `Gzip`, `Brotli`, or `Unknown` with the format's own label) says how the bytes the container stores wrap the content: SVG stores a manifest as base64 text, PDF may store metadata as a zlib (`FlateDecode`) stream, PNG may hold compressed XMP in `iTXt`, and a HEIF `mime` item names a `content_encoding`. It is distinct from the `Encoding` of the content. The wrapping is always identified; undoing a compression is optional. `read_item` returns the stored bytes, which are what a hash covers and what `patch_exact` overwrites; `read_content` returns the content, or `UnsupportedFeature` when the decoder is not built in.
- An item's bytes are its **extents**; an item can be multi-extent (JPEG XT packets, extended XMP, HEIF `iloc` extents). "Fragment" is reserved for fragmented ISO BMFF.
- `Structure::items_in(segment)` says what a segment holds; writers decide what to do with a segment from its items, never from its kind.

asset-io exposes what each format's own specifications define, including companion specifications (EXIF/TIFF, MPF, JPEG XT, the HEIF item model), and never interprets content that is portable across formats (C2PA manifests, XMP schemas); that belongs to higher-level helpers built on this API. See the crate documentation for the full glossary.

## Upgrading from 0.1

0.2 is a redesign, not a drop-in update. The 0.1 `Asset` type and its per-format `*_io` modules are gone. Parsing, reading and writing now go through `HandlerRegistry` and `Structure`, and hashing or other byte-level work goes through a `Sink`. The `parallel`, `c2pa`, `embed-fixtures` and `test-utils` features and the `rayon`, `c2pa`, `atree`, `fs2`, `byteorder` and `log` dependencies were removed. The default features are `jpeg` and `png`; enable `bmff`, `riff`, `svg`, `pdf` and `tiff` (or `all-formats`) for the rest. The 0.1 examples (`parallel_hash`, `progress_cancel`, `c2pa_embeddable`) do not exist in 0.2.

## Containers

| Feature | Container | Read | Write |
|---------|-----------|------|-------|
| `jpeg`  | JPEG      | yes  | yes   |
| `png`   | PNG       | yes  | yes   |
| `bmff`  | ISO BMFF (MP4, HEIC, AVIF, ...) | yes | yes, with streaming chunk-offset rewrite and merkle boxes |
| `riff`  | RIFF (WebP, WAV, AVI) | yes | yes; WebP XMP maintains the `VP8X` flag |
| `svg`   | SVG       | yes  | yes; a splice, nothing re-serialized; the manifest is base64 text |
| `pdf`   | PDF 1.0–2.0 | XMP and the document's C2PA store, found through the catalog | appends an incremental revision; see `PDF.md` |
| `tiff`  | TIFF, BigTIFF, DNG | yes | yes, append-only IFD updates; see the [TIFF README](src/containers/tiff/README.md) |

Decoders for transfer encodings are optional features, in neither the defaults nor `all-formats`:

| Feature | Decodes |
|---------|---------|
| `flate` | zlib, raw deflate and gzip (pure-Rust `flate2`); enabled by `pdf` |
| `brotli` | Brotli (pure-Rust `brotli-decompressor`) |

Containers implement `ContainerHandler` and are found through `HandlerRegistry`, by format string or by content. Callers can register their own handlers.

## Use

```rust
use asset_io::{HandlerRegistry, ItemKind, NullSink, Updates};

let registry = HandlerRegistry::builtin();
let mut src = std::fs::File::open("image.jpg")?;
let structure = registry.parse_detect(&mut src)?;

if let Some(jumbf) = structure.item(ItemKind::Jumbf)? {
    let bytes = structure.read_item(&mut src, jumbf)?;
}
for item in structure.items() {
    println!("{} ({}): {} bytes", item.kind(), item.encoding(), item.len());
}

let mut out = std::fs::File::create("out.jpg")?; // BMFF also needs Read
let updates = Updates::new().set_xmp(xmp_bytes).remove_jumbf();
structure.write(&mut src, &mut out, &updates, &mut NullSink)?;
```

## Sinks

`write(src, out, updates, sink)` and `read(src, sink)` both end with a `Sink`. It receives every byte, in order, tagged with its segment and role, so a caller can hash while reading or writing. Runs the sink doesn't `want` are not read.

## Updates

`set_jumbf(store).as_update()` marks the JUMBF as an update to the existing store. ISO BMFF uses it to keep the original manifests untouched and store only the addition. Other containers ignore it.

`Structure::patch_exact` fills a placeholder in place; the replacement must be the same length.

`set_jumbf` replaces the JUMBF of the same type (the type in its description box) and leaves JUMBF of other types alone; `remove_jumbf_of(type)` removes one type, and `remove_jumbf()` removes all. This applies to JPEG, which carries JUMBF of any type. `Item::jumbf_type()` gives each item's type. Generic ISO BMFF boxes that JPEG XT carries in APP11 (Apple writes several) are not JUMBF: they are `Other` items encoded as `IsoBmff`, and JUMBF updates leave them alone. PNG, RIFF and ISO BMFF reserve an element for the manifest store, and the type does not select anything there.

`Structure::plan(src, &updates)` returns the structure `write` would produce, without writing: where each item would land and the output size.

The `Updates` API is deliberately specific to XMP and JUMBF, whose placement each container knows. Replacing, removing and inserting any item by its identity and encoding is the planned direction. In ISO BMFF, XMP is written as the XMP `uuid` box; HEIF items, XMP items included, are carried through a write unchanged.

`Structure::store(src)` reads the JUMBF store as a reader sees it. For ISO BMFF that combines an original store and its update; `jumbf::{addition_to, extend}` convert between the two forms.

`HandlerRegistry::compose(format, kind, data)` returns the container's own encoding of an item (JPEG APP11 segments, a PNG `caBX` chunk, a BMFF C2PA box), for callers that insert it into an asset themselves. Callers that use `write` don't need it.

Each handler has an `id()` (`"jpeg"`, `"png"`, `"bmff"`, `"riff"`, `"svg"`, `"pdf"`, `"tiff"`) and declares the formats it serves as `FormatSpec`s: a media type, a preferred extension, and any other accepted media types and extensions. `HandlerRegistry::media_type_for("heic")` gives `image/heic` and `extension_for("image/jpeg")` gives `jpg`; aliases resolve to the preferred values (`image/x-webp` gives `image/webp`).

## Sub-assets

`Structure::subassets()` lists the sub-assets of an asset (the "parts" of a C2PA multi-asset hash): contiguous, in file order, covering every byte, each with its absolute `range()`, media type, segments, and a `relative(offset)` helper for sub-asset-relative hash ranges. A file that embeds nothing else has one. A JPEG that holds several images (Multi-Picture Format, Ultra HDR) has one per image, plus one for any data after the last image; their markers are listed in order, so each can be hashed on its own for a C2PA multi-asset hash. asset-io reports the geometry; the caller decides how to hash it.

`Structure::trailing()` is the range of bytes beyond the end of the container's own structure: data after the last chunk, or everything after the first image of a multi-image JPEG. A box hash of the whole asset lists the structure's boxes and then these bytes as one last box named `AFTER_BOX` (`c2pa.after`, C2PA 18.7.2); the hash of a single sub-asset never includes it. JPEG and PNG report it; the other containers do not yet, so bytes after their last box are not covered by a box hash.

Edits to a multi-picture JPEG add metadata to the first image and shift the sizes and offsets recorded in its MPF index to match. The edit fails instead of writing a stale or ambiguous index: the index must be well formed, appear once, and record the offset of every image and the size of every image but the first exactly. The first image's size may be recorded short of the image, as in Pixel Ultra HDR files, and is corrected on an edit. Parsing and copying unchanged never consult it. Bytes after an image that start like another image but do not parse as one are trailing data.

## Writing a handler

A handler implements `ContainerHandler`:

Handlers use `Io` and `BoxFuture` for asynchronous reads, without constructing or
driving a `Task`. The shared reader types live in `src/io.rs`; task execution lives
in `src/task.rs`. Both remain available through the crate-root exports.

- `parse_async(self: Arc<Self>, io: Io)` builds a `Structure` with `StructureBuilder`, reading the stream through `io` (`io.read_at(offset, &mut buf).await?`, `io.reader(at)` for a run of fields, `io.fetch(..)` for a borrowed slice). Everything but the reads is ordinary code. Return `Box::pin(async move { ... })`.
- `write_plan_async(&self, structure, io, updates)` describes a write as a `WritePlan`: the structure of the output plus ordered `Step`s that copy source bytes or supply new ones. It reads the source only for what it must know (`io`), and does no writing. A handler that cannot write leaves the default. Decide what the output holds from `Updates` (`xmp()`, `jumbf()`, ...; a `MetadataSlot` says whether each existing XMP or JUMBF item is kept, replaced or dropped, and what is left to insert), then describe it in order with a `PlanBuilder`: `copy_segment(id)` for a source segment kept, `put_segment(kind, name, header, payload, trailer)` for a new one, `item(ItemInfo, &[id])` for the item it holds, `carry_items(|item| ..)` for the items of the kept segments, and `finish()`. Decide what a source segment holds from `Structure::items_in(id)`, not from its kind. The builder works out sizes, offsets and the output structure. When a segment is part copied and part changed, `shape_like`/`shape` add it and `copy`, `put` and `relocate` supply its bytes.

The blocking `parse`, `write_plan` and `write` are provided: they run those methods against a `Read + Seek`. The default `write` runs the plan with `WritePlan::execute`, which coalesces reads, feeds the sink and checks the bytes against the structure and role. `plan::validate` checks a plan without any I/O, for a handler's tests. A container with absolute file offsets (ISO BMFF chunk tables) uses `Step::Relocate`, which copies a table of offsets and rewrites them from an `OffsetMap`. Read what to change from `Updates` (`xmp()`, `jumbf()`, ...). Options only one container has live outside `Updates`: attach them with `Updates::set_extension` and read them with `Updates::extension` (ISO BMFF's merkle boxes are `containers::bmff::BmffUpdates`). `tests/external_handler.rs` is a complete handler written with only the public API. Test it with `testing::Conformance::new(handler).check(name, &sample_bytes)`: it checks the promises every handler makes (the structure tiles the stream, a no-op write is byte-identical, plan, write and stepper agree, the sink sees every byte once and in order, set and remove read back, damaged input never panics), so a handler's own tests cover only what is particular to its format.

Register a handler with `HandlerRegistry::with_handler`; the last handler registered for a format wins. Enums and structs a handler touches are `#[non_exhaustive]`, so new variants and fields are not breaking changes.

## Running with your own I/O

Parsing and planning are `async` code that does no I/O: a `Task` runs it, and when it needs bytes it does not hold (`Io` keeps a 4 KiB window, so a run of tiny reads is one request) `Task::resume` returns a `ReadRequest` with the offset, how much, and the buffer to fill. Fill it however you like (blocking, `async`, from a buffer you already have) and resume. No runtime is involved, and a task is `Send`. Get one from `HandlerRegistry::parse_task(format, total)` or `parse_detect_task(total)` for a parse, or `Structure::write_plan_task(&updates)` for a plan.

A `WritePlan` runs the same way: `WritePlan::start` gives an `Execution` whose `step` returns `Action::Read { offset, buf }`, `Action::Write(bytes)` or `Action::Done`, and does the sink calls, the checking and the read coalescing itself, so every way of running a plan behaves like `execute`. The blocking calls (`HandlerRegistry::parse`, `Structure::write`) are these drivers over a `Read + Seek` and a `Write`.

`examples/tokio_client.rs` drives parsing, planning and writing with `tokio::fs`, one spawned task per file, hashing the output as it is written; `tests/tokio_driver.rs` checks that what it writes matches the blocking API byte for byte.

Its `edit` command reports wall-clock times for parsing, planning, writing with SHA-256, and the total edit. Writing includes output creation and flushing, but not an explicit disk sync. The total excludes loading the replacement XMP, task spawning, and compilation.

## Zero-copy

With the bytes in memory, `Structure::slice`, `slice_item` and `read_slice` borrow instead of copying. The `memory-mapped` feature adds `MappedFile` for that. Mapping is `unsafe`: the file must not change while mapped, so don't map files untrusted parties can write.

## Other modules

- `MiniXmp` (feature `xmp`): minimal XMP read/modify.
- `Structure::exif_info` (feature `exif`): basic EXIF fields, for containers that report EXIF items: JPEG, PNG, WebP, and HEIF/AVIF (the `Exif` item).
- `Structure::xmp`: the XMP packet. For HEIF it is the XMP `uuid` box if there is one, else the XMP item that describes (`cdsc`) the primary item, else the only XMP item.
- `Structure::embedded_thumbnail`: an item with a `Thumbnail` relation (a HEIF `thmb` reference; its data is the coded item unless the item is a `jpeg` or a `mime` item), or with the `exif` feature the JPEG thumbnail inside EXIF data. `Thumbnail::media_type()` says what the data is, or `None` when the container cannot tell (a coded `hvc1`/`av01` item, which is an `Other` item, not an embedded asset); `Item::media_type()` and `SubAsset::media_type()` report media types the same way.

Run `cargo run --example inspect --features all-formats,xmp,exif -- <file>` to print a file's structure and items.

### Embedded files

`Structure::embedded_assets()` lists the `EmbeddedAsset` items: those the format
declares to be files of a media type. In HEIF these are `jpeg` items and `mime`
items with a valid content type other than XMP. A `uri ` item declares no media
type, so it is an `Other` item of unknown encoding, keeping the URI that
identifies its purpose in `Item::uri()`; a URI is never fetched. Apple's
reference image (`tag:apple.com,2025:photo:metadata:provenance:processedimage`)
is such an item. So are `mime` items without a valid content type, and coded HEIF
images such as an `hvc1` thumbnail, which also keep their thumbnail relation. EXIF-internal thumbnails remain available
through `embedded_thumbnail`; MPF images remain `subassets`.

Parsing stays shallow. Opt in to recursive scanning with the same registry,
including any custom handlers:

```rust,no_run
use asset_io::{Encoding, HandlerRegistry, ScanLimits};

# fn main() -> asset_io::Result<()> {
let registry = HandlerRegistry::builtin();
let mut src = std::fs::File::open("photo.heic")?;
let structure = registry.parse_detect(&mut src)?;
let embedded = structure.scan_embedded(&mut src, &registry, ScanLimits::default())?;
for asset in &embedded {
    match &asset.structure {
        Ok(child) => {
            let mut reader = asset.reader(&mut src)?;
            for item in child.items().filter(|i| i.encoding() == Encoding::IsoBmff) {
                let bytes = child.read_item(&mut reader, item)?;
                // The application interprets these bytes.
                println!("{} bytes of box content", bytes.len());
            }
        }
        Err(error) => eprintln!("Embedded item {:?}: {error}", asset.item.uri()),
    }
}
# Ok(())
# }
```

Results are depth-first, with parent indices and nesting depth. Child offsets
are relative to the child's stream; `asset.reader()` maps them back to the
original source, even through multi-extent and nested items, without copying
image data. Every embedded asset is reported; one that is unsupported or malformed retains
its error and raw byte ranges, so siblings remain inspectable. Opaque items
(`Other`, unknown encoding, stored as is) are reported only when the registry
recognises their leading bytes; the rest are skipped and count against no
limit. Apple's processed image is found this way as a JPEG, while its two
styles `uri ` items are not reported. Budget overruns fail the scan explicitly:
defaults are depth 4, 64 assets and 256 MiB of cumulative embedded lengths.
Depth is always capped at 32. Scanning follows declared items, not arbitrary
file headers hidden in opaque payloads, and never decodes pixels.

JPEG exposes JUMBF superboxes carried in JPEG XT APP11 packets as `Jumbf` items,
and generic `uuid` boxes carried the same way as `Other` items encoded as
`IsoBmff`. Transport headers are removed and packet bodies reassembled. A UUID box
is not a JUMBF superbox, so its `jumbf_type()` is `None`; interpreting its UUID
and payload belongs to the application.

Add `--embedded` to the `inspect` command to print nested metadata and raw box
prefixes.

## Safety

Lengths are checked, segment counts and nesting are capped, and malformed input returns an error. Fuzz targets are in `fuzz/` (see `FUZZING_SETUP.md`).

## Application C bindings

The standalone [`asset-io-c`](asset-io-c/README.md) package provides a checked-in C
header and shared/static libraries for C++, Python and Swift applications, with
memory/read-at parsing, structure inspection, metadata updates and tagged streaming
output. It supports minimal, JPEG/PNG-default and all-formats builds independently
of this Rust crate. It does not define a dynamically loaded handler/plugin ABI.

## License

MIT OR Apache-2.0
