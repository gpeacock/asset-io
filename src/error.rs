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

//! Error types

use std::io;

/// Result type for container operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur while reading or writing a container
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// A caller-supplied callback (for example a [`Sink`](crate::Sink))
    /// stopped the operation, such as on user cancel.
    #[error("operation canceled by a callback")]
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

    /// A required item is absent.
    #[error("{what} not found")]
    NotFound { what: &'static str },

    /// More than one candidate where exactly one is required.
    #[error("{what} is ambiguous ({count} candidates)")]
    Ambiguous { what: &'static str, count: usize },

    /// A resource limit was exceeded.
    #[error("limit exceeded: {what} (limit {limit})")]
    LimitExceeded { what: &'static str, limit: u64 },

    /// XML parsing error (from quick-xml)
    #[cfg(feature = "xmp")]
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
}
