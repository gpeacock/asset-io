//! Error types for jumbf-io

use std::io;

/// Result type for jumbf-io operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during JUMBF I/O operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Invalid file format
    #[error("Invalid format: {0}")]
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

