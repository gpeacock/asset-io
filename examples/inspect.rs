// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

// Unless required by applicable law or agreed to in writing,
// this software is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR REPRESENTATIONS OF ANY KIND, either express or
// implied. See the LICENSE-MIT and LICENSE-APACHE files for the
// specific language governing permissions and limitations under
// each license.

//! Parse a media file and print its structure and metadata.
//!
//! Run: `cargo run --example inspect --features all-formats,xmp,exif -- <file>`

use asset_io::{HandlerRegistry, Role, SegmentKind};
use std::{env, fs::File};

fn main() -> asset_io::Result<()> {
    let Some(path) = env::args().nth(1) else {
        eprintln!("Usage: inspect <file>");
        std::process::exit(1);
    };

    let mut src = File::open(&path)?;
    let structure = HandlerRegistry::builtin().parse_detect(&mut src)?;

    println!("File: {path}");
    println!("  Container: {}", structure.container_id());
    println!("  Media type: {}", structure.media_type());
    println!("  Size: {} bytes", structure.total_size());
    println!("  Segments: {}", structure.segments().len());

    for kind in [SegmentKind::Xmp, SegmentKind::Jumbf] {
        for e in structure.entities(kind) {
            let label = e.label().map(|l| format!(" [{l}]")).unwrap_or_default();
            println!(
                "\n{kind}{label}: {} bytes, {} part(s)",
                e.len(),
                e.part_count()
            );
            if kind == SegmentKind::Xmp {
                let data = structure.read_entity(&mut src, e)?;
                let head = &data[..data.len().min(100)];
                println!("  Preview: {}...", String::from_utf8_lossy(head));
            }
        }
    }

    if let Some(info) = structure.exif_info(&mut src)? {
        println!("\nEXIF: {info}");
        if let Some(v) = &info.software {
            println!("  Software: {v}");
        }
        if let Some(v) = info.orientation {
            println!("  Orientation: {v}");
        }
    }

    if let Some(t) = structure.embedded_thumbnail(&mut src)? {
        let dims = match (t.width, t.height) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => "unknown size".into(),
        };
        println!(
            "\nThumbnail: {}, {dims}, {} bytes",
            t.media_type().unwrap_or("unknown type"),
            t.data.len()
        );
    }

    println!("\nSegments:");
    for (i, seg) in structure.segments().iter().enumerate() {
        let name = structure
            .box_name(seg.name())
            .map(|n| n.into_owned())
            .unwrap_or_default();
        println!(
            "  [{i:4}] {name:8} {:10} offset {:10} size {:10} (header {}, payload {}, trailer {})",
            seg.kind().to_string(),
            seg.extent().offset(),
            seg.extent().len(),
            seg.header().len(),
            seg.payload().len(),
            seg.trailer().len(),
        );
    }

    // Region view: the roles a hasher would see.
    let payload: u64 = structure
        .regions()
        .filter(|r| r.role == Role::Payload)
        .map(|r| r.range.len())
        .sum();
    println!("\nPayload bytes: {payload} of {}", structure.total_size());
    Ok(())
}
