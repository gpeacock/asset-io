//! Tests for the runtime-agnostic async driver (`sans_io::drive_async`).
//!
//! Results must match the blocking [`Asset`] API byte for byte and event for event.

#![cfg(all(
    feature = "futures-io",
    feature = "jpeg",
    feature = "png",
    feature = "bmff",
    feature = "riff",
    feature = "pdf"
))]

use asset_io::sans_io::{self, drive_async};
use asset_io::{Asset, ExclusionMode, ProcessChunk, SegmentKind, Updates};
use futures::executor::block_on;
use futures::io::Cursor as AsyncCursor;
use std::io::Cursor;
use std::path::PathBuf;

const FIXTURES: &[&str] = &[
    "Designer.jpeg",
    "GreenCat.png",
    "sample1.heic",
    "sample1.m4a",
    "sample1.webp",
    "sample1.wav",
    "pdf_metadata.pdf",
];

fn fixture(name: &str) -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    std::fs::read(p).unwrap()
}

fn updates() -> Updates {
    Updates::new()
        .set_jumbf(vec![7u8; 5000])
        .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly)
}

/// Processed bytes plus a compact event log, comparable across drivers.
#[derive(Default, PartialEq, Debug)]
struct Events {
    data: Vec<u8>,
    log: Vec<String>,
}

impl Events {
    fn push(&mut self, c: &dyn ProcessChunk) {
        self.data.extend_from_slice(c.data());
        self.log.push(format!(
            "{}:{:?}:{:?}:{}",
            c.data().len(),
            c.id(),
            c.large_size(),
            c.segment().is_some()
        ));
    }
}

#[test]
fn async_driver_matches_sync_api() {
    for name in FIXTURES {
        let data = fixture(name);
        let len = data.len() as u64;
        let mut asset = Asset::from_source(Cursor::new(data.clone())).unwrap();
        let updates = updates();

        block_on(async {
            let mut src = AsyncCursor::new(data.clone());

            let structure = drive_async(sans_io::parse(len), Some(&mut src), None, None)
                .await
                .unwrap();
            assert_eq!(
                format!("{:?}", structure),
                format!("{:?}", asset.structure()),
                "{name}: parse"
            );

            let xmp = drive_async(sans_io::read_xmp(&structure), Some(&mut src), None, None)
                .await
                .unwrap();
            assert_eq!(xmp, asset.xmp().unwrap(), "{name}: xmp");
            let jumbf = drive_async(sans_io::read_jumbf(&structure), Some(&mut src), None, None)
                .await
                .unwrap();
            assert_eq!(jumbf, asset.jumbf().unwrap(), "{name}: jumbf");

            // Hashing
            let mut hashed = Vec::new();
            drive_async(
                sans_io::read_process(&structure, &updates),
                Some(&mut src),
                None,
                Some(&mut |c: &dyn ProcessChunk| {
                    hashed.extend_from_slice(c.data());
                    Ok(())
                }),
            )
            .await
            .unwrap();
            let mut expected = Vec::new();
            asset
                .read_with_processing(&updates, &mut |d: &[u8]| {
                    expected.extend_from_slice(d);
                    Ok(())
                })
                .unwrap();
            assert!(hashed == expected, "{name}: read_process");

            // Write with processing
            let mut dest = AsyncCursor::new(Vec::new());
            let mut events = Events::default();
            let result = drive_async(
                sans_io::write(&structure, &updates, true),
                Some(&mut src),
                Some(&mut dest),
                Some(&mut |c: &dyn ProcessChunk| {
                    events.push(c);
                    Ok(())
                }),
            )
            .await;

            let mut sync_dest = Cursor::new(Vec::new());
            let mut sync_events = Events::default();
            let sync_result = asset.write_with_processing(
                &mut sync_dest,
                &updates,
                &mut |c: &dyn ProcessChunk| {
                    sync_events.push(c);
                    Ok(())
                },
            );
            match (result, sync_result) {
                (Ok(out), Ok(sync_out)) => {
                    assert!(
                        dest.get_ref() == sync_dest.get_ref(),
                        "{name}: written bytes"
                    );
                    assert!(events == sync_events, "{name}: write events");
                    assert_eq!(
                        format!("{:?}", out),
                        format!("{:?}", sync_out),
                        "{name}: written structure"
                    );
                }
                (Err(e), Err(sync_e)) => {
                    assert_eq!(e.to_string(), sync_e.to_string(), "{name}: write error")
                }
                (a, b) => panic!("{name}: async {:?} vs sync {:?}", a.is_ok(), b.is_ok()),
            }
        });
    }
}

#[test]
fn async_driver_reports_missing_source() {
    let err = block_on(drive_async(sans_io::parse(10), None, None, None)).unwrap_err();
    assert!(matches!(err, asset_io::Error::Protocol(_)), "{err:?}");
}

#[test]
fn async_driver_propagates_processor_errors() {
    let data = fixture("GreenCat.png");
    let asset = Asset::from_source(Cursor::new(data.clone())).unwrap();
    let structure = asset.structure();
    let updates = Updates::new();
    let mut calls = 0;
    let err = block_on(drive_async(
        sans_io::read_process(structure, &updates),
        Some(&mut AsyncCursor::new(data)),
        None,
        Some(&mut |_c: &dyn ProcessChunk| {
            calls += 1;
            Err(asset_io::Error::UserCanceled)
        }),
    ))
    .unwrap_err();
    assert!(matches!(err, asset_io::Error::UserCanceled), "{err:?}");
    assert_eq!(calls, 1);
}

/// The driver future must be `Send` so it can be spawned on multi-threaded runtimes.
#[test]
fn async_driver_future_is_send() {
    fn assert_send<F: std::future::Future + Send>(_: &F) {}
    let mut src = AsyncCursor::new(Vec::<u8>::new());
    let fut = drive_async(sans_io::parse(0), Some(&mut src), None, None);
    assert_send(&fut);
}

#[test]
fn works_on_tokio_with_compat() {
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let dir = tempfile::tempdir().unwrap();
    let in_path = dir.path().join("in.png");
    let out_path = dir.path().join("out.png");
    let data = fixture("GreenCat.png");
    std::fs::write(&in_path, &data).unwrap();
    let write_updates = updates();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let structure = rt.block_on(async {
        let file = tokio::fs::File::open(&in_path).await.unwrap();
        let len = file.metadata().await.unwrap().len();
        let mut src = file.compat();
        let structure = drive_async(sans_io::parse(len), Some(&mut src), None, None)
            .await
            .unwrap();
        // Spawn the write on the runtime to prove the driver future is `Send + 'static`-able.
        let handle = tokio::spawn(async move {
            let mut dest = tokio::fs::File::create(&out_path)
                .await
                .unwrap()
                .compat_write();
            drive_async(
                sans_io::write(&structure, &write_updates, false),
                Some(&mut src),
                Some(&mut dest),
                None,
            )
            .await
            .unwrap();
            out_path
        });
        handle.await.unwrap()
    });

    let mut expected = Cursor::new(Vec::new());
    Asset::from_source(Cursor::new(data))
        .unwrap()
        .write(&mut expected, &updates())
        .unwrap();
    assert!(std::fs::read(structure).unwrap() == expected.into_inner());
}
