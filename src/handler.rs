//! Pluggable container handlers.
//!
//! A [`ContainerHandler`] teaches asset-io one container format. The built-in
//! JPEG and PNG handlers implement it, and so can a handler registered by a
//! caller (see [`HandlerRegistry`](crate::HandlerRegistry)). A parsed
//! [`Structure`] keeps the handler that produced it, so everything that depends on
//! the container (subdividing scans, box names, patching trailers, rewriting) works
//! on any structure without the caller knowing the format.

use crate::error::{Error, Result};
use crate::segment::{Name, Segment, SegmentKind};
use crate::sink::Sink;
use crate::structure::{SegmentId, Structure};
use crate::walk::Node;
use crate::Updates;
use std::borrow::Cow;
use std::io::{Read, Seek, Write};
use std::sync::Arc;

/// A readable, seekable stream. Blanket-implemented, so any `Read + Seek` works.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

/// A readable, writable, seekable stream. Blanket-implemented.
pub trait ReadWriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek + ?Sized> ReadWriteSeek for T {}

/// How many leading bytes [`ContainerHandler::detect`] is given.
pub const DETECT_LEN: usize = 64;

/// Knowledge of one container format.
///
/// Implementations must be cheap to share (`Send + Sync`) and stateless in
/// practice: one instance serves every asset of its formats.
pub trait ContainerHandler: std::fmt::Debug + Send + Sync {
    /// Format strings this handler serves: file extensions and MIME types, for
    /// example `["jpg", "jpeg", "image/jpeg"]`. Matching ignores case and
    /// surrounding whitespace. The first entry is the handler's canonical id.
    fn formats(&self) -> &[&'static str];

    /// Whether `header` (the first [`DETECT_LEN`] bytes, or fewer for a short
    /// stream) looks like this container. The default never matches, so the handler
    /// is found only by format string.
    fn detect(&self, _header: &[u8]) -> bool {
        false
    }

    /// Parse the whole layout of an asset.
    ///
    /// The result must tile the stream (see [`StructureBuilder`](crate::StructureBuilder))
    /// and must be built with `self` as its handler, which is why this takes the
    /// `Arc`: `StructureBuilder::new(self, ...)`.
    fn parse(self: Arc<Self>, src: &mut dyn ReadSeek) -> Result<Structure>;

    /// Write the asset with `updates` applied, delivering every output byte to
    /// `sink`, and return the layout of what was written.
    fn write(
        &self,
        src_structure: &Structure,
        src: &mut dyn ReadSeek,
        out: &mut dyn ReadWriteSeek,
        updates: &Updates,
        sink: &mut dyn Sink,
    ) -> Result<Structure>;

    /// The layout [`write`](Self::write) would produce for `updates`, without
    /// writing anything. `src` must be the stream `src_structure` was parsed from;
    /// handlers that need nothing from it ignore it.
    ///
    /// Called through [`Structure::plan`](crate::Structure::plan). The default
    /// reports [`Error::UnsupportedFormat`], so handlers written before this method
    /// existed keep compiling. Implementations must return exactly what `write` does.
    fn plan(
        &self,
        _src_structure: &Structure,
        _src: &mut dyn ReadSeek,
        _updates: &Updates,
    ) -> Result<Structure> {
        Err(Error::UnsupportedFormat)
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

    /// Pairs of (file extension, media type) for the formats this handler serves.
    ///
    /// The default pairs every extension-shaped entry of [`formats`](Self::formats)
    /// (no `/`) with the first media-type-shaped entry, which is right when all
    /// entries are one format (`["jpg", "jpeg", "image/jpeg"]`). Override it when
    /// one container serves several media types (BMFF: `heic`, `mp4`, `m4a`...).
    /// Used through [`HandlerRegistry::media_type_for`](crate::HandlerRegistry::media_type_for).
    fn media_types(&self) -> Vec<(&'static str, &'static str)> {
        let formats = self.formats();
        let Some(mime) = formats.iter().find(|f| f.contains('/')) else {
            return Vec::new();
        };
        formats
            .iter()
            .filter(|f| !f.contains('/'))
            .map(|ext| (*ext, *mime))
            .collect()
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
    /// a box. The default covers JPEG markers, four character codes and numeric
    /// ids; override it for names with other text.
    fn box_name(&self, name: Name) -> Option<Cow<'static, str>> {
        name.box_name()
    }
}
