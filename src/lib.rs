//! Streaming, container-agnostic access to media structure and metadata.
//!
//! This facade re-exports [`asset_io_core`] and assembles the handlers selected by
//! Cargo features. No handlers are enabled by default. Enable `jpeg`, `png`,
//! `bmff`, `riff`, `svg`, `pdf`, `c2pa`, or `all-formats` to populate
//! [`HandlerRegistry::builtin`].
//!
//! Applications that supply their own handlers can depend on `asset-io-core`
//! directly. Individual handlers can also be registered explicitly.
//!
//! ```no_run
//! use asset_io::{HandlerRegistry, ItemKind};
//!
//! # fn main() -> asset_io::Result<()> {
//! // Enable the `jpeg` feature for this example.
//! let registry = HandlerRegistry::builtin();
//! let mut source = std::fs::File::open("image.jpg")?;
//! let structure = registry.parse_detect(&mut source)?;
//! if let Some(item) = structure.item(ItemKind::Xmp)? {
//!     let xmp = structure.read_content(&mut source, item)?;
//!     println!("{} bytes of XMP", xmp.len());
//! }
//! # Ok(())
//! # }
//! ```

pub use asset_io_core::*;

pub mod containers;
mod registry;

pub use registry::HandlerRegistry;
