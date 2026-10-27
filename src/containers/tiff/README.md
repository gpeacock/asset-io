# TIFF and DNG

The handler supports classic TIFF and BigTIFF in both byte orders, including
multi-page TIFF, DNG SubIFDs, EXIF, GPS, and interoperability directory links.
Enable the `tiff` feature. The declared formats are `tif`, `tiff`, and `dng`;
DNG is identified by its `DNGVersion` tag when parsing by content.

## Metadata and writes

XMP is tag 700 with type BYTE (1), in the first IFD. The C2PA manifest store is
tag 52545 (`0xcd41`) with type UNDEFINED (7). Reading prefers the last IFD's
manifest and falls back to the first IFD for older files.

Writes follow c2pa-rs's active append-only strategy: existing image data,
SubIFDs, private tags, MakerNotes, and DNG BigTable data remain at their original
offsets. Changed main IFDs are appended and the header's first-IFD pointer is
updated. A single-page image carries the manifest tag in its first IFD; a
multi-page image carries it in a dedicated final IFD. Out-of-line manifests are
written last. Same-size manifest replacement patches only its bytes, so filling
a signing placeholder changes neither the IFDs nor any other byte.

Removal unlinks tags; it does not erase historical payloads or old directories.
Changing metadata can therefore grow the file. A no-op write is byte-identical.
Inline TIFF values are supported, including small metadata placeholders.

```rust,no_run
use asset_io::{containers::tiff::{layout, parse}, NullSink, Updates};

let mut src = std::fs::File::open("image.dng")?;
let structure = parse(&mut src)?;
if let Some(entry) = layout(&structure)?.manifest_entry() {
    println!("manifest {:?}, count {:?}", entry.value_range(), entry.count_range());
}
let mut out = std::fs::File::create("out.dng")?;
structure.write(&mut src, &mut out, &Updates::new().set_xmp(b"<xmp/>".to_vec()), &mut NullSink)?;
# Ok::<(), asset_io::Error>(())
```

`layout` exposes reachable directories and native tag, type, count, entry,
value, and count-field ranges without more I/O. Image strip/tile/JPEG ranges
are media-data segments; IFDs, metadata, and gaps form a gapless physical tiling.
Only the selected manifest and first-IFD XMP are logical metadata items.
Unknown field types are preserved without guessing their value geometry.
Malformed bounds, cyclic links, duplicate tags, and metadata that aliases other
fields or known image data are errors. Directory, nesting, and entry limits
bound work on untrusted streams. This handler does not decode pixels, interpret
manifest contents, sign assets, or claim support for proprietary ARW/NEF writes.

## Primary reference

This port follows the active `clone_c2pa_mode`, `write_adjusted_ifd`,
`get_cai_data`, and `get_xmp_data` paths in
[c2pa-rs `tiff_io.rs` at fea8d1c5ac4f37cc2e258dc65a045ddb47c5b17b](https://github.com/contentauth/c2pa-rs/blob/fea8d1c5ac4f37cc2e258dc65a045ddb47c5b17b/sdk/src/asset_handlers/tiff_io.rs).
It does not port the unused full-file cloner. Unlike that seek-based writer, the
handler describes changes as a streaming write plan, clones the main IFD chain
on structural edits, and preserves correct inline-value geometry.
