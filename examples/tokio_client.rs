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

//! Use asset-io from tokio, with async file I/O.
//!
//! asset-io does no I/O of its own when it parses a file or plans and runs a
//! write: it hands the caller a request ("these bytes, please") or an action
//! ("write these bytes"). This example answers them with `tokio::fs`, so a parse
//! or a write never blocks a runtime thread, and many files can be handled at
//! once. Everything that crosses an `.await` is `Send`, so the work can be spawned.
//!
//! Run:
//!
//! ```text
//! cargo run --example tokio_client --features all-formats -- inspect a.jpg b.png c.mp4
//! cargo run --example tokio_client --features all-formats -- edit in.jpg out.jpg new.xmp
//! ```

use asset_io::{
    Action, Chunk, HandlerRegistry, Progress, ReadRequest, Result, SegmentKind, Structure, Task,
    Updates,
};
use sha2::{Digest, Sha256};
use std::io::SeekFrom;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Answer one request of a task from an async file: seek to the offset and read
/// the window into the request's buffer.
async fn serve(file: &mut File, mut req: ReadRequest<'_>) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(req.offset())).await?;
    let window = req.window_len() as u64;
    let buf = req.buffer();
    buf.clear();
    // The request owns its buffer, so it can be held across the `.await`.
    (&mut *file).take(window).read_to_end(buf).await?;
    Ok(())
}

/// Run a task to its end, reading what it asks for from `file`.
async fn drive<T>(mut task: Task<'_, T>, file: &mut File) -> Result<T> {
    loop {
        match task.resume()? {
            Progress::Read(req) => serve(file, req).await?,
            Progress::Done(value) => return Ok(value),
            _ => unreachable!("a progress this driver does not know"),
        }
    }
}

/// Read a range of a file.
async fn read_range(file: &mut File, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Parse a file and describe it.
async fn inspect(path: String) -> Result<String> {
    let registry = HandlerRegistry::builtin();
    let mut file = File::open(&path).await?;
    let total = file.metadata().await?.len();

    // The container is found from the first bytes, which the task asks for.
    let structure = drive(registry.parse_detect_task(total), &mut file).await?;

    let mut out = format!(
        "{path}\n  {} ({}), {} bytes, {} segments\n",
        structure.media_type(),
        structure.container_id(),
        structure.total_size(),
        structure.segments().len()
    );
    for kind in [SegmentKind::Xmp, SegmentKind::Jumbf, SegmentKind::Exif] {
        for e in structure.entities(kind) {
            // The parts of an entity are plain byte ranges; read them however you like.
            let mut bytes = Vec::with_capacity(e.len() as usize);
            for part in e.parts() {
                let r = part.range();
                bytes.extend(read_range(&mut file, r.offset(), r.len()).await?);
            }
            let label = e.label().map(|l| format!(" [{l}]")).unwrap_or_default();
            out += &format!("  {kind}{label}: {} bytes\n", bytes.len());
        }
    }
    Ok(out)
}

/// Replace the XMP of `input`, writing `output`, and hash the output as it is
/// written. Returns the SHA-256 of the output.
async fn edit(input: String, output: String, xmp: Vec<u8>) -> Result<(Structure, String)> {
    let registry = HandlerRegistry::builtin();
    let mut src = File::open(&input).await?;
    let total = src.metadata().await?.len();
    let structure = drive(registry.parse_detect_task(total), &mut src).await?;

    // Plan the write: the structure of the output and the steps that produce it.
    // Planning reads a little of the source (for some formats), asked for the same way.
    let updates = Updates::new().set_xmp(xmp);
    let plan = drive(structure.write_plan_task(&updates), &mut src).await?;

    // Run the plan. Every output byte also goes to a sink, tagged with its segment
    // and role, so it can be hashed without a second pass.
    let mut out = File::create(&output).await?;
    let mut hash = Sha256::new();
    let mut sink = |chunk: Chunk<'_>| {
        hash.update(chunk.data);
        Ok(())
    };
    let mut run = plan.start();
    let mut at: Option<u64> = None;
    loop {
        match run.step(&mut sink)? {
            Action::Read { offset, buf } => {
                if at != Some(offset) {
                    src.seek(SeekFrom::Start(offset)).await?;
                }
                src.read_exact(buf).await?;
                at = Some(offset + buf.len() as u64);
            }
            Action::Write(bytes) => out.write_all(bytes).await?,
            Action::Done => break,
            _ => unreachable!("an action this driver does not know"),
        }
    }
    out.flush().await?;

    // The layout of what was written is known without parsing it again.
    let written = plan.into_structure();
    let xmp_entities = written.entities(SegmentKind::Xmp).count();
    println!(
        "  wrote {} bytes, {xmp_entities} XMP packet(s)",
        written.total_size()
    );
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok((written, digest))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inspect") if args.len() > 1 => {
            // One task per file, all running at once. Each can move between threads.
            let handles: Vec<_> = args[1..]
                .iter()
                .cloned()
                .map(|path| tokio::spawn(inspect(path)))
                .collect();
            for h in handles {
                match h.await.expect("task panicked") {
                    Ok(text) => print!("{text}"),
                    Err(e) => eprintln!("error: {e}"),
                }
            }
        }
        Some("edit") if args.len() == 4 => {
            let xmp = tokio::fs::read(&args[3]).await?;
            let (_, digest) = tokio::spawn(edit(args[1].clone(), args[2].clone(), xmp))
                .await
                .expect("task panicked")?;
            println!("  sha256 {digest}");
        }
        _ => {
            eprintln!("Usage:");
            eprintln!("  tokio_client inspect <file>...");
            eprintln!("  tokio_client edit <in> <out> <xmp-file>");
            std::process::exit(2);
        }
    }
    Ok(())
}
