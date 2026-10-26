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

//! Items: logical units defined by a format, assembled from extents.

use crate::range::ByteRange;
use crate::segment::Name;
use crate::structure::SegmentId;
use crate::transfer::TransferEncoding;
use std::borrow::Cow;

/// The specification that defines an [`Item`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemKind {
    /// XMP (ISO 16684-1).
    Xmp,
    /// A JUMBF superbox (ISO/IEC 19566-5), such as a C2PA manifest store.
    Jumbf,
    /// EXIF, whose data is TIFF.
    Exif,
    /// A self-contained file stored inside the asset, or a reference to one.
    EmbeddedAsset,
    /// Anything else the container exposes; see its native identifiers.
    Other,
}

impl ItemKind {
    /// A string for this kind: `"xmp"`, `"jumbf"`, `"exif"`, `"embedded_asset"`,
    /// `"other"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Xmp => "xmp",
            Self::Jumbf => "jumbf",
            Self::Exif => "exif",
            Self::EmbeddedAsset => "embedded_asset",
            Self::Other => "other",
        }
    }

    /// Whether items of this kind are metadata or assets.
    pub fn category(&self) -> ItemCategory {
        match self {
            Self::Xmp | Self::Jumbf | Self::Exif => ItemCategory::Metadata,
            Self::EmbeddedAsset => ItemCategory::Asset,
            Self::Other => ItemCategory::Other,
        }
    }
}

impl std::fmt::Display for ItemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Broad category of an [`ItemKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemCategory {
    /// Data about the asset.
    Metadata,
    /// A file in its own right.
    Asset,
    /// Neither, or unknown.
    Other,
}

impl ItemCategory {
    /// `"metadata"`, `"asset"` or `"other"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Asset => "asset",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for ItemCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an item's bytes are serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Encoding {
    /// XML (XMP is RDF/XML).
    Xml,
    /// TIFF structure (EXIF is always TIFF).
    Tiff,
    /// A JUMBF superbox tree.
    Jumbf,
    /// Plain ISO BMFF boxes that are not JUMBF superboxes.
    IsoBmff,
    /// A self-contained file; its media type, when known, is [`Item::media_type`].
    Asset,
    /// Not known to the container.
    Unknown,
}

impl Encoding {
    /// `"xml"`, `"tiff"`, `"jumbf"`, `"iso_bmff"`, `"asset"` or `"unknown"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Xml => "xml",
            Self::Tiff => "tiff",
            Self::Jumbf => "jumbf",
            Self::IsoBmff => "iso_bmff",
            Self::Asset => "asset",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Encoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A common name for a native item reference type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RelationKind {
    /// The item is a thumbnail of the target (HEIF `thmb`).
    Thumbnail,
    /// The item is an auxiliary image of the target, such as alpha or depth
    /// (HEIF `auxl`).
    Auxiliary,
    /// The item describes the target, as metadata does (HEIF `cdsc`).
    Describes,
    /// The item is derived from the target (HEIF `dimg`).
    Derived,
    /// Any other reference type; see [`Relation::native`].
    Other,
}

impl RelationKind {
    /// The kind of a HEIF `iref` reference type.
    pub fn from_native(native: [u8; 4]) -> Self {
        match &native {
            b"thmb" => Self::Thumbnail,
            b"auxl" => Self::Auxiliary,
            b"cdsc" => Self::Describes,
            b"dimg" => Self::Derived,
            _ => Self::Other,
        }
    }

    /// `"thumbnail"`, `"auxiliary"`, `"describes"`, `"derived"` or `"other"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
            Self::Auxiliary => "auxiliary",
            Self::Describes => "describes",
            Self::Derived => "derived",
            Self::Other => "other",
        }
    }
}

/// A reference from one item to another, keeping its native type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Relation {
    kind: RelationKind,
    native: [u8; 4],
    target: u64,
}

impl Relation {
    /// A reference of native type `native` (for example `thmb`) to the item whose
    /// [native id](Item::native_id) is `target`.
    pub fn new(native: [u8; 4], target: u64) -> Self {
        Self {
            kind: RelationKind::from_native(native),
            native,
            target,
        }
    }

    /// The common name of the reference type.
    pub fn kind(&self) -> RelationKind {
        self.kind
    }

    /// The reference type as the format writes it.
    pub fn native(&self) -> [u8; 4] {
        self.native
    }

    /// The [native id](Item::native_id) of the referenced item.
    pub fn target(&self) -> u64 {
        self.target
    }
}

/// What an item is, apart from where its bytes are. Used to declare items with
/// [`StructureBuilder::item`](crate::StructureBuilder::item) and
/// [`PlanBuilder::item`](crate::PlanBuilder::item); see [`Item`] for each field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInfo {
    kind: ItemKind,
    encoding: Encoding,
    transfer: TransferEncoding,
    label: Option<Cow<'static, str>>,
    media_type: Option<Cow<'static, str>>,
    jumbf_type: Option<[u8; 16]>,
    native_type: Option<Name>,
    native_id: Option<u64>,
    uri: Option<Cow<'static, str>>,
    relations: Vec<Relation>,
}

impl ItemInfo {
    /// An item of `kind` serialized as `encoding`, with nothing else known.
    pub fn new(kind: ItemKind, encoding: Encoding) -> Self {
        Self {
            kind,
            encoding,
            transfer: TransferEncoding::Identity,
            label: None,
            media_type: None,
            jumbf_type: None,
            native_type: None,
            native_id: None,
            uri: None,
            relations: Vec::new(),
        }
    }

    /// An XMP packet.
    pub fn xmp() -> Self {
        Self::new(ItemKind::Xmp, Encoding::Xml)
    }

    /// A JUMBF superbox.
    pub fn jumbf() -> Self {
        Self::new(ItemKind::Jumbf, Encoding::Jumbf)
    }

    /// EXIF (TIFF) data.
    pub fn exif() -> Self {
        Self::new(ItemKind::Exif, Encoding::Tiff)
    }

    /// Set the [transfer encoding](Item::transfer).
    pub fn transfer(mut self, transfer: TransferEncoding) -> Self {
        self.transfer = transfer;
        self
    }

    /// Set the [label](Item::label).
    pub fn label(mut self, label: impl Into<Cow<'static, str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the [media type](Item::media_type).
    pub fn media_type(mut self, media_type: impl Into<Cow<'static, str>>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    /// Set or clear the [JUMBF type](Item::jumbf_type).
    pub fn jumbf_type(mut self, jumbf_type: Option<[u8; 16]>) -> Self {
        self.jumbf_type = jumbf_type;
        self
    }

    /// Set the [native type](Item::native_type).
    pub fn native_type(mut self, native_type: Name) -> Self {
        self.native_type = Some(native_type);
        self
    }

    /// Set the [native id](Item::native_id).
    pub fn native_id(mut self, native_id: u64) -> Self {
        self.native_id = Some(native_id);
        self
    }

    /// Set the [URI](Item::uri).
    pub fn uri(mut self, uri: impl Into<Cow<'static, str>>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// Add a [relation](Item::relations).
    pub fn relation(mut self, relation: Relation) -> Self {
        self.relations.push(relation);
        self
    }
}

/// One contiguous run of an [`Item`]'s bytes, and the segment holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub(crate) range: ByteRange,
    pub(crate) segment: SegmentId,
}

impl Extent {
    /// The bytes.
    pub fn range(&self) -> ByteRange {
        self.range
    }

    /// The segment that contains them.
    pub fn segment(&self) -> SegmentId {
        self.segment
    }
}

/// A logical unit defined by the format (XMP, JUMBF, EXIF, an embedded file, ...),
/// assembled from one or more extents.
///
/// Extents are payload-only: framing that repeats per extent (JPEG XT headers, PNG
/// chunk headers) is not included. The common single-extent case does not allocate.
#[derive(Debug, Clone)]
pub struct Item {
    pub(crate) info: ItemInfo,
    pub(crate) first: Extent,
    pub(crate) rest: Vec<Extent>,
}

impl Item {
    /// The specification that defines the item.
    pub fn kind(&self) -> ItemKind {
        self.info.kind
    }

    /// How its bytes are serialized.
    pub fn encoding(&self) -> Encoding {
        self.info.encoding
    }

    /// How the stored bytes wrap the content: [`Identity`](TransferEncoding::Identity)
    /// unless the container stores the item as base64 text (SVG) or compressed
    /// (PDF streams, PNG `iTXt`, HEIF `mime` items). [`Structure::read_item`](crate::Structure::read_item) returns the stored
    /// bytes; [`Structure::read_content`](crate::Structure::read_content) the content.
    pub fn transfer(&self) -> &TransferEncoding {
        &self.info.transfer
    }

    /// Everything known about the item apart from its extents.
    pub fn info(&self) -> &ItemInfo {
        &self.info
    }

    /// Container-defined role of the item among others of its kind, for example
    /// the purpose of a BMFF C2PA box (`manifest`, `update`, `merkle`; see
    /// [`labels`](crate::labels)). `None` if the container does not distinguish
    /// them.
    pub fn label(&self) -> Option<&str> {
        self.info.label.as_deref()
    }

    /// Media type of the item's data, when the container records one: for example
    /// `image/jpeg` for a HEIF `jpeg` item, or the content type of a HEIF `mime`
    /// item. `None` when the container does not say, as with
    /// [`SubAsset::media_type`](crate::SubAsset::media_type).
    pub fn media_type(&self) -> Option<&str> {
        self.info.media_type.as_deref()
    }

    /// The type of a JUMBF superbox, from its description box, when the container
    /// recorded it while parsing: JPEG does, as it carries JUMBF of any type.
    /// `None` for other kinds, for containers that reserve an element for one
    /// kind of store (PNG `caBX`, the BMFF C2PA box), and for data that is not a
    /// superbox. [`Structure::jumbf_type`](crate::Structure::jumbf_type) reads it
    /// from the stream when needed. This crate attaches no meaning to any type.
    pub fn jumbf_type(&self) -> Option<[u8; 16]> {
        self.info.jumbf_type
    }

    /// The item's type as the format names it, for example a HEIF item type
    /// (`jpeg`, `mime`, `uri `, `hvc1`, `Exif`).
    pub fn native_type(&self) -> Option<Name> {
        self.info.native_type
    }

    /// The item's identifier in the format, for example a HEIF `item_ID`. The
    /// [target](Relation::target) of a relation is one of these.
    pub fn native_id(&self) -> Option<u64> {
        self.info.native_id
    }

    /// The URI that identifies the item's type, for a HEIF `uri ` item. It is an
    /// identifier only and is never fetched.
    pub fn uri(&self) -> Option<&str> {
        self.info.uri.as_deref()
    }

    /// References from this item to others, as the format records them.
    pub fn relations(&self) -> &[Relation] {
        &self.info.relations
    }

    /// Extents in logical order.
    pub fn extents(&self) -> impl Iterator<Item = &Extent> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }

    /// Number of extents.
    pub fn extent_count(&self) -> usize {
        1 + self.rest.len()
    }

    /// Total bytes across all extents (saturating).
    pub fn len(&self) -> u64 {
        self.extents()
            .fold(0u64, |acc, p| acc.saturating_add(p.range.len()))
    }

    /// True if all extents are empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether any extent lies in segment `id`.
    pub fn is_in(&self, id: SegmentId) -> bool {
        self.extents().any(|e| e.segment == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_map_to_categories_and_strings() {
        assert_eq!(ItemKind::Xmp.category(), ItemCategory::Metadata);
        assert_eq!(ItemKind::Jumbf.category(), ItemCategory::Metadata);
        assert_eq!(ItemKind::Exif.category(), ItemCategory::Metadata);
        assert_eq!(ItemKind::EmbeddedAsset.category(), ItemCategory::Asset);
        assert_eq!(ItemKind::Other.category(), ItemCategory::Other);
        assert_eq!(ItemKind::EmbeddedAsset.as_str(), "embedded_asset");
        assert_eq!(Encoding::IsoBmff.as_str(), "iso_bmff");
        assert_eq!(ItemInfo::exif().encoding, Encoding::Tiff);
    }

    #[test]
    fn relations_keep_their_native_type() {
        for (native, kind) in [
            (*b"thmb", RelationKind::Thumbnail),
            (*b"auxl", RelationKind::Auxiliary),
            (*b"cdsc", RelationKind::Describes),
            (*b"dimg", RelationKind::Derived),
            (*b"base", RelationKind::Other),
        ] {
            let r = Relation::new(native, 7);
            assert_eq!((r.kind(), r.native(), r.target()), (kind, native, 7));
        }
    }
}
