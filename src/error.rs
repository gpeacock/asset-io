//! Error types for jumbf-io

use std::cell::RefCell;
use std::io;

/// Result type for jumbf-io operations
pub type Result<T> = std::result::Result<T, Error>;

/// Message carried by synthetic [`io::Error`] values from [`crate::processing_writer::ProcessingWriter`]
/// when a processor callback fails. Used so [`From<io::Error>`] can recover the real [`Error`] safely.
pub(crate) const PROCESSOR_IO_SENTINEL: &str = "asset-io processor error";

thread_local! {
    static PENDING_PROCESSOR_ERROR: RefCell<Option<Error>> = const { RefCell::new(None) };
}

/// Stash a processor callback error so it can be recovered when converting the
/// synthetic [`io::Error`] from [`crate::processing_writer::ProcessingWriter`] via [`From`].
pub(crate) fn set_pending_processor_error(e: Error) {
    PENDING_PROCESSOR_ERROR.with(|c| {
        *c.borrow_mut() = Some(e);
    });
}

pub(crate) fn take_pending_processor_error() -> Option<Error> {
    PENDING_PROCESSOR_ERROR.with(|c| c.borrow_mut().take())
}

/// Errors that can occur during JUMBF I/O operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(io::Error),

    /// Returned when a read/write processor callback stops the operation (e.g. user cancel).
    #[error("operation canceled by processor")]
    UserCanceled,

    /// Invalid file format
    #[error("Invalid container: {0}")]
    InvalidFormat(String),

    /// Unsupported file format
    #[error("Unsupported format")]
    UnsupportedFormat,

    /// Invalid segment
    #[error("Invalid segment at offset {offset}: {reason}")]
    InvalidSegment { offset: u64, reason: String },

    /// XML parsing error (from quick-xml)
    #[cfg(feature = "xmp")]
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::Other && e.to_string() == PROCESSOR_IO_SENTINEL {
            if let Some(pending) = take_pending_processor_error() {
                return pending;
            }
        }
        Error::Io(e)
    }
}
