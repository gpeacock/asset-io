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
//! split into header, payload and trailer, plus [`Item`]s: logical units such as
//! XMP, JUMBF, EXIF or an embedded file, assembled from [`Extent`]s. It carries
//! geometry only: no data, and no hashing or signing rules. Callers decide what
//! each byte range means.
//!
//! # Glossary
//!
//! - **Asset**: a complete byte stream. Assets are recursive: an asset can contain
//!   embedded assets.
//! - **Container**: a structural family (ISO BMFF, JPEG, RIFF). **Format**: a media
//!   type within one (HEIC and MP4 are both ISO BMFF).
//! - **Handler**: implements a container's specifications ([`ContainerHandler`]).
//! - **Range**: an offset and a length ([`ByteRange`]).
//! - **Segment**: a unit of the asset's tiling. Segments are ordered, do not
//!   overlap and cover every byte; a segment is what a writer copies, replaces or
//!   moves. Its [`SegmentKind`] gives its dominant purpose: `Preamble` (the leading
//!   bytes that identify the format; "signature" is avoided, as C2PA uses it for
//!   cryptographic signatures), `Metadata`, `MediaData` (coded media of any type)
//!   or `Other`. Items, not segment kinds, say what bytes belong to.
//! - **Role**: within a segment, framing ([`Role::Header`], [`Role::Trailer`])
//!   versus content ([`Role::Payload`]). "Header" always means framing.
//! - **Region**: a segment, a role and a range: the view a hasher takes
//!   ([`Region`]).
//! - **Element**: a native unit of the container (box, chunk, marker segment, PDF
//!   object, IFD). [`Structure::elements`] is the finest tiling by elements.
//! - **Node**: an element and its position in the container's tree
//!   ([`Node`], [`Structure::walk`]).
//! - **Item**: a logical unit the format defines, with an [`ItemKind`] (the
//!   specification that defines it), an [`ItemCategory`] derived from the kind
//!   (metadata or asset) and an [`Encoding`] (its serialization).
//! - **Transfer encoding**: how the bytes a container stores for an item wrap its
//!   content, such as SVG's base64, a PDF `FlateDecode` (zlib) stream or a HEIF
//!   `content_encoding` ([`TransferEncoding`], [`Item::transfer`]). It is distinct
//!   from the [`Encoding`], which is the serialization of the content itself. The
//!   core always identifies it; decoders are optional features (`flate`,
//!   `brotli`). [`Structure::read_item`] returns the
//!   stored bytes (what a hash covers); [`Structure::read_content`] the content.
//! - **Extent**: one contiguous run of an item's bytes. An item may be
//!   multi-extent; "fragment" is reserved for fragmented ISO BMFF.
//! - **Identifiers**: native names, kept exactly: FourCCs, markers, UUIDs, JUMBF
//!   types, URIs, media types and container labels ([`Name`],
//!   [`Item::native_type`], [`Item::native_id`]).
//! - **Relation**: a reference from one item to another that keeps its native type
//!   (HEIF `thmb`, `auxl`, `cdsc`, `dimg`); [`RelationKind`] gives common names.
//! - **Sub-asset**: a contiguous, top-level embedded asset, such as each image of a
//!   Multi-Picture Format file ([`SubAsset`]).
//! - **Embedded asset**: an item the format declares to be a file of a media type
//!   ([`ItemKind::EmbeddedAsset`]). Items whose bytes the format does not type are
//!   [`ItemKind::Other`]; [`Structure::scan_embedded`] also finds those that turn out
//!   to be files.
//!
//! # Layering
//!
//! 1. Account for every byte: segments always tile the asset.
//! 2. Expose the structure the format's own specifications define, including
//!    companion specifications (EXIF/TIFF, MPF, JPEG XT, the HEIF item model), and
//!    preserve native identifiers.
//! 3. Never interpret content that is portable across formats (C2PA manifests, XMP
//!    schemas, IPTC vocabularies). That belongs to optional higher-level helpers
//!    built on this API. Where such content is placed in each container is
//!    container knowledge, and is here.
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
//! use asset_io::{HandlerRegistry, ItemKind, NullSink, Updates};
//! use std::fs::OpenOptions;
//!
//! # fn main() -> asset_io::Result<()> {
//! let registry = HandlerRegistry::builtin();
//! let mut src = std::fs::File::open("image.jpg")?;
//! let structure = registry.parse("jpg", &mut src)?;
//!
//! // Read a metadata item, which may have extents in several segments.
//! if let Some(jumbf) = structure.item(ItemKind::Jumbf)? {
//!     let bytes = structure.read_item(&mut src, jumbf)?;
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
mod embedded;
mod error;
mod execute;
mod handler;
mod io;
mod item;
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
pub mod transfer;
mod updates;
mod walk;
#[cfg(feature = "xmp")]
mod xmp;

pub use crc::Crc32;
pub use embedded::{EmbeddedAsset, EmbeddedReader, ScanLimits};
pub use error::{Error, Result};
pub use execute::{Action, Execution};
pub use handler::{ContainerHandler, FormatSpec, ReadSeek, ReadWriteSeek, DETECT_LEN};
pub use io::{BoxFuture, Io, Reader, DEFAULT_WINDOW};
pub use item::{Encoding, Extent, Item, ItemCategory, ItemInfo, ItemKind, Relation, RelationKind};
#[cfg(feature = "memory-mapped")]
pub use mapped::MappedFile;
pub use plan::{Step, WritePlan};
pub use plan_builder::PlanBuilder;
pub use range::ByteRange;
pub use registry::HandlerRegistry;
pub use segment::{labels, Name, Role, Segment, SegmentKind, AFTER_BOX, MAX_SEGMENT_SIZE};
pub use sink::{Chunk, NullSink, Sink};
pub use structure::{Region, SegmentId, Structure, StructureBuilder, SubAsset, MAX_SEGMENTS};
pub use task::{block_on, Progress, ReadRequest, Task};
pub use thumbnail::Thumbnail;
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;
pub use transfer::TransferEncoding;
pub use updates::{Existing, MetadataSlot, MetadataUpdate, Updates};
pub use walk::Node;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;
