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

//! HEIF items found through the file-level `meta` box: EXIF, XMP, thumbnails and
//! embedded files (JPEG, MIME and URI-typed items).
//!
//! This is not a container handler. It is a parser for one part of ISO BMFF that
//! the [`bmff`](super::bmff) handler calls when it parses a file, and only that
//! handler uses it. The handler turns what it finds into [`Item`](crate::Item)s
//! with their native types, ids and relations, which the container-agnostic code
//! (`Structure::exif_info`, `Structure::embedded_thumbnail`,
//! `Structure::embedded_assets`) then reads without knowing the format.
//!
//! HEIF (HEIC, AVIF) stores images and metadata as items. `iinf` lists them with
//! a type, `iloc` says where their bytes are, `iref` links them (a `thmb`
//! reference from item A to item B says A is a thumbnail of B; `cdsc` says A
//! describes B) and `pitm` names the primary item. This module reads those boxes
//! from the bytes of one `meta` box and reports the items that matter here, with
//! the file ranges that hold their data and every reference from them. Everything
//! is bounded by the size of the buffer given.
//!
//! Items whose data is not plain bytes of this file are not reported: external
//! data references, and construction methods other than file offset (0) and
//! `idat` (1). A zero extent length means "to the end of the file" and is not
//! supported either.

use crate::transfer::MAX_LABEL_LEN;
use crate::{Error, Result, TransferEncoding};
use std::collections::{HashMap, HashSet};

const IINF: [u8; 4] = *b"iinf";
const INFE: [u8; 4] = *b"infe";
const ILOC: [u8; 4] = *b"iloc";
const IREF: [u8; 4] = *b"iref";
const IDAT: [u8; 4] = *b"idat";
const PITM: [u8; 4] = *b"pitm";
const THMB: [u8; 4] = *b"thmb";
/// Item type of EXIF data.
const EXIF_ITEM: [u8; 4] = *b"Exif";
/// Content type of a `mime` item that holds XMP.
const XMP_CONTENT_TYPE: &str = "application/rdf+xml";

/// Most boxes read from one level, and most items kept.
const MAX_BOXES: usize = 1 << 20;
/// Most extents kept for one item.
const MAX_EXTENTS: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `Exif` item: a 4-byte offset, then TIFF data.
    Exif,
    /// A `mime` item of content type `application/rdf+xml`.
    Xmp,
    /// A `jpeg` item, or another `mime` item with a valid content type: a file of
    /// a media type the format declares.
    Asset,
    /// Bytes the format does not declare a media type for: a `uri ` item, a `mime`
    /// item without a valid content type, or any other item that is the source of a
    /// `thmb` reference (a coded bitstream such as `hvc1`, which is not a file).
    Opaque,
}

/// An item of interest and where its data is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub kind: Kind,
    pub item_id: u32,
    pub item_type: [u8; 4],
    /// Media type of the item's data: `image/jpeg` for a `jpeg` item, the content
    /// type the file gives for a `mime` item, and `None` for anything else (coded
    /// bitstreams such as `hvc1` are not files of any media type).
    pub media_type: Option<String>,
    pub uri: Option<String>,
    /// How the item's bytes are wrapped (a `mime` item's `content_encoding`).
    pub transfer: TransferEncoding,
    /// `(reference type, to_item_ID)` of every `iref` reference from this item.
    pub refs: Vec<([u8; 4], u32)>,
    /// `(file offset, length)` of each extent, in order.
    pub extents: Vec<(u64, u64)>,
}

/// What one `meta` box says: the primary item, and the items of interest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Items {
    /// `item_ID` from `pitm`.
    pub primary: Option<u32>,
    pub found: Vec<Found>,
}

fn malformed(what: &str) -> Error {
    Error::InvalidFormat(format!("malformed HEIF {what} box"))
}

/// Big-endian reader that fails instead of running past the end.
struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.b.len() < n {
            return None;
        }
        let (head, rest) = self.b.split_at(n);
        self.b = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|v| v[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|v| u16::from_be_bytes([v[0], v[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
    }

    /// An unsigned value of 0, 4 or 8 bytes (0 reads as 0).
    fn uint(&mut self, n: u8) -> Option<u64> {
        match n {
            0 => Some(0),
            4 => self.u32().map(u64::from),
            8 => self
                .take(8)
                .map(|v| u64::from_be_bytes([v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]])),
            _ => None,
        }
    }

    /// A NUL-terminated string, without the NUL.
    fn cstr(&mut self) -> Option<&'a [u8]> {
        let n = self.b.iter().position(|&c| c == 0)?;
        let s = self.take(n)?;
        self.take(1)?;
        Some(s)
    }

    fn id(&mut self, wide: bool) -> Option<u32> {
        if wide {
            self.u32()
        } else {
            self.u16().map(u32::from)
        }
    }
}

/// The child boxes in `buf`: type and payload bounds.
fn boxes(buf: &[u8]) -> Result<Vec<([u8; 4], usize, usize)>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < buf.len() {
        if out.len() >= MAX_BOXES {
            return Err(Error::LimitExceeded {
                what: "HEIF boxes",
                limit: MAX_BOXES as u64,
            });
        }
        let mut r = Reader { b: &buf[at..] };
        let size32 = r.u32().ok_or_else(|| malformed("header"))?;
        let ty: [u8; 4] = r
            .take(4)
            .and_then(|t| t.try_into().ok())
            .ok_or_else(|| malformed("header"))?;
        let (size, header) = match size32 {
            0 => ((buf.len() - at) as u64, 8usize),
            1 => (r.uint(8).ok_or_else(|| malformed("header"))?, 16usize),
            n => (n as u64, 8usize),
        };
        let end = (at as u64)
            .checked_add(size)
            .filter(|&e| size >= header as u64 && e <= buf.len() as u64)
            .ok_or_else(|| malformed("size"))?;
        out.push((ty, at + header, end as usize));
        at = end as usize;
    }
    Ok(out)
}

/// An item's type and, when the file says, its content type.
struct Info {
    item_type: [u8; 4],
    media_type: Option<String>,
    uri: Option<String>,
    transfer: TransferEncoding,
}

/// The transfer encoding a `mime` item's `content_encoding` names. An empty or
/// missing one is [`Identity`](TransferEncoding::Identity); one this crate does not
/// know is kept as [`Unknown`](TransferEncoding::Unknown), with any byte that is not
/// printable ASCII replaced by `?` and the label cut to its maximum length.
fn transfer_of(label: &[u8]) -> TransferEncoding {
    match label {
        b"" => TransferEncoding::Identity,
        b"deflate" => TransferEncoding::Deflate,
        b"compress_zlib" => TransferEncoding::Zlib {
            png_predictor_columns: None,
        },
        b"gzip" => TransferEncoding::Gzip,
        b"br" => TransferEncoding::Brotli,
        other => {
            let text: String = other
                .iter()
                .take(MAX_LABEL_LEN)
                .map(|&c| {
                    if (0x20..0x7f).contains(&c) {
                        c as char
                    } else {
                        '?'
                    }
                })
                .collect();
            TransferEncoding::Unknown(text.into())
        }
    }
}

/// Longest media type accepted, in bytes.
const MAX_MEDIA_TYPE: usize = 255;

/// `text` if it looks like a media type: `type/subtype`, printable ASCII, bounded.
/// Anything else is dropped rather than reported, since the file is not trusted.
fn media_type_of(text: &[u8]) -> Option<String> {
    let ok = (3..=MAX_MEDIA_TYPE).contains(&text.len())
        && text.iter().all(|c| (0x20..0x7f).contains(c))
        && text.iter().filter(|&&c| c == b'/').count() == 1
        && text[0] != b'/'
        && text[text.len() - 1] != b'/';
    ok.then(|| String::from_utf8_lossy(text).trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// `(item id, type info)` for every `infe` entry that has a type (version 2, 3).
fn parse_iinf(p: &[u8]) -> Result<Vec<(u32, Info)>> {
    let mut r = Reader { b: p };
    let version = r.u8().ok_or_else(|| malformed("iinf"))?;
    r.take(3).ok_or_else(|| malformed("iinf"))?;
    // The entry count is advisory; the entries present are what is read.
    if version == 0 {
        r.u16().ok_or_else(|| malformed("iinf"))?;
    } else {
        r.u32().ok_or_else(|| malformed("iinf"))?;
    }
    let mut out = Vec::new();
    for (ty, from, to) in boxes(r.b)? {
        if ty != INFE {
            continue;
        }
        let mut e = Reader { b: &r.b[from..to] };
        let v = e.u8().ok_or_else(|| malformed("infe"))?;
        e.take(3).ok_or_else(|| malformed("infe"))?;
        if v < 2 {
            continue; // no item type
        }
        let id = e.id(v >= 3).ok_or_else(|| malformed("infe"))?;
        e.u16().ok_or_else(|| malformed("infe"))?; // protection index
        let item_type: [u8; 4] = e
            .take(4)
            .and_then(|t| t.try_into().ok())
            .ok_or_else(|| malformed("infe"))?;
        // A `mime` item names its content type after the item name. A string that is
        // missing or malformed leaves the type unknown; it does not fail the file.
        // The optional `content_encoding` after it says how the bytes are wrapped.
        let mut transfer = TransferEncoding::Identity;
        let media_type = if item_type == *b"jpeg" {
            Some("image/jpeg".to_owned())
        } else if item_type == *b"mime" {
            let content_type = e.cstr().and_then(|_name| e.cstr());
            if content_type.is_some() {
                transfer = e.cstr().map_or(TransferEncoding::Identity, transfer_of);
            }
            content_type.and_then(media_type_of)
        } else {
            None
        };
        let uri = if item_type == *b"uri " {
            let text = e
                .cstr()
                .and_then(|_name| e.cstr())
                .ok_or_else(|| malformed("infe URI"))?;
            if text.len() > 4096 {
                return Err(Error::LimitExceeded {
                    what: "HEIF item URI",
                    limit: 4096,
                });
            }
            Some(
                std::str::from_utf8(text)
                    .map_err(|_| malformed("infe URI"))?
                    .to_owned(),
            )
        } else {
            None
        };
        out.push((
            id,
            Info {
                item_type,
                media_type,
                uri,
                transfer,
            },
        ));
    }
    Ok(out)
}

/// Every reference in an `iref` box: `(from_item_ID, reference type, to_item_ID)`.
fn parse_iref(p: &[u8]) -> Result<Vec<(u32, [u8; 4], u32)>> {
    let mut r = Reader { b: p };
    let version = r.u8().ok_or_else(|| malformed("iref"))?;
    r.take(3).ok_or_else(|| malformed("iref"))?;
    let wide = version != 0;
    let mut out = Vec::new();
    for (ty, from, to) in boxes(r.b)? {
        let mut e = Reader { b: &r.b[from..to] };
        let bad = || malformed("iref reference");
        let source = e.id(wide).ok_or_else(bad)?;
        let count = e.u16().ok_or_else(bad)?;
        for _ in 0..count {
            if out.len() >= MAX_BOXES {
                return Err(Error::LimitExceeded {
                    what: "HEIF item references",
                    limit: MAX_BOXES as u64,
                });
            }
            out.push((source, ty, e.id(wide).ok_or_else(bad)?));
        }
    }
    Ok(out)
}

/// The primary item from a `pitm` box.
fn parse_pitm(p: &[u8]) -> Result<u32> {
    let mut r = Reader { b: p };
    let version = r.u8().ok_or_else(|| malformed("pitm"))?;
    r.take(3).ok_or_else(|| malformed("pitm"))?;
    r.id(version != 0).ok_or_else(|| malformed("pitm"))
}

/// An item to locate: what it is, its type info, and its references.
type Wanted = (Kind, Info, Vec<([u8; 4], u32)>);

fn parse_iloc(p: &[u8], wanted: &HashMap<u32, Wanted>, idat: Option<u64>) -> Result<Vec<Found>> {
    let bad = || malformed("iloc");
    let mut r = Reader { b: p };
    let version = r.u8().ok_or_else(bad)?;
    r.take(3).ok_or_else(bad)?;
    if version > 2 {
        return Ok(Vec::new());
    }
    let a = r.u8().ok_or_else(bad)?;
    let b = r.u8().ok_or_else(bad)?;
    let (offset_size, length_size, base_size) = (a >> 4, a & 15, b >> 4);
    let index_size = if version == 0 { 0 } else { b & 15 };
    if [offset_size, length_size, base_size, index_size]
        .iter()
        .any(|n| !matches!(n, 0 | 4 | 8))
    {
        return Err(bad());
    }
    let count = if version < 2 {
        r.u16().ok_or_else(bad)? as u32
    } else {
        r.u32().ok_or_else(bad)?
    };
    let per_extent = (index_size + offset_size + length_size) as usize;

    let mut out = Vec::new();
    for _ in 0..count {
        let id = r.id(version >= 2).ok_or_else(bad)?;
        let method = if version == 0 {
            0
        } else {
            (r.u16().ok_or_else(bad)? & 15) as u8
        };
        let data_ref = r.u16().ok_or_else(bad)?;
        let base = r.uint(base_size).ok_or_else(bad)?;
        let extent_count = r.u16().ok_or_else(bad)? as usize;

        let Some((kind, info, refs)) = wanted.get(&id) else {
            r.take(extent_count * per_extent).ok_or_else(bad)?;
            continue;
        };
        let origin = match (method, data_ref) {
            (0, 0) => Some(0),
            (1, 0) => idat,
            _ => None,
        };
        let mut extents = Vec::new();
        let mut ok = origin.is_some() && extent_count <= MAX_EXTENTS;
        for _ in 0..extent_count {
            if index_size > 0 {
                r.uint(index_size).ok_or_else(bad)?;
            }
            let offset = r.uint(offset_size).ok_or_else(bad)?;
            let len = r.uint(length_size).ok_or_else(bad)?;
            if !ok {
                continue;
            }
            let at = origin
                .and_then(|o| o.checked_add(base))
                .and_then(|v| v.checked_add(offset));
            match at {
                Some(at) if len > 0 && at.checked_add(len).is_some() => extents.push((at, len)),
                _ => ok = false,
            }
        }
        if ok && !extents.is_empty() {
            out.push(Found {
                kind: *kind,
                item_id: id,
                item_type: info.item_type,
                media_type: info.media_type.clone(),
                uri: info.uri.clone(),
                transfer: info.transfer.clone(),
                refs: refs.clone(),
                extents,
            });
        }
    }
    Ok(out)
}

/// Whether a `mime` item's content type says it holds XMP. Parameters after `;`
/// are ignored and the type is compared without regard to case.
fn is_xmp(media_type: Option<&str>) -> bool {
    media_type.is_some_and(|m| {
        m.split(';')
            .next()
            .is_some_and(|t| t.trim().eq_ignore_ascii_case(XMP_CONTENT_TYPE))
    })
}

/// The items of interest described by the `meta` box whose body (after any FullBox
/// header) is `meta`, and which starts at file offset `meta_at`, and its primary item.
pub(super) fn find(meta: &[u8], meta_at: u64) -> Result<Items> {
    let children = boxes(meta)?;
    let get = |ty: [u8; 4]| children.iter().find(|c| c.0 == ty);
    // Best effort: a malformed `pitm` only loses primary-item preference, not the items.
    let primary = get(PITM).and_then(|c| parse_pitm(&meta[c.1..c.2]).ok());
    let (Some(iinf), Some(iloc)) = (get(IINF), get(ILOC)) else {
        return Ok(Items {
            primary,
            found: Vec::new(),
        });
    };
    let types = parse_iinf(&meta[iinf.1..iinf.2])?;
    let refs = match get(IREF) {
        Some(c) => parse_iref(&meta[c.1..c.2])?,
        None => Vec::new(),
    };
    let thumbs: HashSet<u32> = refs.iter().filter(|r| r.1 == THMB).map(|r| r.0).collect();
    let mut wanted: HashMap<u32, Wanted> = HashMap::new();
    for (id, info) in types {
        let kind = if info.item_type == EXIF_ITEM {
            Kind::Exif
        } else if info.item_type == *b"mime" && is_xmp(info.media_type.as_deref()) {
            Kind::Xmp
        } else if info.item_type == *b"jpeg"
            || (info.item_type == *b"mime" && info.media_type.is_some())
        {
            Kind::Asset
        } else if matches!(&info.item_type, b"mime" | b"uri ") || thumbs.contains(&id) {
            Kind::Opaque
        } else {
            continue;
        };
        wanted.entry(id).or_insert((kind, info, Vec::new()));
    }
    if wanted.is_empty() {
        return Ok(Items {
            primary,
            found: Vec::new(),
        });
    }
    for (from, ty, to) in refs {
        if let Some(w) = wanted.get_mut(&from) {
            w.2.push((ty, to));
        }
    }
    let idat = get(IDAT).map(|c| meta_at + c.1 as u64);
    Ok(Items {
        primary,
        found: parse_iloc(&meta[iloc.1..iloc.2], &wanted, idat)?,
    })
}

/// `extents` without their first `n` bytes.
pub(super) fn skip_bytes(extents: &[(u64, u64)], mut n: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::with_capacity(extents.len());
    for &(at, len) in extents {
        if n >= len {
            n -= len;
        } else {
            out.push((at + n, len - n));
            n = 0;
        }
    }
    out
}

/// Builders for the boxes the tests need.
#[cfg(test)]
pub(super) mod build {
    pub fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    pub fn full(ty: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![version, 0, 0, 0];
        p.extend_from_slice(body);
        bx(ty, &p)
    }

    pub fn infe(id: u16, ty: &[u8; 4]) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(ty);
        b.push(0); // empty name
        full(b"infe", 2, &b)
    }

    /// An `infe` for a `mime` item. `content_type` is written as given, with its
    /// NUL unless it ends in one already.
    pub fn infe_mime(id: u16, content_type: &[u8]) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(b"mime");
        b.push(0); // empty name
        b.extend_from_slice(content_type);
        if content_type.last() != Some(&0) {
            b.push(0);
        }
        full(b"infe", 2, &b)
    }

    /// An `infe` for a `mime` item with a `content_encoding`.
    pub fn infe_mime_encoded(id: u16, content_type: &[u8], encoding: &[u8]) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(b"mime\0");
        b.extend_from_slice(content_type);
        b.push(0);
        b.extend_from_slice(encoding);
        b.push(0);
        full(b"infe", 2, &b)
    }

    pub fn iinf(items: &[Vec<u8>]) -> Vec<u8> {
        let mut b = (items.len() as u16).to_be_bytes().to_vec();
        for i in items {
            b.extend_from_slice(i);
        }
        full(b"iinf", 0, &b)
    }

    /// iloc version 0, 4-byte offsets and lengths, no base offset.
    pub fn iloc0(items: &[(u16, &[(u32, u32)])]) -> Vec<u8> {
        let mut b = vec![0x44, 0x00];
        b.extend_from_slice(&(items.len() as u16).to_be_bytes());
        for (id, extents) in items {
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&[0, 0]); // data reference index
            b.extend_from_slice(&(extents.len() as u16).to_be_bytes());
            for (o, l) in *extents {
                b.extend_from_slice(&o.to_be_bytes());
                b.extend_from_slice(&l.to_be_bytes());
            }
        }
        full(b"iloc", 0, &b)
    }

    pub fn thmb(from: u16, to: u16) -> Vec<u8> {
        iref(&[(b"thmb", from, &[to])])
    }

    /// An `iref` (version 0, 16-bit ids) with one reference box per
    /// `(type, from, to...)`.
    pub fn iref(refs: &[(&[u8; 4], u16, &[u16])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (ty, from, to) in refs {
            let mut b = from.to_be_bytes().to_vec();
            b.extend_from_slice(&(to.len() as u16).to_be_bytes());
            for t in *to {
                b.extend_from_slice(&t.to_be_bytes());
            }
            body.extend(bx(ty, &b));
        }
        full(b"iref", 0, &body)
    }

    /// An `iref` version 1 (32-bit ids).
    pub fn iref1(refs: &[(&[u8; 4], u32, &[u32])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (ty, from, to) in refs {
            let mut b = from.to_be_bytes().to_vec();
            b.extend_from_slice(&(to.len() as u16).to_be_bytes());
            for t in *to {
                b.extend_from_slice(&t.to_be_bytes());
            }
            body.extend(bx(ty, &b));
        }
        full(b"iref", 1, &body)
    }

    /// A `pitm` (version 0) naming the primary item.
    pub fn pitm(id: u16) -> Vec<u8> {
        full(b"pitm", 0, &id.to_be_bytes())
    }

    /// An `infe` for a `uri ` item.
    pub fn infe_uri(id: u16, uri: &str) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(b"uri ");
        b.push(0); // empty name
        b.extend_from_slice(uri.as_bytes());
        b.push(0);
        full(b"infe", 2, &b)
    }

    pub fn meta(children: &[Vec<u8>]) -> Vec<u8> {
        children.concat()
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    #[test]
    fn finds_exif_and_thumbnail_items() {
        let m = meta(&[
            iinf(&[infe(1, b"hvc1"), infe(2, b"Exif"), infe(3, b"jpeg")]),
            thmb(3, 1),
            iloc0(&[
                (1, &[(900, 50)]),
                (2, &[(100, 20), (200, 8)]),
                (3, &[(500, 40)]),
            ]),
        ]);
        let found = find(&m, 0).unwrap().found;
        assert_eq!(
            found,
            vec![
                Found {
                    kind: Kind::Exif,
                    item_id: 2,
                    item_type: *b"Exif",
                    media_type: None,
                    uri: None,
                    transfer: TransferEncoding::Identity,
                    refs: vec![],
                    extents: vec![(100, 20), (200, 8)]
                },
                Found {
                    kind: Kind::Asset,
                    item_id: 3,
                    item_type: *b"jpeg",
                    media_type: Some("image/jpeg".into()),
                    uri: None,
                    transfer: TransferEncoding::Identity,
                    refs: vec![(*b"thmb", 1)],
                    extents: vec![(500, 40)]
                },
            ]
        );
    }

    #[test]
    fn a_content_encoding_names_the_transfer_and_keeps_the_classification() {
        let mime = infe_mime_encoded;
        let zlib = TransferEncoding::Zlib {
            png_predictor_columns: None,
        };
        for (content_type, encoding, kind, transfer) in [
            (
                &b"application/rdf+xml"[..],
                &b""[..],
                Kind::Xmp,
                TransferEncoding::Identity,
            ),
            (
                b"application/rdf+xml",
                b"deflate",
                Kind::Xmp,
                TransferEncoding::Deflate,
            ),
            (b"image/png", b"compress_zlib", Kind::Asset, zlib),
            (b"image/png", b"gzip", Kind::Asset, TransferEncoding::Gzip),
            (b"nonsense", b"br", Kind::Opaque, TransferEncoding::Brotli),
            (
                b"image/png",
                b"x-lzma",
                Kind::Asset,
                TransferEncoding::unknown("x-lzma").unwrap(),
            ),
            (
                b"image/png",
                b"a\x01b",
                Kind::Asset,
                TransferEncoding::unknown("a?b").unwrap(),
            ),
        ] {
            let m = meta(&[
                iinf(&[infe(1, b"hvc1"), mime(3, content_type, encoding)]),
                iloc0(&[(3, &[(500, 40)])]),
            ]);
            let found = find(&m, 0).unwrap().found;
            assert_eq!((found[0].kind, &found[0].transfer), (kind, &transfer));
        }
        // A missing content encoding is none.
        let m = meta(&[
            iinf(&[infe_mime(3, b"image/png")]),
            iloc0(&[(3, &[(500, 40)])]),
        ]);
        assert_eq!(
            find(&m, 0).unwrap().found[0].transfer,
            TransferEncoding::Identity
        );
    }

    #[test]
    fn a_malformed_primary_item_box_does_not_hide_the_items() {
        let m = meta(&[
            full(b"pitm", 1, &[0, 1]), // version 1 needs a 4-byte id
            iinf(&[infe(1, b"hvc1"), infe(2, b"Exif")]),
            iloc0(&[(2, &[(100, 20)])]),
        ]);
        let items = find(&m, 0).unwrap();
        assert_eq!(items.primary, None);
        assert_eq!(items.found.len(), 1);
        assert_eq!(items.found[0].kind, Kind::Exif);
    }

    #[test]
    fn every_reference_and_the_primary_item_are_read() {
        let m = meta(&[
            pitm(1),
            iinf(&[
                infe(1, b"hvc1"),
                infe(2, b"Exif"),
                infe(3, b"hvc1"),
                infe_mime(4, b"application/rdf+xml"),
                infe(5, b"av01"),
                infe_uri(6, "urn:example:x"),
            ]),
            iref(&[
                (b"cdsc", 2, &[1]),
                (b"thmb", 3, &[1]),
                (b"cdsc", 4, &[1, 3]),
                (b"auxl", 5, &[1]),
                (b"dimg", 1, &[3]),
            ]),
            iloc0(&[
                (1, &[(900, 50)]),
                (2, &[(100, 20)]),
                (3, &[(300, 30)]),
                (4, &[(400, 10)]),
                (5, &[(600, 10)]),
                (6, &[(700, 10)]),
            ]),
        ]);
        let items = find(&m, 0).unwrap();
        assert_eq!(items.primary, Some(1));
        let got: Vec<_> = items
            .found
            .iter()
            .map(|f| (f.item_id, f.kind, f.refs.clone(), f.uri.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (2, Kind::Exif, vec![(*b"cdsc", 1)], None),
                (3, Kind::Opaque, vec![(*b"thmb", 1)], None),
                (4, Kind::Xmp, vec![(*b"cdsc", 1), (*b"cdsc", 3)], None),
                (6, Kind::Opaque, vec![], Some("urn:example:x".into())),
            ],
            "items 1 and 5 are coded images that are not thumbnails"
        );

        // Version 1 uses 32-bit ids.
        let m = meta(&[
            iinf(&[infe(1, b"hvc1"), infe(3, b"av01")]),
            iref1(&[(b"thmb", 3, &[1])]),
            iloc0(&[(3, &[(300, 30)])]),
        ]);
        let items = find(&m, 0).unwrap();
        assert_eq!(items.primary, None);
        assert_eq!(items.found[0].refs, vec![(*b"thmb", 1)]);
        assert_eq!(items.found[0].kind, Kind::Opaque);
    }

    #[test]
    fn a_mime_item_reports_its_content_type() {
        let m = meta(&[
            iinf(&[infe(1, b"hvc1"), infe_mime(3, b"image/png")]),
            thmb(3, 1),
            iloc0(&[(3, &[(500, 40)])]),
        ]);
        let found = find(&m, 0).unwrap().found;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].item_type, *b"mime");
        assert_eq!(found[0].media_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn a_content_type_that_is_not_one_leaves_the_item_found_but_untyped() {
        let long = vec![b'a'; 300];
        let long = [&long[..], b"/b"].concat();
        let bad: [&[u8]; 7] = [
            b"",
            b"png",
            b"image/",
            b"/png",
            b"a/b/c",
            b"image/\x01png",
            &long,
        ];
        for ct in bad {
            let m = meta(&[
                iinf(&[infe(1, b"hvc1"), infe_mime(3, ct)]),
                thmb(3, 1),
                iloc0(&[(3, &[(500, 40)])]),
            ]);
            let found = find(&m, 0).unwrap().found;
            assert_eq!(found.len(), 1, "{ct:?}");
            assert_eq!(found[0].media_type, None, "{ct:?}");
        }

        // A name and content type that run off the end of the entry.
        let mut cut = infe_mime(3, b"image/png");
        cut.truncate(cut.len() - 4);
        let n = cut.len() as u32;
        cut[..4].copy_from_slice(&n.to_be_bytes());
        let m = meta(&[
            iinf(&[infe(1, b"hvc1"), cut]),
            thmb(3, 1),
            iloc0(&[(3, &[(500, 40)])]),
        ]);
        assert_eq!(find(&m, 0).unwrap().found[0].media_type, None);
    }

    #[test]
    fn idat_items_are_located_in_the_file() {
        // construction_method 1: offsets are relative to the idat payload.
        let mut b = vec![0x44, 0x00];
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&2u16.to_be_bytes()); // id
        b.extend_from_slice(&1u16.to_be_bytes()); // method 1
        b.extend_from_slice(&[0, 0]); // data reference
        b.extend_from_slice(&1u16.to_be_bytes()); // extents
        b.extend_from_slice(&4u32.to_be_bytes());
        b.extend_from_slice(&10u32.to_be_bytes());
        let pre = meta(&[iinf(&[infe(2, b"Exif")]), full(b"iloc", 1, &b)]);
        let m = meta(&[pre.clone(), bx(b"idat", &[0u8; 32])]);
        let found = find(&m, 1000).unwrap().found;
        // idat payload starts after the boxes before it and its 8-byte header.
        let at = 1000 + pre.len() as u64 + 8 + 4;
        assert_eq!(found[0].extents, vec![(at, 10)]);

        // Without an idat box the item cannot be placed.
        assert!(find(&pre, 1000).unwrap().found.is_empty());
    }

    #[test]
    fn unusable_items_are_left_out() {
        // A data reference index other than 0 means data elsewhere.
        let mut m = iloc0(&[(2, &[(100, 20)])]);
        m[8 + 4 + 2 + 2 + 2 + 1] = 1; // data reference index, low byte
        let with_ref = meta(&[iinf(&[infe(2, b"Exif")]), m]);
        assert!(find(&with_ref, 0).unwrap().found.is_empty());

        // A zero length means "to the end" and is not supported.
        assert!(find(&meta_zero_len(), 0).unwrap().found.is_empty());

        // No items of interest, or no iloc.
        let none = meta(&[iinf(&[infe(1, b"hvc1")]), iloc0(&[(1, &[(0, 4)])])]);
        assert!(find(&none, 0).unwrap().found.is_empty());
        assert!(find(&iinf(&[infe(2, b"Exif")]), 0)
            .unwrap()
            .found
            .is_empty());
    }

    fn meta_zero_len() -> Vec<u8> {
        meta(&[iinf(&[infe(2, b"Exif")]), iloc0(&[(2, &[(100, 0)])])])
    }

    #[test]
    fn malformed_boxes_are_errors_not_panics() {
        let good = meta(&[iinf(&[infe(2, b"Exif")]), iloc0(&[(2, &[(100, 20)])])]);
        for cut in 0..good.len() {
            // Every truncation either parses (cut on a box edge) or errors.
            let _ = find(&good[..cut], 0);
        }
        // A box that claims more than the buffer holds.
        let mut huge = good.clone();
        huge[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(find(&huge, 0).is_err());
        // An iloc with a size nibble that is not 0, 4 or 8.
        let mut b = iloc0(&[(2, &[(100, 20)])]);
        b[12] = 0x34;
        assert!(find(&meta(&[iinf(&[infe(2, b"Exif")]), b]), 0).is_err());
    }

    #[test]
    fn skipping_bytes_spans_extents() {
        let e = [(10, 4), (50, 6)];
        assert_eq!(skip_bytes(&e, 0), e);
        assert_eq!(skip_bytes(&e, 3), vec![(13, 1), (50, 6)]);
        assert_eq!(skip_bytes(&e, 4), vec![(50, 6)]);
        assert_eq!(skip_bytes(&e, 7), vec![(53, 3)]);
        assert!(skip_bytes(&e, 10).is_empty());
        assert!(skip_bytes(&e, 99).is_empty());
    }
}
