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

//! Driving parsing, planning and writing from tokio's multi-threaded runtime with
//! async file I/O. It compiles only if tasks, requests and executions are `Send`,
//! which `tokio::spawn` needs.

#![cfg(all(feature = "jpeg", feature = "png", feature = "bmff", feature = "riff"))]

use asset_io::{
    Action, Chunk, HandlerRegistry, NullSink, Progress, ReadRequest, Result, Structure, Task,
    Updates,
};
use std::io::{Cursor, SeekFrom};
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

fn fixture(name: &str) -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p.exists().then_some(p)
}

async fn serve(file: &mut File, mut req: ReadRequest<'_>) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(req.offset())).await?;
    let window = req.window_len() as u64;
    let buf = req.buffer();
    buf.clear();
    (&mut *file).take(window).read_to_end(buf).await?;
    Ok(())
}

async fn drive<T>(mut task: Task<'_, T>, file: &mut File) -> Result<T> {
    loop {
        match task.resume()? {
            Progress::Read(req) => serve(file, req).await?,
            Progress::Done(v) => return Ok(v),
            _ => unreachable!(),
        }
    }
}

/// Parse `src`, plan a write with `updates`, run it into `dest`, and return the
/// output's layout and the bytes the sink saw.
async fn write_file(
    src_path: PathBuf,
    dest: PathBuf,
    updates: Updates,
) -> Result<(Structure, Vec<u8>)> {
    let registry = HandlerRegistry::builtin();
    let mut src = File::open(&src_path).await?;
    let total = src.metadata().await?.len();
    let structure = drive(registry.parse_detect_task(total), &mut src).await?;
    let plan = drive(structure.write_plan_task(&updates), &mut src).await?;

    let mut out = File::create(&dest).await?;
    let mut seen = Vec::new();
    let mut sink = |c: Chunk<'_>| {
        seen.extend_from_slice(c.data);
        Ok(())
    };
    let mut run = plan.start();
    loop {
        match run.step(&mut sink)? {
            Action::Read { offset, buf } => {
                src.seek(SeekFrom::Start(offset)).await?;
                src.read_exact(buf).await?;
            }
            Action::Write(bytes) => out.write_all(bytes).await?,
            Action::Done => break,
            _ => unreachable!(),
        }
    }
    out.flush().await?;
    Ok((plan.into_structure(), seen))
}

fn updates() -> Vec<Updates> {
    let jumbf = {
        let mut v = 5000u32.to_be_bytes().to_vec();
        v.extend(b"jumb");
        v.extend(vec![3u8; 4992]);
        v
    };
    vec![
        Updates::new().set_xmp(b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>".to_vec()),
        Updates::new().set_jumbf(jumbf),
        Updates::new().remove_xmp().remove_jumbf(),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawned_tasks_write_what_the_blocking_api_writes() {
    let dir = tempfile::tempdir().unwrap();
    let reg = HandlerRegistry::builtin();
    let mut handles = Vec::new();
    let mut n = 0;
    for name in [
        "P1000708.jpg",
        "GreenCat.png",
        "sample1.heic",
        "sample1.m4a",
        "sample1.webp",
        "sample1.wav",
    ] {
        let Some(path) = fixture(name) else { continue };
        for (j, u) in updates().into_iter().enumerate() {
            let dest: PathBuf = dir.path().join(format!("{j}-{name}"));
            // What the blocking API writes, for comparison.
            let bytes = std::fs::read(&path).unwrap();
            let s = reg.parse_detect(&mut Cursor::new(&bytes)).unwrap();
            let mut want = Cursor::new(Vec::new());
            let want_layout = s
                .write(&mut Cursor::new(&bytes), &mut want, &u, &mut NullSink)
                .unwrap_or_else(|e| panic!("{name} updates {j}: {e}"));
            // The same, from a spawned task with async I/O, many at once.
            let handle = tokio::spawn(write_file(path.clone(), dest.clone(), u));
            handles.push((handle, dest, want.into_inner(), want_layout.total_size()));
            n += 1;
        }
    }
    assert!(n > 0, "no fixtures found");
    for (handle, dest, want, size) in handles {
        let (layout, seen) = handle
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{}: {e}", dest.display()));
        let got = tokio::fs::read(Path::new(&dest)).await.unwrap();
        assert_eq!(layout.total_size(), size);
        assert!(got == want, "{}: bytes differ", dest.display());
        assert!(seen == want, "{}: sink bytes differ", dest.display());
    }
}
