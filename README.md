# asset-io

Streaming, container-agnostic access to the structure and metadata of media files.

A parsed file is a `Structure`: a gapless tiling of `Segment`s (header, payload, trailer) plus `Entity`s that group the parts of logical items such as XMP and JUMBF. It holds geometry only. It does not hash, sign or interpret C2PA; callers decide what each byte range means.

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

`Structure::plan(src, &updates)` returns the layout `write` would produce, without writing: where each entity would land and the output size.

`Structure::store(src)` reads the JUMBF store as a reader sees it. For ISO BMFF that combines an original store and its update; `containers::jumbf::{addition_to, extend}` convert between the two forms.

`HandlerRegistry::compose(format, kind, data)` returns the container's own encoding of an item (JPEG APP11 segments, a PNG `caBX` chunk, a BMFF C2PA box), for callers that insert it into an asset themselves. Callers that use `write` don't need it.

`HandlerRegistry::media_type_for("heic")` gives `image/heic`, from the handler that serves the format.

## Multi-part assets

`Structure::subassets()` lists the parts of an asset: contiguous, in file order, covering every byte, each with its absolute `range()`, media type, segments, and a `relative(offset)` helper for part-relative hash ranges. A file that embeds nothing else has one part. A JPEG that holds several images (Multi-Picture Format, Ultra HDR) has one per image, plus one for any data after the last image; their markers are listed in order, so a box hash can cover them as one asset, or each part can be hashed on its own for a C2PA multi-asset hash. asset-io reports the geometry; the caller decides how to hash it.

Edits to a multi-picture JPEG add metadata to the first image and shift the sizes and offsets recorded in its MPF index to match. If the index disagrees with the images, the edit fails instead of writing a stale index.

## Zero-copy

With the bytes in memory, `Structure::slice`, `slice_entity` and `read_slice` borrow instead of copying. The `memory-mapped` feature adds `MappedFile` for that. Mapping is `unsafe`: the file must not change while mapped, so don't map files untrusted parties can write.

## Other modules

- `MiniXmp` (feature `xmp`): minimal XMP read/modify.
- `Structure::exif_info` (feature `exif`): basic EXIF fields, for containers that report EXIF entities: JPEG, PNG, WebP, and HEIF/AVIF (the `Exif` item).
- `Structure::embedded_thumbnail`: a thumbnail the container stores as its own element (a HEIF `thmb` item, entity kind `Thumbnail`; its data is the coded item, a JPEG only when the item type is `jpeg`), or with the `exif` feature the JPEG thumbnail inside EXIF data.

Run `cargo run --example inspect --features all-formats,xmp,exif -- <file>` to print a file's structure.

## Safety

Lengths are checked, segment counts and nesting are capped, and malformed input returns an error. Fuzz targets are in `fuzz/` (see `FUZZING_SETUP.md`).

## License

MIT OR Apache-2.0
