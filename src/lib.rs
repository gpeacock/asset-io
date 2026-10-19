//! Streaming, container-agnostic access to the structure and metadata of media
//! files (JPEG, PNG, ISO BMFF, ...).
//!
//! A [`Structure`] describes a file as an ordered tiling of [`Segment`]s, each
//! split into header, payload and trailer, plus [`Entity`]s that group the parts
//! of logical items such as XMP or JUMBF. It carries geometry only: no data, and
//! no hashing or signing rules. Callers decide what each byte range means.
//!
//! # Design principles
//!
//! - **Container agnostic**: one API for every format. Format knowledge lives in a
//!   [`ContainerHandler`]; callers never test which format they have.
//! - **Streaming**: a file is parsed and written in one pass, in bounded memory.
//! - **Safe on hostile input**: every length is checked, resources are capped,
//!   and errors are returned rather than guessed around.
//! - **Pluggable**: [`HandlerRegistry`] finds handlers by format string or by
//!   content, and callers can register their own.
//!
//! # Example
//!
//! ```no_run
//! use asset_io::{HandlerRegistry, NullSink, SegmentKind, Updates};
//! use std::fs::OpenOptions;
//!
//! # fn main() -> asset_io::Result<()> {
//! let registry = HandlerRegistry::builtin();
//! let mut src = std::fs::File::open("image.jpg")?;
//! let structure = registry.parse("jpg", &mut src)?;
//!
//! // Read a metadata item, which may span several segments.
//! if let Some(jumbf) = structure.entity(SegmentKind::Jumbf)? {
//!     let bytes = structure.read_entity(&mut src, jumbf)?;
//!     println!("JUMBF: {} bytes", bytes.len());
//! }
//!
//! // Write a copy with changes. Every output byte also goes to a sink, tagged
//! // with its segment and role, so it can be hashed without a second pass.
//! let mut out = OpenOptions::new()
//!     .read(true)
//!     .write(true)
//!     .create(true)
//!     .truncate(true)
//!     .open("out.jpg")?;
//! let updates = Updates::new().set_xmp(b"<x:xmpmeta/>".to_vec()).remove_jumbf();
//! structure.write(&mut src, &mut out, &updates, &mut NullSink)?;
//! # Ok(())
//! # }
//! ```

pub mod containers;
mod crc;
mod error;
mod handler;
#[cfg(feature = "memory-mapped")]
mod mapped;
mod range;
mod registry;
mod segment;
mod sink;
mod structure;
mod thumbnail;
#[cfg(feature = "exif")]
mod tiff;
mod updates;
mod walk;
#[cfg(feature = "xmp")]
mod xmp;

pub use crc::Crc32;
pub use error::{Error, Result};
pub use handler::{ContainerHandler, ReadSeek, ReadWriteSeek, DETECT_LEN};
#[cfg(feature = "memory-mapped")]
pub use mapped::MappedFile;
pub use range::ByteRange;
pub use registry::HandlerRegistry;
pub use segment::{labels, Name, Role, Segment, SegmentKind};
pub use sink::{Chunk, NullSink, Sink};
pub use structure::{Entity, Part, Region, SegmentId, Structure, StructureBuilder, MAX_SEGMENTS};
pub use thumbnail::{Thumbnail, ThumbnailKind};
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;
pub use updates::Updates;
pub use walk::Node;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;

pub(crate) use updates::{MerkleUpdate, MetadataUpdate};
