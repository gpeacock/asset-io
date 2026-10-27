# ISO BMFF

The BMFF handler reads and writes ISO Base Media File Format streams, including
MP4, HEIF, AVIF, and fragmented media.

## Fragmented ISO BMFF

The handler supports both a single file containing initialization boxes and
multiple movie fragments, and separate DASH initialization and media streams.
It declares `m4s` (`video/iso.segment`), with `cmfv` and `cmfa` aliases.
Detection recognizes `styp`, `moof`, `sidx`, `emsg`, and `prft` prefixes as well as
`ftyp`. Each stream is parsed and written independently; the handler does not
fetch URLs, parse MPDs, join streams, hash, or sign them.

```rust,no_run
use asset_io::containers::bmff::{fragment_layout, parse};

let mut src = std::fs::File::open("segment.m4s")?;
let structure = parse(&mut src)?;
let layout = fragment_layout(&structure)?;
for fragment in layout.fragments() {
    println!("{:?}: moof {:?}", fragment.range(), fragment.moof());
}
# Ok::<(), asset_io::Error>(())
```

`fragment_layout` needs no I/O and works on parsed, planned, and written
structures. Each fragment starts at a `moof` and extends to the next `moof` or
EOF, including auxiliary metadata and the final random-access index. The prefix
is everything before the first `moof`, or the whole stream if there is none.
A prefix is not necessarily initialization data: an external media segment can
start with `styp` and `sidx`. A DASH segment can contain multiple movie fragments;
callers hashing an external segment should use its whole stream when required,
not assume it is exactly one fragment range.

Metadata is inserted after `ftyp` or `styp`, or before the first box of a media
stream without either. Writes relocate absolute offsets, signed `trun` data
offsets, fragment-relative `saio` entries, and `sidx` first offsets and reference
sizes. Relative offsets stay unchanged when base and target move together.
Edits requiring resolution of inherited track data bases or implicit track-run
locations, and resizing subsegments described by `ssix` range tables, return
`UnsupportedFeature` rather than guessing. These layouts can still be read and
copied unchanged.
