//! Progress reporting with cooperative cancel (Enter key).
//!
//! Streams through the file with [`Asset::read_with_processing`], prints throughput and
//! approximate progress on a timer, and stops with [`Error::UserCanceled`] when you press Enter.
//!
//! ```bash
//! cargo run --release --example progress_cancel --features all-formats -- /path/to/file.jpg
//! ```

use asset_io::{Asset, Error, ExclusionMode, SegmentKind, Updates};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often to print a progress line (wall clock).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);

struct Progress {
    bytes: u64,
    last_print: Instant,
}

fn main() -> asset_io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/fixtures/P1000708.jpg".to_string());

    println!("Progress + cancel demo");
    println!("File: {}", path);
    println!("Press Enter at any time to cancel.\n");

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_watch = Arc::clone(&cancel);
    std::thread::spawn(move || {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => {} // EOF: do not cancel
            Ok(_) => cancel_watch.store(true, Ordering::Relaxed),
            Err(_) => {}
        }
    });

    let mut asset = Asset::open(&path)?;
    let total_size = asset.structure().total_size.max(1);

    let updates = Updates::new()
        .with_chunk_size(256 * 1024)
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);

    let progress = Arc::new(Mutex::new(Progress {
        bytes: 0,
        last_print: Instant::now(),
    }));

    let progress_clone = Arc::clone(&progress);
    let cancel_clone = Arc::clone(&cancel);
    let run_start = Instant::now();
    let mut hasher = Sha256::new();

    let result = asset.read_with_processing(&updates, &mut |chunk| {
        if cancel_clone.load(Ordering::Relaxed) {
            return Err(Error::UserCanceled);
        }

        hasher.update(chunk);

        let mut p = progress_clone.lock().unwrap();
        p.bytes += chunk.len() as u64;
        if p.last_print.elapsed() >= PROGRESS_INTERVAL {
            let elapsed = run_start.elapsed();
            let mb = p.bytes as f64 / 1_048_576.0;
            let pct = (p.bytes as f64 / total_size as f64 * 100.0).min(100.0);
            let mb_s = if elapsed.as_secs_f64() > 0.0 {
                mb / elapsed.as_secs_f64()
            } else {
                0.0
            };
            print!(
                "\r  {:6.1}%  {:8.2} MB  {:6.2} MB/s  {:6.1}s elapsed",
                pct,
                mb,
                mb_s,
                elapsed.as_secs_f64()
            );
            let _ = std::io::stdout().flush();
            p.last_print = Instant::now();
        }

        Ok(())
    });

    println!();

    match result {
        Ok(()) => {
            let elapsed = run_start.elapsed();
            let p = progress.lock().unwrap();
            let mb = p.bytes as f64 / 1_048_576.0;
            let mb_s = if elapsed.as_secs_f64() > 0.0 {
                mb / elapsed.as_secs_f64()
            } else {
                0.0
            };
            println!(
                "Finished: {} bytes in {:.2}s ({:.2} MB/s), sha256 {:x}",
                p.bytes,
                elapsed.as_secs_f64(),
                mb_s,
                hasher.finalize()
            );
        }
        Err(Error::UserCanceled) => {
            let elapsed = run_start.elapsed();
            let p = progress.lock().unwrap();
            println!(
                "Canceled: {} bytes processed in {:.2}s (partial hash not finalized)",
                p.bytes,
                elapsed.as_secs_f64()
            );
        }
        Err(e) => return Err(e),
    }

    Ok(())
}
