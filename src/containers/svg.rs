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

//! SVG on the structure model: a text container, with no binary framing.
//!
//! A C2PA manifest is the base64 text of a `<c2pa:manifest>` element in the root
//! element's `<metadata>`; XMP is an `<?xpacket ...?>` packet. The file is cut into
//! segments at those, and only those:
//!
//! - the first segment, a [`Header`](SegmentKind::Header), runs from the start of the
//!   file through the root element's start tag;
//! - a [`Jumbf`](SegmentKind::Jumbf) segment is one `<c2pa:manifest>` element: the open
//!   tag is its header, the base64 text its payload and the close tag its trailer, so
//!   the payload is the range a C2PA hash excludes;
//! - an [`Xmp`](SegmentKind::Xmp) segment is the whole xpacket;
//! - everything between is [`Other`](SegmentKind::Other) text. A gap is cut where the
//!   start tag of the first `<metadata>` ends, and the piece before the cut is named
//!   `Name::Id(1)`, so a writer finds where new metadata goes without scanning again.
//!
//! These boundaries are a function of the bytes, which a writer has to honour: what it
//! plans must be what parsing the output finds. A write is a splice. Text that is
//! kept is copied, so nothing is re-serialized, and only the new elements and, when a
//! manifest is written, the `xmlns:c2pa` declaration on the root element are new bytes.
//!
//! The manifest is stored as base64, so [`decode_entity`](ContainerHandler::decode_entity)
//! and [`encode_entity`](ContainerHandler::encode_entity) turn it from and into the
//! JUMBF itself. [`Structure::store`](crate::Structure::store) does that for you.
//!
//! The whole document is read to parse it (up to 256 MiB), since an XML parser needs
//! it all. A manifest that is not in `svg > metadata` is not one; several are
//! ambiguous, as in any container.

use super::jumbf::superbox_type;
use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, FormatSpec};
use crate::plan::WritePlan;
use crate::plan_builder::PlanBuilder;
use crate::range::ByteRange;
use crate::segment::{Name, Role, SegmentKind, MAX_SEGMENT_SIZE};
use crate::structure::{SegmentId, Structure, StructureBuilder};
use crate::task::{BoxFuture, Io};
use crate::{Existing, MetadataSlot, MetadataUpdate, Updates};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, Reader};
use std::borrow::Cow;
use std::io::{Read, Seek};
use std::sync::Arc;

const SVG_NS: &[u8] = b"http://www.w3.org/2000/svg";
const C2PA_NS: &[u8] = b"http://c2pa.org/manifest";
const XMP_ID: &[u8] = b"W5M0MpCehiHzreSzNTczkc9d";
const XPACKET: &[u8] = b"xpacket";
const MANIFEST_OPEN: &[u8] = b"<c2pa:manifest>";
const MANIFEST_CLOSE: &[u8] = b"</c2pa:manifest>";
const NS_DECL: &[u8] = b" xmlns:c2pa=\"http://c2pa.org/manifest\"";

/// The name of the text that ends with the start tag of `<metadata>`.
const METADATA_OPEN: Name = Name::Id(1);

/// How many characters of base64 hold the 32 bytes that give a JUMBF its type.
const TYPE_CHARS: usize = 44;

/// The SVG container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct SvgHandler;

/// The SVG handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(SvgHandler)
}

/// The SVG container's [`ContainerHandler::id`].
pub const ID: &str = "svg";

impl ContainerHandler for SvgHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[
            FormatSpec::new("image/svg+xml", "svg").media_type_aliases(&["application/svg+xml"])
        ];
        F
    }

    // No `detect`: `<svg` near the start is not a signature, so SVG is found by
    // extension or media type only.

    fn parse_async(self: Arc<Self>, io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(parse_with(self, io))
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        src_structure: &'f Structure,
        mut io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move { plan(src_structure, &mut io, updates).await })
    }

    fn compose(&self, kind: SegmentKind, data: &[u8]) -> Result<Vec<u8>> {
        match kind {
            SegmentKind::Jumbf => {
                let text = base64::encode(data);
                let mut out = Vec::with_capacity(text.len() + 40);
                out.extend_from_slice(MANIFEST_OPEN);
                out.extend_from_slice(&text);
                out.extend_from_slice(MANIFEST_CLOSE);
                Ok(out)
            }
            SegmentKind::Xmp => {
                check_packet(data)?;
                Ok(data.to_vec())
            }
            _ => Err(Error::UnsupportedFormat),
        }
    }

    fn decode_entity(&self, kind: SegmentKind, stored: Vec<u8>) -> Result<Vec<u8>> {
        match kind {
            SegmentKind::Jumbf => base64::decode(&stored),
            _ => Ok(stored),
        }
    }

    fn encode_entity<'a>(&self, kind: SegmentKind, data: &'a [u8]) -> Result<Cow<'a, [u8]>> {
        match kind {
            SegmentKind::Jumbf => Ok(Cow::Owned(base64::encode(data))),
            _ => Ok(Cow::Borrowed(data)),
        }
    }
}

/// Parse an SVG into a validated structure.
pub fn parse<R: Read + Seek>(src: &mut R) -> Result<Structure> {
    handler().parse(src)
}

fn xml_error(e: impl std::fmt::Display) -> Error {
    Error::InvalidFormat(format!("SVG: XML is not well formed: {e}"))
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// Which of the elements that matter an element is, by where it is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Svg,
    Metadata,
    Manifest,
    Other,
}

/// A `<c2pa:manifest>` element: where it starts and the lengths of its three parts.
#[derive(Clone, Copy)]
struct Element {
    start: u64,
    header: u64,
    payload: u64,
    trailer: u64,
}

impl Element {
    fn end(&self) -> u64 {
        self.start + self.header + self.payload + self.trailer
    }
}

/// What the scan of a document found.
struct Scan {
    /// Where the root element's start tag ends.
    root_end: u64,
    /// Where the start tag of the first `<metadata>` of the root ends.
    metadata_end: Option<u64>,
    manifests: Vec<Element>,
    /// The xpackets, each from its begin to the end of its end.
    xmps: Vec<(u64, u64)>,
}

/// The namespace an element is in, as far as it matters.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Svg,
    C2pa,
    Other,
}

fn ns_of(bound: &ResolveResult<'_>) -> Ns {
    match bound {
        ResolveResult::Bound(n) if n.as_ref() == SVG_NS => Ns::Svg,
        ResolveResult::Bound(n) if n.as_ref() == C2PA_NS => Ns::C2pa,
        _ => Ns::Other,
    }
}

/// Whether `e` is the element called `local` in `want`. As for c2pa-rs, a name
/// written as `raw` counts too, whatever it is bound to.
fn named(e: &BytesStart<'_>, ns: Ns, want: Ns, local: &[u8], raw: &[u8]) -> bool {
    (ns == want && e.local_name().as_ref() == local) || e.name().as_ref() == raw
}

fn kind_at(depth: usize, path: &[Kind; 3], e: &BytesStart<'_>, ns: Ns) -> Kind {
    match depth {
        0 if named(e, ns, Ns::Svg, b"svg", b"svg") => Kind::Svg,
        1 if path[0] == Kind::Svg && named(e, ns, Ns::Svg, b"metadata", b"metadata") => {
            Kind::Metadata
        }
        2 if path[1] == Kind::Metadata && named(e, ns, Ns::C2pa, b"manifest", b"c2pa:manifest") => {
            Kind::Manifest
        }
        _ => Kind::Other,
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn scan(data: &[u8]) -> Result<Scan> {
    let mut reader = NsReader::from_reader(data);
    let mut depth = 0usize;
    let mut path = [Kind::Other; 3];
    let mut root_end = None;
    let mut metadata_end = None;
    let mut manifests: Vec<Element> = Vec::new();
    let mut open: Option<(u64, u64)> = None;
    let mut xmps = Vec::new();
    // Where an xpacket began and how deep, and how shallow the document has been
    // since. Until it ends we do not know whether it is an item, so the first
    // `<metadata>` inside it waits in `pending_metadata`.
    let mut begin: Option<(u64, usize)> = None;
    let mut low = usize::MAX;
    let mut pending_metadata = None;

    loop {
        let before = reader.buffer_position();
        let (bound, event) = reader.read_resolved_event().map_err(xml_error)?;
        let ns = ns_of(&bound);
        let after = reader.buffer_position();
        match event {
            Event::Start(e) => {
                let first_root = depth == 0 && root_end.is_none();
                let kind = if depth == 0 && !first_root {
                    Kind::Other
                } else {
                    kind_at(depth, &path, &e, ns)
                };
                if first_root {
                    if kind != Kind::Svg {
                        return Err(Error::InvalidFormat(
                            "SVG: the root element is not svg".into(),
                        ));
                    }
                    root_end = Some(after);
                }
                if depth == 1 && kind == Kind::Metadata {
                    if begin.is_some() {
                        pending_metadata.get_or_insert(after);
                    } else if metadata_end.is_none() {
                        metadata_end = Some(after);
                    }
                }
                if kind == Kind::Manifest {
                    open = Some((before, after));
                }
                if let Some(slot) = path.get_mut(depth) {
                    *slot = kind;
                }
                depth += 1;
            }
            Event::Empty(e) => {
                let first_root = depth == 0 && root_end.is_none();
                if first_root {
                    if kind_at(0, &path, &e, ns) != Kind::Svg {
                        return Err(Error::InvalidFormat(
                            "SVG: the root element is not svg".into(),
                        ));
                    }
                    root_end = Some(after);
                } else if kind_at(depth, &path, &e, ns) == Kind::Manifest {
                    manifests.push(Element {
                        start: before,
                        header: after - before,
                        payload: 0,
                        trailer: 0,
                    });
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| xml_error("an end tag with nothing open"))?;
                low = low.min(depth);
                if depth == 2 {
                    if let Some((start, header_end)) = open.take() {
                        manifests.push(Element {
                            start,
                            header: header_end - start,
                            payload: before - header_end,
                            trailer: after - before,
                        });
                    }
                }
            }
            Event::PI(pi) => {
                let content: &[u8] = pi.as_ref();
                if contains(content, XPACKET) {
                    if contains(content, XMP_ID) {
                        // Not inside a manifest, which would be the outer item, and
                        // not before the root element, which would hold the document.
                        if begin.is_none() && open.is_none() && root_end.is_some() {
                            begin = Some((before, depth));
                            low = depth;
                            pending_metadata = None;
                        }
                    } else if let Some((b, at)) = begin.take() {
                        // Only a packet that opens and closes at one depth and never
                        // closes anything outside itself can be replaced without
                        // changing how the rest of the document nests.
                        if depth == at && low >= at {
                            xmps.push((b, after));
                            // What is inside an item is part of it, not an item.
                            manifests.retain(|m| m.start < b);
                        } else if metadata_end.is_none() {
                            metadata_end = pending_metadata;
                        }
                        pending_metadata = None;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if open.is_some() {
        return Err(Error::InvalidFormat(
            "SVG: a c2pa:manifest element is not closed".into(),
        ));
    }
    // An xpacket that never ended is not an item.
    if metadata_end.is_none() {
        metadata_end = pending_metadata;
    }
    Ok(Scan {
        root_end: root_end.ok_or_else(|| Error::InvalidFormat("SVG: no root element".into()))?,
        metadata_end,
        manifests,
        xmps,
    })
}

/// What the plan needs to know about the root element, from its start tag.
struct Root {
    /// Where to insert an attribute: just before the `>`.
    attr_at: u64,
    has_c2pa_ns: bool,
    /// The prefix the root element is written with, `ns0` in `<ns0:svg>`.
    prefix: Option<String>,
    self_closing: bool,
}

fn root_info(header: &[u8]) -> Result<Root> {
    let mut reader = Reader::from_reader(header);
    loop {
        let event = reader.read_event().map_err(xml_error)?;
        let (e, self_closing) = match event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::Eof => return Err(xml_error("no root element")),
            _ => continue,
        };
        let has_c2pa_ns = e
            .attributes()
            .filter_map(|a| a.ok())
            .any(|a| a.key.as_ref() == b"xmlns:c2pa");
        let prefix = e
            .name()
            .prefix()
            .map(|p| String::from_utf8_lossy(p.into_inner()).into_owned());
        return Ok(Root {
            attr_at: (header.len() as u64).saturating_sub(1),
            has_c2pa_ns,
            prefix,
            self_closing,
        });
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// An element or packet the structure has a segment for.
#[derive(Clone, Copy)]
struct Span {
    xmp: bool,
    el: Element,
}

async fn parse_with(handler: Arc<dyn ContainerHandler>, mut io: Io) -> Result<Structure> {
    let total = io.len();
    if total > MAX_SEGMENT_SIZE {
        return Err(Error::LimitExceeded {
            what: "SVG size",
            limit: MAX_SEGMENT_SIZE,
        });
    }
    let data = io.read_vec(0, total as usize).await?;
    let sc = scan(&data)?;

    // Items do not overlap: what is inside one is part of it.
    let mut spans: Vec<Span> = sc
        .manifests
        .iter()
        .map(|&el| Span { xmp: false, el })
        .collect();
    spans.extend(sc.xmps.iter().map(|&(s, e)| Span {
        xmp: true,
        el: Element {
            start: s,
            header: 0,
            payload: e - s,
            trailer: 0,
        },
    }));
    spans.sort_by_key(|s| s.el.start);
    // The cut after `<metadata>` must not fall inside a span.
    let cut = sc
        .metadata_end
        .filter(|&m| !spans.iter().any(|s| s.el.start < m && m < s.el.end()));

    let mut b = StructureBuilder::new(handler, "image/svg+xml", total);
    b.push_span(
        SegmentKind::Header,
        Name::None,
        u32::try_from(sc.root_end).map_err(|_| Error::LimitExceeded {
            what: "SVG header size",
            limit: u32::MAX as u64,
        })?,
        0,
        0,
    )?;
    let mut at = sc.root_end;
    for s in &spans {
        gap(&mut b, at, s.el.start, cut)?;
        let (h, p, t) = (
            u32::try_from(s.el.header).map_err(|_| too_long())?,
            s.el.payload,
            u16::try_from(s.el.trailer).map_err(|_| too_long())?,
        );
        if s.xmp {
            b.push_item(SegmentKind::Xmp, Name::None, h, p, t)?;
        } else {
            let id = b.push_span(SegmentKind::Jumbf, Name::None, h, p, t)?;
            // An empty element holds no manifest.
            if p > 0 {
                let text = &data[(s.el.start + s.el.header) as usize..][..p as usize];
                let ty = base64_head(text).and_then(|head| superbox_type(&head));
                b.entity_of_segments_typed(SegmentKind::Jumbf, ty, &[id])?;
            }
        }
        at = s.el.end();
    }
    gap(&mut b, at, total, cut)?;
    b.finish()
}

fn too_long() -> Error {
    Error::LimitExceeded {
        what: "SVG element tag size",
        limit: u32::MAX as u64,
    }
}

/// The text from `from` to `to` as one segment, or two where `cut` falls in it.
fn gap(b: &mut StructureBuilder, from: u64, to: u64, cut: Option<u64>) -> Result<()> {
    if let Some(c) = cut.filter(|&c| from < c && c <= to) {
        b.push_span(SegmentKind::Other, METADATA_OPEN, 0, c - from, 0)?;
        if c < to {
            b.push_span(SegmentKind::Other, Name::None, 0, to - c, 0)?;
        }
    } else if from < to {
        b.push_span(SegmentKind::Other, Name::None, 0, to - from, 0)?;
    }
    Ok(())
}

/// The first bytes of the JUMBF `text` is the base64 of, enough to tell its type.
fn base64_head(text: &[u8]) -> Option<Vec<u8>> {
    let head: Vec<u8> = text
        .iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace())
        .take(TYPE_CHARS)
        .collect();
    base64::decode(&head).ok()
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Part of a run of text: kept source bytes or new ones.
enum Piece<'a> {
    Copy(ByteRange),
    Put(Cow<'a, [u8]>),
}

impl Piece<'_> {
    fn len(&self) -> u64 {
        match self {
            Piece::Copy(r) => r.len(),
            Piece::Put(d) => d.len() as u64,
        }
    }
}

/// Emit the run as one segment of `kind` and `name`, with its bytes in `role`.
fn flush<'a>(
    b: &mut PlanBuilder<'_, 'a>,
    run: &mut Vec<Piece<'a>>,
    kind: SegmentKind,
    name: Name,
    role: Role,
) -> Result<()> {
    let len: u64 = run.iter().map(Piece::len).sum();
    if len == 0 {
        run.clear();
        return Ok(());
    }
    let (header, payload) = if role == Role::Header {
        (
            u32::try_from(len).map_err(|_| Error::LimitExceeded {
                what: "SVG header size",
                limit: u32::MAX as u64,
            })?,
            0,
        )
    } else {
        (0, len)
    };
    let id = b.shape(kind, name, header, payload, 0)?;
    for piece in run.drain(..) {
        match piece {
            Piece::Copy(r) if !r.is_empty() => {
                b.copy(id, role, r);
            }
            Piece::Put(d) if !d.is_empty() => {
                b.put(id, role, d);
            }
            _ => {}
        }
    }
    Ok(())
}

fn put_manifest<'a>(b: &mut PlanBuilder<'_, 'a>, jumbf: &[u8]) -> Result<()> {
    let id = b.put_segment(
        SegmentKind::Jumbf,
        Name::None,
        MANIFEST_OPEN,
        base64::encode(jumbf),
        MANIFEST_CLOSE,
    )?;
    b.entity_typed(SegmentKind::Jumbf, superbox_type(jumbf), &[id])
}

fn put_xmp<'a>(b: &mut PlanBuilder<'_, 'a>, packet: &'a [u8]) -> Result<()> {
    let id = b.put_segment(SegmentKind::Xmp, Name::None, &[][..], packet, &[][..])?;
    b.entity_for(id)
}

/// Decide what the output contains, and describe how to write it.
async fn plan<'a>(src: &Structure, io: &mut Io, updates: &'a Updates) -> Result<WritePlan<'a>> {
    let segs = src.segments();
    let first = segs
        .first()
        .filter(|s| s.kind() == SegmentKind::Header)
        .ok_or_else(|| Error::InvalidFormat("SVG: structure has no root element".into()))?;
    let has_xmp = segs.iter().any(|s| s.kind() == SegmentKind::Xmp);
    let has_manifest = segs.iter().any(|s| s.kind() == SegmentKind::Jumbf);
    let has_open = segs.iter().any(|s| s.name() == METADATA_OPEN);

    if let MetadataUpdate::Set(d) = updates.xmp() {
        check_packet(d)?;
    }
    let sets_manifest = matches!(updates.jumbf(), MetadataUpdate::Set(_));
    if let MetadataUpdate::Set(d) = updates.jumbf() {
        if d.is_empty() {
            return Err(Error::InvalidFormat("SVG: JUMBF is empty".into()));
        }
    }
    let new_xmp = !has_xmp && matches!(updates.xmp(), MetadataUpdate::Set(_));
    let new_manifest = !has_manifest && sets_manifest;
    let insert = new_xmp || new_manifest;
    let wrap = insert && !has_open;

    // The root element's start tag, for where to declare the namespace and how to
    // write `<metadata>`.
    let root = if sets_manifest || wrap {
        let header = io.read_vec(0, first.extent().len() as usize).await?;
        Some(root_info(&header)?)
    } else {
        None
    };
    if wrap && root.as_ref().is_some_and(|r| r.self_closing) {
        return Err(Error::InvalidFormat(
            "SVG: the root element is empty, so there is nowhere to put metadata".into(),
        ));
    }

    let mut xmp = MetadataSlot::new(updates.xmp());
    let mut jumbf = MetadataSlot::new(updates.jumbf());
    let mut b = PlanBuilder::new(src);
    let mut run: Vec<Piece<'a>> = Vec::new();

    // The root's start tag, with the declaration of the C2PA namespace added if a
    // manifest is written and it is not there.
    let end = first.extent().len();
    match root.as_ref().filter(|r| sets_manifest && !r.has_c2pa_ns) {
        Some(r) => {
            run.push(Piece::Copy(ByteRange::new(0, r.attr_at)?));
            run.push(Piece::Put(Cow::Borrowed(NS_DECL)));
            run.push(Piece::Copy(ByteRange::new(r.attr_at, end - r.attr_at)?));
        }
        None => run.push(Piece::Copy(ByteRange::new(0, end)?)),
    }
    flush(
        &mut b,
        &mut run,
        SegmentKind::Header,
        Name::None,
        Role::Header,
    )?;

    let new_items = |b: &mut PlanBuilder<'_, 'a>,
                     xmp: &mut MetadataSlot<'a>,
                     jumbf: &mut MetadataSlot<'a>|
     -> Result<()> {
        if !has_xmp {
            if let Some(d) = xmp.pending() {
                put_xmp(b, d)?;
            }
        }
        if !has_manifest {
            if let Some(d) = jumbf.pending() {
                put_manifest(b, d)?;
            }
        }
        Ok(())
    };

    // No `<metadata>` to put them in: make one, right after the root's start tag.
    if wrap {
        let qname = match root.as_ref().and_then(|r| r.prefix.as_deref()) {
            Some(p) => format!("{p}:metadata"),
            None => "metadata".to_owned(),
        };
        run.push(Piece::Put(Cow::Owned(format!("<{qname}>").into_bytes())));
        flush(
            &mut b,
            &mut run,
            SegmentKind::Other,
            METADATA_OPEN,
            Role::Payload,
        )?;
        new_items(&mut b, &mut xmp, &mut jumbf)?;
        run.push(Piece::Put(Cow::Owned(format!("</{qname}>").into_bytes())));
    }

    for (i, seg) in segs.iter().enumerate().skip(1) {
        let id = SegmentId::from_index(i)?;
        match seg.kind() {
            SegmentKind::Xmp => match xmp.existing() {
                Existing::Keep => {
                    flush(
                        &mut b,
                        &mut run,
                        SegmentKind::Other,
                        Name::None,
                        Role::Payload,
                    )?;
                    b.copy_segment(id)?;
                }
                Existing::Replace(d) => {
                    flush(
                        &mut b,
                        &mut run,
                        SegmentKind::Other,
                        Name::None,
                        Role::Payload,
                    )?;
                    put_xmp(&mut b, d)?;
                }
                _ => {}
            },
            SegmentKind::Jumbf => match jumbf.existing() {
                Existing::Keep => {
                    flush(
                        &mut b,
                        &mut run,
                        SegmentKind::Other,
                        Name::None,
                        Role::Payload,
                    )?;
                    b.copy_segment(id)?;
                }
                Existing::Replace(d) => {
                    flush(
                        &mut b,
                        &mut run,
                        SegmentKind::Other,
                        Name::None,
                        Role::Payload,
                    )?;
                    put_manifest(&mut b, d)?;
                }
                _ => {}
            },
            _ => {
                run.push(Piece::Copy(seg.extent()));
                if seg.name() == METADATA_OPEN {
                    flush(
                        &mut b,
                        &mut run,
                        SegmentKind::Other,
                        METADATA_OPEN,
                        Role::Payload,
                    )?;
                    if insert {
                        new_items(&mut b, &mut xmp, &mut jumbf)?;
                    }
                }
            }
        }
    }
    flush(
        &mut b,
        &mut run,
        SegmentKind::Other,
        Name::None,
        Role::Payload,
    )?;
    b.carry_entities(|_| true);
    b.finish()
}

/// Whether `packet` is an xpacket that reads back as one XMP item: it begins and ends
/// with the packet's processing instructions, holds no other, and its elements nest.
fn check_packet(packet: &[u8]) -> Result<()> {
    let bad = |why: &str| Error::InvalidFormat(format!("SVG: XMP is not an xpacket: {why}"));
    if !packet.starts_with(b"<?xpacket") || !packet.ends_with(b"?>") {
        return Err(bad("it must begin and end with the xpacket instructions"));
    }
    let mut reader = Reader::from_reader(packet);
    let (mut depth, mut packets, mut first) = (0i64, 0, true);
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            Event::PI(pi) => {
                let content: &[u8] = pi.as_ref();
                if contains(content, XPACKET) {
                    if first && !contains(content, XMP_ID) {
                        return Err(bad("the first instruction is not the XMP packet's"));
                    }
                    first = false;
                    packets += 1;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        if depth < 0 {
            return Err(bad("an end tag has nothing to close"));
        }
    }
    if depth != 0 || packets != 2 {
        return Err(bad(
            "elements must nest and there must be one begin and one end",
        ));
    }
    Ok(())
}

/// Standard base64 with padding, which is how a manifest is written in SVG.
mod base64 {
    use crate::error::{Error, Result};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]);
                } else {
                    out.push(b'=');
                }
            }
        }
        out
    }

    /// Decode, ignoring whitespace, which an XML text may have.
    pub fn decode(text: &[u8]) -> Result<Vec<u8>> {
        let bad = |why: &str| Error::InvalidFormat(format!("SVG: manifest is not base64: {why}"));
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let (mut n, mut bits, mut pad) = (0u32, 0, 0);
        for &c in text.iter().filter(|c| !c.is_ascii_whitespace()) {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    pad += 1;
                    continue;
                }
                _ => return Err(bad("an invalid character")),
            };
            if pad > 0 {
                return Err(bad("data after padding"));
            }
            n = n << 6 | v as u32;
            bits += 6;
            if bits == 24 {
                out.extend_from_slice(&n.to_be_bytes()[1..]);
                (n, bits) = (0, 0);
            }
        }
        match (bits, pad) {
            (0, 0) => {}
            (12, 2) => out.push((n >> 4) as u8),
            (18, 1) => out.extend_from_slice(&[(n >> 10) as u8, (n >> 2) as u8]),
            _ => return Err(bad("the length or padding is wrong")),
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trips_every_length() {
            for n in 0..40usize {
                let data: Vec<u8> = (0..n as u8).map(|b| b.wrapping_mul(37)).collect();
                assert_eq!(decode(&encode(&data)).unwrap(), data);
            }
            assert_eq!(encode(b"Man"), b"TWFu");
            assert_eq!(encode(b"Ma"), b"TWE=");
            assert_eq!(encode(b"M"), b"TQ==");
        }

        #[test]
        fn rejects_what_is_not_base64() {
            for bad in [&b"TWE"[..], b"TW=u", b"T!Fu", b"TQ==TQ==", b"TQ="] {
                assert!(decode(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
            }
            assert_eq!(decode(b" TW\nFu ").unwrap(), b"Man");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Conformance;
    use crate::NullSink;
    use std::io::Cursor;

    const NS: &str = "http://www.w3.org/2000/svg";
    const PACKET: &str = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
        <x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
        xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"/></x:xmpmeta>\
        <?xpacket end=\"w\"?>";

    /// A JUMBF of `n` bytes that has a description box, so it has a type.
    fn jumbf(n: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
        v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
        v[4..8].copy_from_slice(b"jumb");
        v[8..12].copy_from_slice(&((n - 8) as u32).to_be_bytes());
        v[12..16].copy_from_slice(b"jumd");
        v[16..32].copy_from_slice(&[0xC2; 16]);
        v
    }

    fn b64(data: &[u8]) -> String {
        String::from_utf8(base64::encode(data)).unwrap()
    }

    fn plain() -> String {
        format!(r#"<svg xmlns="{NS}" width="10" height="10"><rect width="10" height="10"/></svg>"#)
    }

    fn with_manifest(text: &str) -> String {
        format!(
            r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest" width="10"><metadata><c2pa:manifest>{text}</c2pa:manifest></metadata><rect/></svg>"#
        )
    }

    fn parsed(svg: &str) -> Structure {
        parse(&mut Cursor::new(svg.as_bytes())).unwrap()
    }

    fn rewrite(svg: &str, u: &Updates) -> Result<String> {
        let s = parse(&mut Cursor::new(svg.as_bytes()))?;
        let mut out = Vec::new();
        s.write(&mut Cursor::new(svg.as_bytes()), &mut out, u, &mut NullSink)?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn store(svg: &str) -> Option<Vec<u8>> {
        parsed(svg).store(&mut Cursor::new(svg.as_bytes())).unwrap()
    }

    #[test]
    fn conforms() {
        let c = Conformance::new(handler()).jumbf(Some(jumbf(200)));
        c.check("plain", plain().as_bytes());
        c.check(
            "prefixed root",
            format!(r#"<ns0:svg xmlns:ns0="{NS}"><ns0:rect/></ns0:svg>"#).as_bytes(),
        );
        c.check(
            "with metadata",
            format!(r#"<svg xmlns="{NS}"><metadata><title>t</title></metadata><rect/></svg>"#)
                .as_bytes(),
        );
        c.check("manifest", with_manifest(&b64(&jumbf(100))).as_bytes());
        c.check(
            "manifest and xmp",
            format!(
                r#"<?xml version="1.0"?><!-- c --><!DOCTYPE svg><svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><metadata>
  {PACKET}
  <c2pa:manifest>{}</c2pa:manifest>
</metadata><g/></svg>
"#,
                b64(&jumbf(64))
            )
            .as_bytes(),
        );
        c.check(
            "xmp outside metadata",
            format!(r#"<svg xmlns="{NS}">{PACKET}<metadata/></svg>"#).as_bytes(),
        );
        c.check(
            "empty manifest element",
            format!(
                r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><metadata><c2pa:manifest/></metadata></svg>"#
            )
            .as_bytes(),
        );
    }

    #[test]
    fn a_manifest_is_a_header_a_payload_and_a_trailer() {
        let j = jumbf(100);
        let text = b64(&j);
        let svg = with_manifest(&text);
        let s = parsed(&svg);
        let seg = s
            .segments()
            .iter()
            .find(|g| g.kind() == SegmentKind::Jumbf)
            .unwrap();
        let at = |r: ByteRange| &svg.as_bytes()[r.offset() as usize..r.end() as usize];
        assert_eq!(at(seg.header()), MANIFEST_OPEN);
        assert_eq!(at(seg.payload()), text.as_bytes());
        assert_eq!(at(seg.trailer()), MANIFEST_CLOSE);

        // The entity is the text; the item is what it decodes to.
        let e = s.require_entity(SegmentKind::Jumbf).unwrap();
        assert_eq!(e.jumbf_type(), Some([0xC2; 16]));
        assert_eq!(
            s.read_entity(&mut Cursor::new(svg.as_bytes()), e).unwrap(),
            text.as_bytes()
        );
        assert_eq!(store(&svg).unwrap(), j);
    }

    #[test]
    fn the_text_before_metadata_ends_is_named() {
        let s = parsed(&format!(
            r#"<svg xmlns="{NS}"><defs/><metadata><a/></metadata></svg>"#
        ));
        let names: Vec<_> = s.segments().iter().map(|g| g.name()).collect();
        assert_eq!(names, [Name::None, METADATA_OPEN, Name::None]);
        assert_eq!(
            s.segments()[1].extent().len(),
            "<defs/><metadata>".len() as u64
        );
    }

    #[test]
    fn a_new_manifest_is_spliced_in_and_the_root_declares_the_namespace() {
        let j = jumbf(40);
        let out = rewrite(&plain(), &Updates::new().set_jumbf(j.clone())).unwrap();
        let want = format!(
            r#"<svg xmlns="{NS}" width="10" height="10" xmlns:c2pa="http://c2pa.org/manifest"><metadata><c2pa:manifest>{}</c2pa:manifest></metadata><rect width="10" height="10"/></svg>"#,
            b64(&j)
        );
        assert_eq!(out, want);
        assert_eq!(store(&out).unwrap(), j);
    }

    #[test]
    fn metadata_takes_the_roots_prefix_and_a_declared_namespace_is_not_repeated() {
        let svg = format!(
            r#"<ns0:svg xmlns:ns0="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><ns0:rect/></ns0:svg>"#
        );
        let out = rewrite(&svg, &Updates::new().set_jumbf(jumbf(40))).unwrap();
        assert!(out.contains("<ns0:metadata><c2pa:manifest>"), "{out}");
        assert!(out.contains("</c2pa:manifest></ns0:metadata>"), "{out}");
        assert_eq!(out.matches("xmlns:c2pa").count(), 1);
    }

    #[test]
    fn an_existing_manifest_is_replaced_where_it_is_and_removal_leaves_the_rest() {
        let svg = with_manifest(&b64(&jumbf(100)));
        let new = jumbf(60);
        let out = rewrite(&svg, &Updates::new().set_jumbf(new.clone())).unwrap();
        assert_eq!(out, with_manifest(&b64(&new)));

        let out = rewrite(&svg, &Updates::new().remove_jumbf()).unwrap();
        assert_eq!(
            out,
            format!(
                r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest" width="10"><metadata></metadata><rect/></svg>"#
            )
        );
        assert_eq!(rewrite(&svg, &Updates::new()).unwrap(), svg);
    }

    #[test]
    fn xmp_is_replaced_in_place_or_added_to_metadata() {
        let svg = format!(r#"<svg xmlns="{NS}"><metadata>{PACKET}</metadata></svg>"#);
        let other = PACKET.replace("rdf:RDF", "rdf:RDF a=\"1\"");
        let out = rewrite(&svg, &Updates::new().set_xmp(other.clone().into_bytes())).unwrap();
        assert_eq!(out, svg.replace(PACKET, &other));

        let out = rewrite(
            &plain(),
            &Updates::new().set_xmp(PACKET.as_bytes().to_vec()),
        )
        .unwrap();
        assert!(
            out.contains(&format!("<metadata>{PACKET}</metadata>")),
            "{out}"
        );
        let s = parsed(&out);
        assert_eq!(
            s.xmp(&mut Cursor::new(out.as_bytes())).unwrap().unwrap(),
            PACKET.as_bytes()
        );
    }

    #[test]
    fn an_xpacket_that_would_change_the_nesting_is_not_xmp() {
        // Found by fuzzing. Replacing a packet with unclosed elements in it would
        // change what the rest of the document is nested in, so it is left as text.
        let svg = format!(
            r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><metadata><?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta><?xpacket end="w"?><c2pa:manifest>QUJD</c2pa:manifest>"#
        );
        let s = parsed(&svg);
        assert!(s.entities(SegmentKind::Xmp).next().is_none());
        let c = Conformance::new(handler()).jumbf(Some(jumbf(64)));
        c.check("unbalanced xpacket", svg.as_bytes());

        // A packet that closes more than it opens is the same.
        let svg = format!(
            r#"<svg xmlns="{NS}"><g><metadata/><?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?></g><g><?xpacket end="w"?></g></svg>"#
        );
        assert!(parsed(&svg).entities(SegmentKind::Xmp).next().is_none());
        c.check("packet across elements", svg.as_bytes());
    }

    #[test]
    fn every_xpacket_is_xmp_and_what_is_inside_an_item_is_not_one() {
        // Found by fuzzing: removing the first xpacket must not reveal a second.
        let p = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><a/><?xpacket end="w"?>"#;
        let svg = format!(r#"<svg xmlns="{NS}">{p}<g/>{p}</svg>"#);
        let s = parsed(&svg);
        assert_eq!(s.entities(SegmentKind::Xmp).count(), 2);
        assert!(matches!(
            s.xmp(&mut Cursor::new(svg.as_bytes())),
            Err(Error::Ambiguous { count: 2, .. })
        ));
        let out = rewrite(&svg, &Updates::new().remove_xmp()).unwrap();
        assert_eq!(out, format!(r#"<svg xmlns="{NS}"><g/></svg>"#));
        assert_eq!(parsed(&out).entities(SegmentKind::Xmp).count(), 0);
        Conformance::new(handler()).check("two packets", svg.as_bytes());

        // A manifest inside a packet is part of the packet; a packet inside a
        // manifest is part of the manifest. Removing the outer one takes the inner.
        let svg = format!(
            r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><metadata><?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><c2pa:manifest>QUJD</c2pa:manifest><?xpacket end="w"?></metadata></svg>"#
        );
        let s = parsed(&svg);
        assert_eq!(s.entities(SegmentKind::Xmp).count(), 1);
        assert_eq!(s.entities(SegmentKind::Jumbf).count(), 0);
        let c = Conformance::new(handler()).jumbf(Some(jumbf(64)));
        c.check("manifest inside a packet", svg.as_bytes());

        let svg = format!(
            r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><metadata><c2pa:manifest><?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><?xpacket end="w"?></c2pa:manifest></metadata></svg>"#
        );
        let s = parsed(&svg);
        assert_eq!(s.entities(SegmentKind::Xmp).count(), 0);
        c.check("packet inside a manifest", svg.as_bytes());

        // A packet that holds the whole document, or the first metadata, hides them.
        let svg = format!(
            r#"<svg xmlns="{NS}"><?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><metadata><x/></metadata><?xpacket end="w"?><metadata/></svg>"#
        );
        c.check("metadata inside a packet", svg.as_bytes());
    }

    #[test]
    fn several_manifests_are_ambiguous_and_removal_takes_them_all() {
        let one = "<c2pa:manifest>QUJD</c2pa:manifest>";
        let svg = format!(
            r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><metadata>{one}{one}</metadata></svg>"#
        );
        let s = parsed(&svg);
        assert!(matches!(
            s.store(&mut Cursor::new(svg.as_bytes())),
            Err(Error::Ambiguous { count: 2, .. })
        ));
        let out = rewrite(&svg, &Updates::new().remove_jumbf()).unwrap();
        assert!(!out.contains("<c2pa:manifest"), "{out}");
        // A new one replaces the first and drops the rest.
        let out = rewrite(&svg, &Updates::new().set_jumbf(jumbf(40))).unwrap();
        assert_eq!(out.matches("<c2pa:manifest>").count(), 1);
    }

    #[test]
    fn a_manifest_that_is_not_in_the_roots_metadata_is_not_one() {
        let svg = format!(
            r#"<svg xmlns="{NS}" xmlns:c2pa="http://c2pa.org/manifest"><g><metadata><c2pa:manifest>QUJD</c2pa:manifest></metadata></g></svg>"#
        );
        assert!(store(&svg).is_none());
        assert!(parsed(&svg).require_entity(SegmentKind::Jumbf).is_err());
    }

    #[test]
    fn a_placeholder_is_filled_in_place() {
        let placeholder = jumbf(90);
        let svg = with_manifest(&b64(&placeholder));
        let s = parsed(&svg);
        let e = s.require_entity(SegmentKind::Jumbf).unwrap();
        let signed = jumbf(90).iter().map(|b| b ^ 0x55).collect::<Vec<_>>();
        let mut cur = Cursor::new(svg.clone().into_bytes());
        s.patch_exact(&mut cur, e, &signed).unwrap();
        let out = String::from_utf8(cur.into_inner()).unwrap();
        assert_eq!(out, with_manifest(&b64(&signed)));

        // The length that has to match is the stored one.
        let mut cur = Cursor::new(svg.into_bytes());
        assert!(s.patch_exact(&mut cur, e, &signed[..80]).is_err());
    }

    #[test]
    fn what_cannot_be_written_is_refused() {
        // Not SVG.
        assert!(parse(&mut Cursor::new(&b"<html><body/></html>"[..])).is_err());
        assert!(parse(&mut Cursor::new(&b"not xml"[..])).is_err());
        // A manifest that is never closed.
        let open = format!(r#"<svg xmlns="{NS}"><metadata><c2pa:manifest>QU"#);
        assert!(parse(&mut Cursor::new(open.as_bytes())).is_err());

        // XMP that is not an xpacket, and an empty store.
        assert!(rewrite(&plain(), &Updates::new().set_xmp(b"<x:xmpmeta/>".to_vec())).is_err());
        assert!(rewrite(&plain(), &Updates::new().set_jumbf(Vec::new())).is_err());

        // An empty root has nowhere to put metadata, but can be kept.
        let empty = format!(r#"<svg xmlns="{NS}"/>"#);
        assert!(rewrite(&empty, &Updates::new().set_jumbf(jumbf(40))).is_err());
        assert_eq!(rewrite(&empty, &Updates::new()).unwrap(), empty);
    }

    #[test]
    fn compose_is_what_a_write_inserts() {
        let j = jumbf(40);
        let element = handler().compose(SegmentKind::Jumbf, &j).unwrap();
        let out = rewrite(&plain(), &Updates::new().set_jumbf(j)).unwrap();
        assert!(contains(out.as_bytes(), &element));
        assert!(handler().compose(SegmentKind::Xmp, b"<x/>").is_err());
    }

    #[test]
    fn a_hash_skips_exactly_the_base64_text() {
        // What a C2PA data hash excludes: the payload of the manifest segment.
        let j = jumbf(100);
        let svg = with_manifest(&b64(&j));
        let s = parsed(&svg);
        let e = s.require_entity(SegmentKind::Jumbf).unwrap();
        let r = e.parts().next().unwrap().range();
        let before = &svg.as_bytes()[..r.offset() as usize];
        assert!(before.ends_with(MANIFEST_OPEN));
        let after = &svg.as_bytes()[r.end() as usize..];
        assert!(after.starts_with(MANIFEST_CLOSE));
        assert_eq!(r.len() as usize, base64::encode(&j).len());
    }
}
