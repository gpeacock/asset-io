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

//! Pluggable container handlers.
//!
//! A [`ContainerHandler`] teaches asset-io one container format. The built-in
//! JPEG and PNG handlers implement it, and so can a handler registered by a
//! caller (see [`HandlerRegistry`](crate::HandlerRegistry)). A parsed
//! [`Structure`] keeps the handler that produced it, so everything that depends on
//! the container (subdividing scans, box names, patching trailers, rewriting) works
//! on any structure without the caller knowing the format.

use crate::error::{Error, Result};
use crate::plan::WritePlan;
use crate::segment::{Name, Segment, SegmentKind};
use crate::sink::Sink;
use crate::structure::{SegmentId, Structure};
use crate::task::{BoxFuture, Io, Task};
use crate::walk::Node;
use crate::Updates;
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

/// A readable, seekable stream. Blanket-implemented, so any `Read + Seek` works.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

/// A readable, writable, seekable stream. Blanket-implemented.
pub trait ReadWriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek + ?Sized> ReadWriteSeek for T {}

/// How many leading bytes [`ContainerHandler::detect`] is given.
pub const DETECT_LEN: usize = 64;

/// One format a container handler serves: a media type and its file extensions.
///
/// A container that carries several media types (ISO BMFF: HEIC, MP4, M4A...)
/// declares one `FormatSpec` per media type. Format strings are matched ignoring
/// case, surrounding whitespace and a leading dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FormatSpec {
    /// The media type, for example `image/jpeg`.
    pub media_type: &'static str,
    /// The preferred file extension, without a dot, for example `jpg`.
    pub extension: &'static str,
    /// Other media types accepted for this format, for example `image/x-webp`.
    pub media_type_aliases: &'static [&'static str],
    /// Other file extensions accepted for this format, for example `jpeg`.
    pub extension_aliases: &'static [&'static str],
}

impl FormatSpec {
    /// A format with one media type and one extension.
    pub const fn new(media_type: &'static str, extension: &'static str) -> Self {
        Self {
            media_type,
            extension,
            media_type_aliases: &[],
            extension_aliases: &[],
        }
    }

    /// Adds media types accepted for this format.
    pub const fn media_type_aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.media_type_aliases = aliases;
        self
    }

    /// Adds file extensions accepted for this format.
    pub const fn extension_aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.extension_aliases = aliases;
        self
    }

    /// Every file extension this format accepts, the preferred one first.
    pub fn extensions(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.extension).chain(self.extension_aliases.iter().copied())
    }

    /// Every media type this format accepts, the preferred one first.
    pub fn media_types(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.media_type).chain(self.media_type_aliases.iter().copied())
    }

    /// Every format string (extension or media type) this format accepts.
    pub fn format_strings(&self) -> impl Iterator<Item = &'static str> {
        self.extensions().chain(self.media_types())
    }
}

/// Knowledge of one container format.
///
/// Implementations must be cheap to share (`Send + Sync`) and stateless in
/// practice: one instance serves every asset of its formats.
pub trait ContainerHandler: std::fmt::Debug + Send + Sync {
    /// The container's identity, for example `"jpeg"` or `"bmff"`. Stable and
    /// independent of the format strings the handler serves; two format strings
    /// belong to the same container iff their handlers have the same id.
    fn id(&self) -> &'static str;

    /// The formats this handler serves, each a media type with its extensions.
    fn formats(&self) -> &[FormatSpec];

    /// Whether `header` (the first [`DETECT_LEN`] bytes, or fewer for a short
    /// stream) looks like this container. The default never matches, so the handler
    /// is found only by format string.
    fn detect(&self, _header: &[u8]) -> bool {
        false
    }

    /// Parse the whole structure of an asset, reading it through `io`.
    ///
    /// This is the method to implement. It is `async` so that the same code can be
    /// run by any I/O: see [`Task`](crate::Task). Read with [`Io`]; everything else
    /// is ordinary code. The default reports [`Error::UnsupportedFormat`], so a
    /// handler that implements the blocking [`parse`](Self::parse) instead keeps
    /// working, but cannot be run by [`HandlerRegistry::parse_task`](crate::HandlerRegistry::parse_task).
    ///
    /// The result must tile the stream (see [`StructureBuilder`](crate::StructureBuilder))
    /// and must be built with `self` as its handler, which is why this takes the
    /// `Arc`: `StructureBuilder::new(self, ...)`.
    fn parse_async(self: Arc<Self>, _io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async { Err(Error::UnsupportedFormat) })
    }

    /// Parse the whole structure of an asset from a stream.
    ///
    /// The default runs [`parse_async`](Self::parse_async) with blocking reads. A
    /// handler written before that method existed can implement this instead.
    fn parse(self: Arc<Self>, src: &mut dyn ReadSeek) -> Result<Structure> {
        let total = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        Task::new(total, |io| self.parse_async(io)).run(src)
    }

    /// Describe the write of the asset with `updates` applied: the structure of the
    /// output and the steps that produce it. Nothing is written. The source is
    /// read through `io`, which holds the stream `src_structure` was parsed from.
    ///
    /// This is the method to implement for writing; it is `async` for the same
    /// reason as [`parse_async`](Self::parse_async). The default
    /// [`write`](Self::write) executes the plan, and the default
    /// [`plan`](Self::plan) returns its structure. A handler that needs nothing
    /// from the source ignores `io`. The default reports
    /// [`Error::UnsupportedFormat`].
    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        _src_structure: &'f Structure,
        _io: Io,
        _updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async { Err(Error::UnsupportedFormat) })
    }

    /// Like [`write_plan_async`](Self::write_plan_async), reading the source from
    /// `src`. The default runs that method with blocking reads; a handler written
    /// before it existed can implement this instead.
    fn write_plan<'a>(
        &self,
        src_structure: &Structure,
        src: &mut dyn ReadSeek,
        updates: &'a Updates,
    ) -> Result<WritePlan<'a>> {
        Task::new(src_structure.total_size(), |io| {
            self.write_plan_async(src_structure, io, updates)
        })
        .run(src)
    }

    /// Write the asset with `updates` applied, delivering every output byte to
    /// `sink`, and return the structure of what was written.
    ///
    /// The default executes [`write_plan`](Self::write_plan). Override it only to
    /// write some other way, such as a container whose output cannot be described
    /// ahead of time.
    fn write(
        &self,
        src_structure: &Structure,
        src: &mut dyn ReadSeek,
        out: &mut dyn ReadWriteSeek,
        updates: &Updates,
        sink: &mut dyn Sink,
    ) -> Result<Structure> {
        let plan = self.write_plan(src_structure, src, updates)?;
        plan.execute(src, out, sink)?;
        Ok(plan.into_structure())
    }

    /// The structure [`write`](Self::write) would produce for `updates`, without
    /// writing anything. `src` must be the stream `src_structure` was parsed from;
    /// handlers that need nothing from it ignore it.
    ///
    /// Called through [`Structure::plan`](crate::Structure::plan). The default is
    /// the structure of [`write_plan`](Self::write_plan). Implementations must return
    /// exactly what `write` does.
    fn plan(
        &self,
        src_structure: &Structure,
        src: &mut dyn ReadSeek,
        updates: &Updates,
    ) -> Result<Structure> {
        Ok(self
            .write_plan(src_structure, src, updates)?
            .into_structure())
    }

    /// The asset's XMP packet, or `None` if it has none. Called through
    /// [`Structure::xmp`](crate::Structure::xmp).
    ///
    /// The default reads the single XMP entity. Containers that split XMP over
    /// several elements and must reassemble it (JPEG extended XMP) override this.
    fn read_xmp(&self, structure: &Structure, src: &mut dyn ReadSeek) -> Result<Option<Vec<u8>>> {
        let mut src = src;
        match structure.entity(SegmentKind::Xmp)? {
            Some(e) => structure.read_entity(&mut src, e).map(Some),
            None => Ok(None),
        }
    }

    /// The container's own encoding of one metadata item: the bytes
    /// [`write`](Self::write) would insert for `data` of `kind`, framing included.
    /// For JPEG that is the APP11 segments of a JUMBF, for PNG the whole `caBX`
    /// chunk with its checksum, for ISO BMFF the whole C2PA `uuid` box.
    ///
    /// This is for callers that insert the data into an asset themselves and have
    /// to match what a writer produces. Callers that use [`write`](Self::write) never
    /// need it. The default reports [`Error::UnsupportedFormat`]; a container for
    /// which the item is stored as it is returns `data` unchanged.
    fn compose(&self, _kind: SegmentKind, _data: &[u8]) -> Result<Vec<u8>> {
        Err(Error::UnsupportedFormat)
    }

    /// Split a segment flagged [`Segment::subdivides`] into its finer elements, in
    /// order, tiling the segment. Called through
    /// [`Structure::elements`](crate::Structure::elements); never needed unless the
    /// parser flags a segment.
    fn subdivide(
        &self,
        _src: &mut dyn ReadSeek,
        _segment: &Segment,
        _emit: &mut dyn FnMut(&Segment) -> Result<()>,
    ) -> Result<()> {
        Err(Error::UnsupportedFormat)
    }

    /// Visit the container's element hierarchy in file order, parents before
    /// children. Called through [`Structure::walk`](crate::Structure::walk).
    ///
    /// The default reports each segment as a top-level node. Containers that nest
    /// elements inside segments override it and read only what they need.
    fn walk(
        &self,
        structure: &Structure,
        _src: &mut dyn ReadSeek,
        f: &mut dyn FnMut(&Node<'_>) -> Result<()>,
    ) -> Result<()> {
        let mut path = String::new();
        for seg in structure.segments() {
            path.clear();
            path.push('/');
            if let Some(n) = self.box_name(seg.name()) {
                path.push_str(&n);
            }
            f(&Node {
                path: &path,
                name: seg.name(),
                extent: seg.extent(),
                header_len: seg.header().len() as u32,
                depth: 1,
                version: None,
                flags: None,
                user_type: None,
            })?;
        }
        Ok(())
    }

    /// Repair a checksum or similar that covers a segment whose payload was just
    /// patched in place. The default does nothing.
    fn fix_trailer(
        &self,
        _out: &mut dyn ReadWriteSeek,
        _structure: &Structure,
        _segment: SegmentId,
    ) -> Result<()> {
        Ok(())
    }

    /// The name a box-based hash lists for `name`, or `None` if the element is not
    /// a box. The default covers four character codes and numeric ids; override it
    /// for names with other text, such as JPEG marker symbols.
    fn box_name(&self, name: Name) -> Option<Cow<'static, str>> {
        match name {
            Name::None => None,
            Name::FourCC(cc) => Some(Cow::Owned(String::from_utf8_lossy(&cc).into_owned())),
            Name::Id(n) => Some(Cow::Owned(n.to_string())),
            Name::Marker(_) => Some(Cow::Owned(name.to_string())),
        }
    }
}
