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
//! Run: `cargo run --example inspect --features all-formats,xmp,exif -- <file> [--embedded]`

use asset_io::{HandlerRegistry, Item, Name, Role, ScanLimits, TransferEncoding};
use std::{env, fs::File};

fn main() -> asset_io::Result<()> {
    let Some(path) = env::args().nth(1) else {
        eprintln!("Usage: inspect <file> [--embedded]");
        std::process::exit(1);
    };

    let mut src = File::open(&path)?;
    let registry = HandlerRegistry::builtin();
    let structure = registry.parse_detect(&mut src)?;

    println!("File: {path}");
    println!("  Container: {}", structure.container_id());
    println!("  Media type: {}", structure.media_type());
    println!("  Size: {} bytes", structure.total_size());
    println!("  Segments: {}", structure.segments().len());

    println!("\nItems:");
    for e in structure.items() {
        println!("  {}", describe(e));
    }

    if let Some(info) = structure.exif_info(&mut src)? {
        println!("\nEXIF: {info}");
    }
    if let Some(t) = structure.embedded_thumbnail(&mut src)? {
        println!(
            "Thumbnail: {}, {} bytes",
            t.media_type().unwrap_or("unknown type"),
            t.data.len()
        );
    }

    if env::args().skip(2).any(|arg| arg == "--embedded") {
        let assets = structure.scan_embedded(&mut src, &registry, ScanLimits::default())?;
        for (i, asset) in assets.iter().enumerate() {
            let parsed = match &asset.structure {
                Ok(s) => format!("{} {}", s.container_id(), s.media_type()),
                Err(error) => format!("not parsed: {error}"),
            };
            println!(
                "\nEmbedded [{i}]: depth {}, parent {:?}, {parsed}",
                asset.depth, asset.parent
            );
            println!("  from {}", describe(&asset.item));
            if let Ok(s) = &asset.structure {
                for e in s.items() {
                    println!("    {}", describe(e));
                }
            }
        }
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
            seg.range().offset(),
            seg.range().len(),
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

/// One line with every attribute of an item: never its content.
fn describe(e: &Item) -> String {
    let mut out = format!(
        "{} ({}) encoding {}",
        e.kind(),
        e.kind().category().as_str(),
        e.encoding()
    );
    match e.transfer() {
        TransferEncoding::Zlib {
            png_predictor_columns: Some(c),
        } => out += &format!(" transfer zlib (predictor columns {c})"),
        t => out += &format!(" transfer {t}"),
    }
    if let Some(l) = e.label() {
        out += &format!(" label {l}");
    }
    if let Some(m) = e.media_type() {
        out += &format!(" media type {m}");
    }
    if let Some(t) = e.jumbf_type() {
        let hex: String = t.iter().map(|b| format!("{b:02x}")).collect();
        out += &format!(" jumbf type {hex}");
    }
    match e.native_type() {
        Some(Name::FourCC(cc)) => out += &format!(" type '{}'", String::from_utf8_lossy(&cc)),
        Some(t) => out += &format!(" type {t}"),
        None => {}
    }
    if let Some(id) = e.native_id() {
        out += &format!(" id {id}");
    }
    if let Some(u) = e.uri() {
        out += &format!(" uri {u}");
    }
    for r in e.relations() {
        out += &format!(
            " {} ({}) -> {}",
            r.kind().as_str(),
            String::from_utf8_lossy(&r.native()),
            r.target()
        );
    }
    out + &format!(": {} bytes in {} extent(s)", e.len(), e.extent_count())
}
