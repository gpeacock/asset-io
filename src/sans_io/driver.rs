//! Blocking driver for [`Machine`]s over `std::io` types.

use super::{Machine, Step};
use crate::error::{Error, Result};
use crate::processing_writer::ProcessChunk;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};

/// Object-safe `Read + Seek`.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

/// Object-safe `Write + Seek`.
pub trait WriteSeek: Write + Seek {}
impl<T: Write + Seek + ?Sized> WriteSeek for T {}

/// Run a [`Machine`] to completion using blocking `std::io` objects.
///
/// - `source` serves [`Step::Read`] requests (required if the machine reads).
/// - `dest` receives [`Step::Write`] output (required if the machine writes).
/// - `processor` receives [`Step::Process`] events (ignored when `None`). Returning an
///   error from the processor stops the machine and returns that error.
///
/// Seeks are only issued when a request is not at the current position, so sequential
/// access stays sequential.
pub fn drive_sync<T>(
    machine: Machine<'_, T>,
    source: Option<&mut dyn ReadSeek>,
    dest: Option<&mut dyn WriteSeek>,
    processor: Option<ProcessorFn<'_>>,
) -> Result<T> {
    drive(machine, source, dest, 0, processor)
}

/// Processing-event callback accepted by [`drive_sync`].
pub type ProcessorFn<'a> = &'a mut dyn FnMut(&dyn ProcessChunk) -> Result<()>;

/// Like [`drive_sync`], but destination offsets are relative to `dest_base`.
pub(crate) fn drive<T>(
    mut machine: Machine<'_, T>,
    mut source: Option<&mut dyn ReadSeek>,
    mut dest: Option<&mut dyn WriteSeek>,
    dest_base: u64,
    mut processor: Option<ProcessorFn<'_>>,
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
                    src.seek(SeekFrom::Start(offset))?;
                }
                let n = loop {
                    match src.read(buf) {
                        Ok(n) => break n,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
                };
                src_pos = Some(offset + n as u64);
                machine.filled(n)?;
            }
            Step::Write { offset, data } => {
                let offset = dest_base + offset;
                let dst = dest.as_deref_mut().ok_or(Error::Protocol(
                    "machine produced output but no destination was provided",
                ))?;
                if dst_pos != Some(offset) {
                    dst.seek(SeekFrom::Start(offset))?;
                }
                dst.write_all(data)?;
                dst_pos = Some(offset + data.len() as u64);
            }
            Step::Process(chunk) => {
                if let Some(p) = processor.as_deref_mut() {
                    p(&chunk)?;
                }
            }
            Step::Done => {
                if let Some(dst) = dest.as_deref_mut() {
                    dst.flush()?;
                }
                return machine.finish();
            }
        }
    }
}
