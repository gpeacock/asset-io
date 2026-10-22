# asset-io

Streaming, container-agnostic access to the structure and metadata of media files.

A parsed file is a `Structure`: a gapless tiling of `Segment`s (header, payload, trailer) plus `Entity`s that group the parts of logical items such as XMP and JUMBF. It holds geometry only. It does not hash, sign or interpret C2PA; callers decide what each byte range means.

## Upgrading from 0.1

0.2 is a redesign, not a drop-in update. The 0.1 `Asset` type and its per-format `*_io` modules are gone. Parsing, reading and writing now go through `HandlerRegistry` and `Structure`, and hashing or other byte-level work goes through a `Sink`. The `parallel`, `c2pa`, `embed-fixtures` and `test-utils` features and the `rayon`, `c2pa`, `atree`, `fs2`, `byteorder` and `log` dependencies were removed. The default features are `jpeg` and `png`; enable `bmff` and `riff` (or `all-formats`) for the rest. The 0.1 examples (`parallel_hash`, `progress_cancel`, `c2pa_embeddable`) do not exist in 0.2.

## Containers

| Feature | Container | Read | Write |
|---------|-----------|------|-------|
| `jpeg`  | JPEG      | yes  | yes   |
| `png`   | PNG       | yes  | yes   |
| `bmff`  | ISO BMFF (MP4, HEIC, AVIF, ...) | yes | yes, with streaming chunk-offset rewrite and merkle boxes |
| `riff`  | RIFF (WebP, WAV, AVI) | yes | yes; WebP XMP maintains the `VP8X` flag |

Containers implement `ContainerHandler` and are found through `HandlerRegistry`, by format string or by content. Callers can register their own handlers.

## Use

```rust
use asset_io::{HandlerRegistry, NullSink, SegmentKind, Updates};

let registry = HandlerRegistry::builtin();
let mut src = std::fs::File::open("image.jpg")?;
let structure = registry.parse_detect(&mut src)?;

if let Some(jumbf) = structure.entity(SegmentKind::Jumbf)? {
    let bytes = structure.read_entity(&mut src, jumbf)?;
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

`set_jumbf` replaces the JUMBF of the same type (the type in its description box) and leaves JUMBF of other types alone; `remove_jumbf_of(type)` removes one type, and `remove_jumbf()` removes all. This applies to JPEG, which carries JUMBF of any type. `Entity::jumbf_type()` gives each entity's type. PNG, RIFF and ISO BMFF reserve an element for the manifest store, and the type does not select anything there.

`Structure::plan(src, &updates)` returns the structure `write` would produce, without writing: where each entity would land and the output size.

`Structure::store(src)` reads the JUMBF store as a reader sees it. For ISO BMFF that combines an original store and its update; `containers::jumbf::{addition_to, extend}` convert between the two forms.

`HandlerRegistry::compose(format, kind, data)` returns the container's own encoding of an item (JPEG APP11 segments, a PNG `caBX` chunk, a BMFF C2PA box), for callers that insert it into an asset themselves. Callers that use `write` don't need it.

Each handler has an `id()` (`"jpeg"`, `"png"`, `"bmff"`, `"riff"`) and declares the formats it serves as `FormatSpec`s: a media type, a preferred extension, and any other accepted media types and extensions. `HandlerRegistry::media_type_for("heic")` gives `image/heic` and `extension_for("image/jpeg")` gives `jpg`; aliases resolve to the preferred values (`image/x-webp` gives `image/webp`).

## Multi-part assets

`Structure::subassets()` lists the parts of an asset: contiguous, in file order, covering every byte, each with its absolute `range()`, media type, segments, and a `relative(offset)` helper for part-relative hash ranges. A file that embeds nothing else has one part. A JPEG that holds several images (Multi-Picture Format, Ultra HDR) has one per image, plus one for any data after the last image; their markers are listed in order, so each part can be hashed on its own for a C2PA multi-asset hash. asset-io reports the geometry; the caller decides how to hash it.

`Structure::trailing()` is the range of bytes beyond the end of the container's own structure: data after the last chunk, or everything after the first image of a multi-image JPEG. A box hash of the whole asset lists the structure's boxes and then these bytes as one last box named `AFTER_BOX` (`c2pa.after`, C2PA 18.7.2); the hash of a single part never includes it. JPEG and PNG report it; the other containers do not yet, so bytes after their last box are not covered by a box hash.

Edits to a multi-picture JPEG add metadata to the first image and shift the sizes and offsets recorded in its MPF index to match. The edit fails instead of writing a stale or ambiguous index: the index must be well formed, appear once, and record the offset of every image and the size of every image but the first exactly. The first image's size may be recorded short of the image, as in Pixel Ultra HDR files, and is corrected on an edit. Parsing and copying unchanged never consult it. Bytes after an image that start like another image but do not parse as one are trailing data.

## Writing a handler

A handler implements `ContainerHandler`:

- `parse_async(self: Arc<Self>, io: Io)` builds a `Structure` with `StructureBuilder`, reading the stream through `io` (`io.read_at(offset, &mut buf).await?`, `io.reader(at)` for a run of fields, `io.fetch(..)` for a borrowed slice). Everything but the reads is ordinary code. Return `Box::pin(async move { ... })`.
- `write_plan_async(&self, structure, io, updates)` describes a write as a `WritePlan`, a structure plus ordered `Step`s that either copy source bytes (`Copy`) or supply new ones (`Put`). It reads the source only for what it must know (`io`), and does no writing. A handler that cannot write leaves the default.

The blocking `parse`, `write_plan` and `write` are provided: they run those methods against a `Read + Seek`. The default `write` runs the plan with `WritePlan::execute`, which coalesces reads, feeds the sink and checks the bytes against the structure and role. `plan::validate` checks a plan without any I/O, for a handler's tests. A container with absolute file offsets (ISO BMFF chunk tables) uses `Step::Relocate`, which copies a table of offsets and rewrites them from an `OffsetMap`. Read what to change from `Updates` (`xmp()`, `jumbf()`, ...). Options only one container has live outside `Updates`: attach them with `Updates::set_extension` and read them with `Updates::extension` (ISO BMFF's merkle boxes are `containers::bmff::BmffUpdates`). `tests/external_handler.rs` is a complete handler written with only the public API.

Register a handler with `HandlerRegistry::with_handler`; the last handler registered for a format wins. Enums and structs a handler touches are `#[non_exhaustive]`, so new variants and fields are not breaking changes.

## Running with your own I/O

Parsing and planning are `async` code that does no I/O: a `Task` runs it, and when it needs bytes it does not hold (`Io` keeps a 4 KiB window, so a run of tiny reads is one request) `Task::resume` returns a `ReadRequest` with the offset, how much, and the buffer to fill. Fill it however you like (blocking, `async`, from a buffer you already have) and resume. No runtime is involved, and a task is `Send`. Get one from `HandlerRegistry::parse_task(format, total)` or `parse_detect_task(total)` for a parse, or `Structure::write_plan_task(&updates)` for a plan.

A `WritePlan` runs the same way: `WritePlan::start` gives an `Execution` whose `step` returns `Action::Read { offset, buf }`, `Action::Write(bytes)` or `Action::Done`, and does the sink calls, the checking and the read coalescing itself, so every way of running a plan behaves like `execute`. The blocking calls (`HandlerRegistry::parse`, `Structure::write`) are these drivers over a `Read + Seek` and a `Write`.

`examples/tokio_client.rs` drives parsing, planning and writing with `tokio::fs`, one spawned task per file, hashing the output as it is written; `tests/tokio_driver.rs` checks that what it writes matches the blocking API byte for byte.

## Zero-copy

With the bytes in memory, `Structure::slice`, `slice_entity` and `read_slice` borrow instead of copying. The `memory-mapped` feature adds `MappedFile` for that. Mapping is `unsafe`: the file must not change while mapped, so don't map files untrusted parties can write.

## Other modules

- `MiniXmp` (feature `xmp`): minimal XMP read/modify.
- `Structure::exif_info` (feature `exif`): basic EXIF fields, for containers that report EXIF entities: JPEG, PNG, WebP, and HEIF/AVIF (the `Exif` item).
- `Structure::embedded_thumbnail`: a thumbnail the container stores as its own element (a HEIF `thmb` item, entity kind `Thumbnail`; its data is the coded item unless the item is a `jpeg` or a `mime` item), or with the `exif` feature the JPEG thumbnail inside EXIF data. `Thumbnail::media_type()` says what the data is, or `None` when the container cannot tell (a coded `hvc1`/`av01` item); `Entity::media_type()` and `SubAsset::media_type()` report media types the same way.

Run `cargo run --example inspect --features all-formats,xmp,exif -- <file>` to print a file's structure.

## Safety

Lengths are checked, segment counts and nesting are capped, and malformed input returns an error. Fuzz targets are in `fuzz/` (see `FUZZING_SETUP.md`).

## License

MIT OR Apache-2.0
