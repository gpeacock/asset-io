//! Runtime-agnostic async driver for [`Machine`]s over `futures-io` traits.
//!
//! Works with any executor. Tokio users can adapt tokio I/O types with
//! `tokio_util::compat` (`TokioAsyncReadCompatExt::compat`, `compat_write`).

use super::{Machine, Step};
use crate::error::{Error, Result};
use crate::processing_writer::ProcessChunk;
use futures_io::{AsyncRead, AsyncSeek, AsyncWrite};
use std::future::poll_fn;
use std::io::{ErrorKind, SeekFrom};
use std::pin::Pin;

/// Object-safe async `Read + Seek` (`Unpin + Send` so the driver future is `Send`).
pub trait AsyncReadSeek: AsyncRead + AsyncSeek + Unpin + Send {}
impl<T: AsyncRead + AsyncSeek + Unpin + Send + ?Sized> AsyncReadSeek for T {}

/// Object-safe async `Write + Seek` (`Unpin + Send` so the driver future is `Send`).
pub trait AsyncWriteSeek: AsyncWrite + AsyncSeek + Unpin + Send {}
impl<T: AsyncWrite + AsyncSeek + Unpin + Send + ?Sized> AsyncWriteSeek for T {}

/// Processing-event callback accepted by [`drive_async`].
///
/// Processing (e.g. hashing) is CPU work, so the callback is synchronous.
pub type AsyncProcessorFn<'a> = &'a mut (dyn FnMut(&dyn ProcessChunk) -> Result<()> + Send);

/// Run a [`Machine`] to completion using async `futures-io` objects.
///
/// The async counterpart of [`super::drive_sync`]: `source` serves [`Step::Read`],
/// `dest` receives [`Step::Write`] (offsets relative to the start of `dest`), and
/// `processor` receives [`Step::Process`] events. Seeks are only issued when a request is
/// not at the current position. The returned future is `Send` when `T: Send`, and
/// dropping it cancels the operation.
///
/// ```no_run
/// # async fn demo() -> asset_io::Result<()> {
/// use asset_io::{sans_io, Updates};
/// use tokio_util::compat::TokioAsyncReadCompatExt;
///
/// let file = tokio::fs::File::open("image.jpg").await?;
/// let len = file.metadata().await?.len();
/// let mut src = file.compat();
///
/// let structure = sans_io::drive_async(sans_io::parse(len), Some(&mut src), None, None).await?;
/// let updates = Updates::new();
/// let mut bytes = 0u64;
/// sans_io::drive_async(
///     sans_io::read_process(&structure, &updates),
///     Some(&mut src),
///     None,
///     Some(&mut |chunk: &dyn asset_io::ProcessChunk| {
///         bytes += chunk.data().len() as u64;
///         Ok(())
///     }),
/// )
/// .await?;
/// # Ok(())
/// # }
/// ```
pub async fn drive_async<T>(
    mut machine: Machine<'_, T>,
    mut source: Option<&mut dyn AsyncReadSeek>,
    mut dest: Option<&mut dyn AsyncWriteSeek>,
    mut processor: Option<AsyncProcessorFn<'_>>,
) -> Result<T> {
    let mut src_pos: Option<u64> = None;
    let mut dst_pos: Option<u64> = None;
    loop {
        match machine.step()? {
            Step::Read { offset, buf } => {
                let src = source.as_deref_mut().ok_or(Error::Protocol(
                    "machine requested source data but no source was provided",
                ))?;
                if src_pos != Some(offset) {
                    seek(src, offset).await?;
                }
                let n = loop {
                    match poll_fn(|cx| Pin::new(&mut *src).poll_read(cx, buf)).await {
                        Ok(n) => break n,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
                };
                src_pos = Some(offset + n as u64);
                machine.filled(n)?;
            }
            Step::Write { offset, data } => {
                let dst = dest.as_deref_mut().ok_or(Error::Protocol(
                    "machine produced output but no destination was provided",
                ))?;
                if dst_pos != Some(offset) {
                    seek(dst, offset).await?;
                }
                write_all(dst, data).await?;
                dst_pos = Some(offset + data.len() as u64);
            }
            Step::Process(chunk) => {
                if let Some(p) = processor.as_deref_mut() {
                    p(&chunk)?;
                }
            }
            Step::Done => {
                if let Some(dst) = dest.as_deref_mut() {
                    poll_fn(|cx| Pin::new(&mut *dst).poll_flush(cx)).await?;
                }
                return machine.finish();
            }
        }
    }
}

async fn seek<S: AsyncSeek + Unpin + ?Sized>(s: &mut S, offset: u64) -> Result<()> {
    poll_fn(|cx| Pin::new(&mut *s).poll_seek(cx, SeekFrom::Start(offset))).await?;
    Ok(())
}

async fn write_all<W: AsyncWrite + Unpin + ?Sized>(w: &mut W, mut data: &[u8]) -> Result<()> {
    while !data.is_empty() {
        match poll_fn(|cx| Pin::new(&mut *w).poll_write(cx, data)).await {
            Ok(0) => {
                return Err(std::io::Error::new(
                    ErrorKind::WriteZero,
                    "failed to write whole buffer",
                )
                .into())
            }
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
