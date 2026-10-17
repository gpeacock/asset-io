//! Sans-IO state machines.
//!
//! Every long-running operation in this crate (parsing, hashing, writing, metadata reads,
//! in-place updates) is available as a [`Machine`] that never performs I/O itself.
//! Instead, it yields [`Step`]s describing the I/O it needs; the caller performs that I/O
//! however it likes (blocking files, async runtimes, HTTP range requests, ...) and then
//! advances the machine again.
//!
//! All internal state is kept inside the machine between steps, so callers can:
//! - interleave work with other tasks (one step at a time),
//! - observe [`Progress`] at any point,
//! - cancel at any point simply by dropping the machine.
//!
//! # Protocol
//!
//! ```text
//! loop {
//!     match machine.step()? {
//!         Step::Read { offset, buf } => {
//!             // Fill `buf` with source bytes starting at `offset`.
//!             // Short reads are fine; 0 means EOF.
//!             let n = read_at(offset, buf);
//!             machine.filled(n)?;
//!         }
//!         Step::Write { offset, data } => {
//!             // Write all of `data` to the destination at `offset`.
//!             // For streaming writes offsets are strictly sequential.
//!             write_at(offset, data);
//!         }
//!         Step::Process(chunk) => hasher.update(chunk.data()),
//!         Step::Done => break,
//!     }
//! }
//! let output = machine.finish()?;
//! ```
//!
//! Source reads always carry absolute offsets; the machine never asks the caller to seek.
//! Where the container format allows it, read offsets are monotonically increasing so that
//! forward-only streams can be used. Destination writes produced by `write` machines are
//! always strictly sequential (append-only); only in-place update machines issue positioned
//! writes.
//!
//! # Example: hashing with a blocking file
//!
//! ```no_run
//! use asset_io::sans_io::{self, Step};
//! use asset_io::{ProcessChunk, Updates};
//! use sha2::{Digest, Sha256};
//! use std::io::{Read, Seek, SeekFrom};
//!
//! # fn main() -> asset_io::Result<()> {
//! let mut file = std::fs::File::open("image.jpg")?;
//! let len = file.metadata()?.len();
//!
//! // 1. Parse
//! let mut parser = sans_io::parse(len);
//! loop {
//!     match parser.step()? {
//!         Step::Read { offset, buf } => {
//!             file.seek(SeekFrom::Start(offset))?;
//!             let n = file.read(buf)?;
//!             parser.filled(n)?;
//!         }
//!         Step::Done => break,
//!         _ => unreachable!("parsing only reads"),
//!     }
//! }
//! let structure = parser.finish()?;
//!
//! // 2. Hash
//! let updates = Updates::new();
//! let mut hasher = Sha256::new();
//! let mut machine = sans_io::read_process(&structure, &updates);
//! loop {
//!     match machine.step()? {
//!         Step::Read { offset, buf } => {
//!             file.seek(SeekFrom::Start(offset))?;
//!             let n = file.read(buf)?;
//!             machine.filled(n)?;
//!         }
//!         Step::Process(chunk) => hasher.update(chunk.data()),
//!         Step::Done => break,
//!         Step::Write { .. } => unreachable!(),
//!     }
//!     println!("{:?}", machine.progress());
//! }
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "futures-io")]
mod async_driver;
mod driver;
pub(crate) mod io;
mod ops;

#[cfg(feature = "futures-io")]
pub use async_driver::{drive_async, AsyncProcessorFn, AsyncReadSeek, AsyncWriteSeek};
pub(crate) use driver::drive;
pub use driver::{drive_sync, ProcessorFn, ReadSeek, WriteSeek};
pub use ops::*;

use crate::error::{Error, Result};
use crate::processing_writer::ProcessChunk;
use crate::segment::Segment;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// A single I/O request (or event) yielded by a [`Machine`].
#[derive(Debug)]
pub enum Step<'a> {
    /// The machine needs source bytes starting at `offset`.
    ///
    /// Fill `buf` (fully or partially) and then call [`Machine::filled`] with the number of
    /// bytes written into it. Returning `0` signals end of input.
    Read {
        /// Absolute source offset of `buf[0]`.
        offset: u64,
        /// Destination buffer to fill.
        buf: &'a mut [u8],
    },
    /// The machine produced destination bytes. Write all of `data` at `offset`.
    ///
    /// For streaming writes `offset` always equals the number of bytes written so far.
    Write {
        /// Absolute destination offset of `data[0]`.
        offset: u64,
        /// Bytes to write.
        data: &'a [u8],
    },
    /// A processing event (e.g. bytes to hash). See [`Chunk`].
    Process(Chunk<'a>),
    /// The machine has finished. Call [`Machine::finish`] to retrieve the output.
    Done,
}

/// A processing event produced by a [`Machine`].
///
/// Implements [`ProcessChunk`] so it can be passed straight to existing processors.
#[derive(Debug, Clone, Copy)]
pub enum Chunk<'a> {
    /// Bytes to process (hash).
    Data(&'a [u8]),
    /// BMFF `mdat` content (for Merkle-tree hashing).
    Mdat {
        /// Index of the mdat box (0, 1, 2, ...).
        id: usize,
        /// Chunk of mdat content.
        data: &'a [u8],
        /// True if the mdat uses a 64-bit size.
        large_size: bool,
    },
    /// Start of a logical segment in the output (carries no data).
    Boundary(&'a Segment),
}

impl ProcessChunk for Chunk<'_> {
    fn data(&self) -> &[u8] {
        match self {
            Chunk::Data(d) => d,
            Chunk::Mdat { data, .. } => data,
            Chunk::Boundary(_) => &[],
        }
    }

    fn id(&self) -> Option<usize> {
        match self {
            Chunk::Mdat { id, .. } => Some(*id),
            _ => None,
        }
    }

    fn large_size(&self) -> Option<bool> {
        match self {
            Chunk::Mdat { large_size, .. } => Some(*large_size),
            _ => None,
        }
    }

    fn segment(&self) -> Option<&Segment> {
        match self {
            Chunk::Boundary(s) => Some(s),
            _ => None,
        }
    }
}

/// Progress counters for a [`Machine`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Source bytes delivered via [`Machine::filled`].
    pub bytes_read: u64,
    /// Destination bytes emitted via [`Step::Write`].
    pub bytes_written: u64,
    /// Bytes emitted via [`Step::Process`].
    pub bytes_processed: u64,
    /// Expected total for the dominant counter of this operation, when known
    /// (bytes to process when hashing, destination size when writing).
    pub total: Option<u64>,
}

// ---------------------------------------------------------------------------
// Channel between the machine and the internal future
// ---------------------------------------------------------------------------

/// Raw request posted by an internal I/O future.
///
/// Pointers refer to memory borrowed by the (suspended, pinned) future. They are only
/// dereferenced by [`Machine::step`] while the future is suspended and cannot be polled
/// or dropped, which keeps them valid.
#[derive(Clone, Copy)]
pub(crate) enum RawReq {
    Read {
        offset: u64,
        ptr: *mut u8,
        len: usize,
    },
    Write {
        offset: u64,
        ptr: *const u8,
        len: usize,
    },
    Data {
        ptr: *const u8,
        len: usize,
    },
    Mdat {
        id: usize,
        ptr: *const u8,
        len: usize,
        large_size: bool,
    },
    Boundary(*const Segment),
}

// SAFETY: see `RawReq` docs; the pointers are never dereferenced concurrently.
unsafe impl Send for RawReq {}

#[derive(Default)]
pub(crate) struct ChanState {
    pub(crate) req: Option<RawReq>,
    pub(crate) resp: Option<usize>,
    pub(crate) progress: Progress,
}

#[derive(Default)]
pub(crate) struct Chan(Mutex<ChanState>);

impl Chan {
    pub(crate) fn lock(&self) -> MutexGuard<'_, ChanState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Awaiting {
    Nothing,
    Fill(usize),
    Ack,
    Finished,
    Failed,
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A resumable, sans-IO operation. See the [module docs](self).
///
/// Dropping a machine cancels the operation.
pub struct Machine<'a, T> {
    fut: Option<BoxFut<'a, T>>,
    chan: Arc<Chan>,
    awaiting: Awaiting,
    output: Option<T>,
}

impl<T> std::fmt::Debug for Machine<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Machine")
            .field("awaiting", &self.awaiting)
            .field("progress", &self.progress())
            .finish()
    }
}

fn protocol(msg: &'static str) -> Error {
    Error::Protocol(msg)
}

impl<'a, T: Send + 'a> Machine<'a, T> {
    pub(crate) fn new<F, Fut>(f: F) -> Self
    where
        F: FnOnce(io::Io) -> Fut,
        Fut: Future<Output = Result<T>> + Send + 'a,
    {
        let chan = Arc::new(Chan::default());
        let fut = f(io::Io::new(chan.clone()));
        Self {
            fut: Some(Box::pin(fut)),
            chan,
            awaiting: Awaiting::Nothing,
            output: None,
        }
    }
}

impl<T> Machine<'_, T> {
    /// Advance the machine to its next I/O request or event.
    ///
    /// After a [`Step::Read`], [`Machine::filled`] must be called before the next `step`.
    /// [`Step::Write`] and [`Step::Process`] are acknowledged implicitly by the next `step`.
    pub fn step(&mut self) -> Result<Step<'_>> {
        match self.awaiting {
            Awaiting::Fill(_) => return Err(protocol("filled() must be called after Step::Read")),
            Awaiting::Ack => self.chan.lock().resp = Some(0),
            Awaiting::Finished => return Ok(Step::Done),
            Awaiting::Failed => return Err(protocol("machine already failed")),
            Awaiting::Nothing => {}
        }
        let Some(fut) = self.fut.as_mut() else {
            return Ok(Step::Done);
        };
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(result) => {
                self.fut = None;
                match result {
                    Ok(v) => {
                        self.output = Some(v);
                        self.awaiting = Awaiting::Finished;
                        Ok(Step::Done)
                    }
                    Err(e) => {
                        self.awaiting = Awaiting::Failed;
                        Err(e)
                    }
                }
            }
            Poll::Pending => {
                let mut st = self.chan.lock();
                let Some(req) = st.req.take() else {
                    drop(st);
                    self.awaiting = Awaiting::Failed;
                    self.fut = None;
                    return Err(protocol("machine stalled without an I/O request"));
                };
                // SAFETY: the pointers in `req` reference memory owned or borrowed by the
                // suspended future. The returned `Step` borrows `self` mutably, so the future
                // can neither be polled nor dropped while the step is alive.
                let step = unsafe {
                    match req {
                        RawReq::Read { offset, ptr, len } => {
                            self.awaiting = Awaiting::Fill(len);
                            Step::Read {
                                offset,
                                buf: std::slice::from_raw_parts_mut(ptr, len),
                            }
                        }
                        RawReq::Write { offset, ptr, len } => {
                            self.awaiting = Awaiting::Ack;
                            st.progress.bytes_written += len as u64;
                            Step::Write {
                                offset,
                                data: std::slice::from_raw_parts(ptr, len),
                            }
                        }
                        RawReq::Data { ptr, len } => {
                            self.awaiting = Awaiting::Ack;
                            st.progress.bytes_processed += len as u64;
                            Step::Process(Chunk::Data(std::slice::from_raw_parts(ptr, len)))
                        }
                        RawReq::Mdat {
                            id,
                            ptr,
                            len,
                            large_size,
                        } => {
                            self.awaiting = Awaiting::Ack;
                            st.progress.bytes_processed += len as u64;
                            Step::Process(Chunk::Mdat {
                                id,
                                data: std::slice::from_raw_parts(ptr, len),
                                large_size,
                            })
                        }
                        RawReq::Boundary(seg) => {
                            self.awaiting = Awaiting::Ack;
                            Step::Process(Chunk::Boundary(&*seg))
                        }
                    }
                };
                Ok(step)
            }
        }
    }

    /// Report how many bytes were placed into the buffer of the last [`Step::Read`].
    ///
    /// `0` signals end of input.
    pub fn filled(&mut self, n: usize) -> Result<()> {
        match self.awaiting {
            Awaiting::Fill(len) if n <= len => {
                let mut st = self.chan.lock();
                st.resp = Some(n);
                st.progress.bytes_read += n as u64;
                self.awaiting = Awaiting::Nothing;
                Ok(())
            }
            Awaiting::Fill(_) => Err(protocol("filled() reported more bytes than requested")),
            _ => Err(protocol("filled() called without a pending Step::Read")),
        }
    }

    /// Current progress counters.
    pub fn progress(&self) -> Progress {
        self.chan.lock().progress
    }

    /// True once [`Step::Done`] has been returned.
    pub fn is_done(&self) -> bool {
        self.awaiting == Awaiting::Finished
    }

    /// Retrieve the output after [`Step::Done`].
    pub fn finish(mut self) -> Result<T> {
        self.output
            .take()
            .ok_or(protocol("finish() called before the machine completed"))
    }
}
