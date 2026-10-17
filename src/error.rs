//! Error types for jumbf-io

use std::io;

/// Result type for jumbf-io operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during JUMBF I/O operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(io::Error),

    /// Returned when a read/write processor callback stops the operation (e.g. user cancel).
    #[error("operation canceled by processor")]
    UserCanceled,

    /// A sans-IO [`crate::sans_io::Machine`] was driven incorrectly
    /// (e.g. `step()` called without `filled()` after a read).
    #[error("sans-IO protocol error: {0}")]
    Protocol(&'static str),

    /// Invalid file format
    #[error("Invalid container: {0}")]
    InvalidFormat(String),

    /// Unsupported file format
    #[error("Unsupported format")]
    UnsupportedFormat,

    /// A recognized format uses a feature or operation this reader cannot support.
    #[error("Unsupported feature: {0}")]
    UnsupportedFeature(&'static str),

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
        Error::Io(e)
    }
}
