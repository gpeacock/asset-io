//! Using asset-io from async code (tokio).
//!
//! Part 1 uses the ready-made `sans_io::drive_async` driver (feature `futures-io`) with tokio
//! files adapted through `tokio_util::compat`: parse, hash, and write a new file.
//!
//! Part 2 drives a machine by hand against a simulated remote source that only supports
//! "range requests" with latency (like HTTP `Range:`), showing progress reporting and
//! cancellation by dropping the machine. No `futures-io` is needed for this style: any async
//! I/O can serve `Step::Read` requests.
//!
//! Run with:
//! ```sh
//! cargo run --example async_tokio --features all-formats,futures-io -- [path]
//! ```

use asset_io::sans_io::{self, drive_async, Step};
use asset_io::{ExclusionMode, ProcessChunk, SegmentKind, Updates};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::main]
async fn main() -> asset_io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/fixtures/P1000708.jpg".to_string());

    files_with_drive_async(&path).await?;
    remote_range_requests(&path).await?;
    Ok(())
}

/// Part 1: tokio files through the `futures-io` driver.
async fn files_with_drive_async(path: &str) -> asset_io::Result<()> {
    println!("== drive_async over tokio::fs ({path})");
    let file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let mut src = file.compat();

    let structure = drive_async(sans_io::parse(len), Some(&mut src), None, None).await?;
    println!(
        "parsed {:?}: {} segments",
        structure.container,
        structure.segments.len()
    );

    // Write a copy with a JUMBF placeholder while hashing everything except the JUMBF data,
    // the usual first pass of an embed-then-sign workflow.
    let updates = Updates::new()
        .set_jumbf(vec![0u8; 4096])
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
    let out_path = std::env::temp_dir().join("asset_io_async_tokio_out");
    let mut dest = tokio::fs::File::create(&out_path).await?.compat_write();
    let mut hasher = Sha256::new();
    let written = drive_async(
        sans_io::write(&structure, &updates, true),
        Some(&mut src),
        Some(&mut dest),
        Some(&mut |chunk: &dyn ProcessChunk| {
            hasher.update(chunk.data());
            Ok(())
        }),
    )
    .await?;
    println!(
        "wrote {} bytes to {}; sha256 (excluding JUMBF) = {:x}",
        written.total_size,
        out_path.display(),
        hasher.finalize()
    );
    let _ = tokio::fs::remove_file(&out_path).await;
    Ok(())
}

/// A pretend remote object that serves byte ranges with some latency.
struct RemoteObject {
    data: Vec<u8>,
    requests: usize,
}

impl RemoteObject {
    async fn fetch(&mut self, offset: u64, buf: &mut [u8]) -> usize {
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.requests += 1;
        let start = (offset as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        n
    }
}

/// Part 2: drive machines by hand against a range-request source.
async fn remote_range_requests(path: &str) -> asset_io::Result<()> {
    println!("== hand-driven machines over simulated range requests");
    let mut remote = RemoteObject {
        data: tokio::fs::read(path).await?,
        requests: 0,
    };
    let len = remote.data.len() as u64;

    let mut parser = sans_io::parse(len);
    loop {
        match parser.step()? {
            Step::Read { offset, buf } => {
                let n = remote.fetch(offset, buf).await;
                parser.filled(n)?;
            }
            Step::Done => break,
            Step::Write { .. } | Step::Process(_) => unreachable!("parsing only reads"),
        }
    }
    let structure = parser.finish()?;
    println!("parsed with {} range requests", remote.requests);

    // Hash with progress reporting. Hashing reads are strictly forward, so this would also
    // work over a forward-only stream.
    let updates = Updates::new();
    let mut hasher = Sha256::new();
    let mut machine = sans_io::read_process(&structure, &updates);
    let mut last_pct = 0;
    loop {
        match machine.step()? {
            Step::Read { offset, buf } => {
                let n = remote.fetch(offset, buf).await;
                machine.filled(n)?;
            }
            Step::Process(chunk) => hasher.update(chunk.data()),
            Step::Done => break,
            Step::Write { .. } => unreachable!("hashing does not write"),
        }
        let p = machine.progress();
        if let Some(total) = p.total.filter(|t| *t > 0) {
            let pct = p.bytes_processed * 100 / total;
            if pct >= last_pct + 25 {
                println!("  hashed {pct}%");
                last_pct = pct;
            }
        }
    }
    machine.finish()?;
    println!(
        "sha256 = {:x} ({} range requests total)",
        hasher.finalize(),
        remote.requests
    );

    // Cancellation demo (this is expected to time out): each read below is artificially
    // slowed to 50 ms but the whole hash is only given 20 ms. When the timeout fires the
    // future, and the machine inside it, is dropped, which cancels the operation; no cleanup
    // or cancel flag is required.
    println!("== cancellation demo: 20 ms deadline, 50 ms per read (expected to time out)");
    let mut machine = sans_io::read_process(&structure, &updates);
    let slow = async {
        loop {
            match machine.step()? {
                Step::Read { offset, buf } => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let n = remote.fetch(offset, buf).await;
                    machine.filled(n)?;
                }
                Step::Done => break,
                _ => {}
            }
        }
        asset_io::Result::Ok(())
    };
    match tokio::time::timeout(Duration::from_millis(20), slow).await {
        Ok(r) => r?,
        Err(_) => println!("deadline hit as intended; hash cancelled by dropping the machine"),
    }
    Ok(())
}
