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
mod execute;
mod handler;
pub mod jumbf;
#[cfg(feature = "memory-mapped")]
mod mapped;
pub mod plan;
mod plan_builder;
mod range;
mod registry;
mod segment;
mod sink;
mod structure;
mod task;
pub mod testing;
mod thumbnail;
#[cfg(feature = "exif")]
mod tiff;
mod updates;
mod walk;
#[cfg(feature = "xmp")]
mod xmp;

pub use crc::Crc32;
pub use error::{Error, Result};
pub use execute::{Action, Execution};
pub use handler::{ContainerHandler, FormatSpec, ReadSeek, ReadWriteSeek, DETECT_LEN};
#[cfg(feature = "memory-mapped")]
pub use mapped::MappedFile;
pub use plan::{Step, WritePlan};
pub use plan_builder::PlanBuilder;
pub use range::ByteRange;
pub use registry::HandlerRegistry;
pub use segment::{labels, Name, Role, Segment, SegmentKind, AFTER_BOX, MAX_SEGMENT_SIZE};
pub use sink::{Chunk, NullSink, Sink};
pub use structure::{
    Entity, Part, Region, SegmentId, Structure, StructureBuilder, SubAsset, MAX_SEGMENTS,
};
pub use task::{block_on, BoxFuture, Io, Progress, ReadRequest, Reader, Task, DEFAULT_WINDOW};
pub use thumbnail::Thumbnail;
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;
pub use updates::{Existing, MetadataSlot, MetadataUpdate, Updates};
pub use walk::Node;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;
