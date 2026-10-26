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

//! Documents built for the PDF tests, and a stand-in for the 0.1 `Asset` they were
//! written against.

#![allow(dead_code)]

use asset_io::{
    Error, HandlerRegistry, ItemKind, NullSink, Segment, SegmentKind, Structure, Updates,
};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

/// A parsed document and the stream it came from.
pub struct Asset<R> {
    pub source: R,
    pub structure: Structure,
}

impl<R: Read + Seek> Asset<R> {
    pub fn from_source(mut source: R) -> asset_io::Result<Self> {
        let structure = HandlerRegistry::builtin().parse("pdf", &mut source)?;
        Ok(Self { source, structure })
    }

    pub fn from_file(file: R) -> asset_io::Result<Self> {
        Self::from_source(file)
    }

    pub fn structure(&self) -> &Structure {
        &self.structure
    }

    pub fn xmp(&mut self) -> asset_io::Result<Option<Vec<u8>>> {
        self.structure.xmp(&mut self.source)
    }

    pub fn jumbf(&mut self) -> asset_io::Result<Option<Vec<u8>>> {
        self.structure.store(&mut self.source)
    }

    /// Write with `updates`, returning the structure of the output.
    pub fn write<W: Write>(
        &mut self,
        out: &mut W,
        updates: &Updates,
    ) -> asset_io::Result<Structure> {
        self.structure
            .write(&mut self.source, out, updates, &mut NullSink)
    }

    /// Write, delivering every output byte to `sink`.
    pub fn write_with_sink<W: Write>(
        &mut self,
        out: &mut W,
        updates: &Updates,
        sink: &mut dyn asset_io::Sink,
    ) -> asset_io::Result<Structure> {
        self.structure.write(&mut self.source, out, updates, sink)
    }
}

impl<R: Read + Write + Seek> Asset<R> {
    /// Overwrite the payload of the `kind` store in this document, in place.
    pub fn update_segment_in_place(
        &mut self,
        kind: ItemKind,
        data: Vec<u8>,
    ) -> asset_io::Result<()> {
        patch_kind(&self.structure, &mut self.source, kind, data)
    }
}

/// Overwrite, in `out`, the bytes of the item of `kind`, which must be the same length.
pub fn patch_kind<W: Read + Write + Seek>(
    structure: &Structure,
    out: &mut W,
    kind: ItemKind,
    data: Vec<u8>,
) -> asset_io::Result<()> {
    let item = structure.require_item(kind).map_err(|_| Error::NotFound {
        what: "metadata item",
    })?;
    structure.patch_exact(out, item, &data)
}

/// What the 0.1 `Structure` called overwriting a store in place.
pub trait UpdateSegment {
    fn update_segment<W: Read + Write + Seek>(
        &self,
        out: &mut W,
        kind: ItemKind,
        data: Vec<u8>,
    ) -> asset_io::Result<()>;
}

impl UpdateSegment for Structure {
    fn update_segment<W: Read + Write + Seek>(
        &self,
        out: &mut W,
        kind: ItemKind,
        data: Vec<u8>,
    ) -> asset_io::Result<()> {
        patch_kind(self, out, kind, data)
    }
}

/// Where the payload of the `kind` store is.
pub fn payload_range(structure: &Structure, kind: ItemKind) -> Location {
    let range = structure
        .require_item(kind)
        .unwrap()
        .extents()
        .next()
        .unwrap()
        .range();
    Location {
        offset: range.offset(),
        size: range.len(),
    }
}

/// The two structures describe the same layout: segments, items and size.
pub fn same_layout(expected: &Structure, actual: &Structure) {
    assert_eq!(expected.total_size(), actual.total_size());
    let layout = |s: &Structure| {
        let mut items: Vec<_> = s
            .items()
            .map(|e| {
                (
                    e.kind(),
                    e.extents()
                        .map(|p| (p.segment().index(), p.range()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        items.sort_by_key(|(k, parts)| (format!("{k:?}"), parts.first().map(|p| p.1.offset())));
        (
            s.segments()
                .iter()
                .map(|g| format!("{g:?}"))
                .collect::<Vec<_>>(),
            format!("{items:?}"),
        )
    };
    assert_eq!(layout(expected), layout(actual));
}

pub struct Location {
    pub offset: u64,
    pub size: u64,
}

impl Location {
    pub fn end_offset(&self) -> u64 {
        self.offset + self.size
    }
}

pub trait SegmentExt {
    fn location(&self) -> Location;
    fn is_type(&self, kind: SegmentKind) -> bool;
}

impl SegmentExt for Segment {
    fn location(&self) -> Location {
        Location {
            offset: self.range().offset(),
            size: self.range().len(),
        }
    }

    fn is_type(&self, kind: SegmentKind) -> bool {
        self.kind() == kind
    }
}

/// How many bytes a source has been asked for.
pub struct Counted<R> {
    pub source: R,
    pub read: std::rc::Rc<std::cell::Cell<usize>>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.source.read(bytes)?;
        self.read.set(self.read.get() + n);
        Ok(n)
    }
}

impl<R: Seek> Seek for Counted<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.source.seek(position)
    }
}

pub struct Pdf {
    pub bytes: Vec<u8>,
    pub entries: BTreeMap<u32, (u64, u16, bool)>,
    pub highest_id: u32,
}

impl Pdf {
    pub fn new() -> Self {
        Self {
            bytes: b"%PDF-1.7\n".to_vec(),
            entries: BTreeMap::new(),
            highest_id: 0,
        }
    }

    pub fn object(&mut self, id: u32, generation: u16, body: &[u8]) {
        self.highest_id = self.highest_id.max(id);
        self.entries
            .insert(id, (self.bytes.len() as u64, generation, true));
        self.bytes
            .extend_from_slice(format!("{id} {generation} obj\n").as_bytes());
        self.bytes.extend_from_slice(body);
        self.bytes.extend_from_slice(b"\nendobj\n");
    }

    pub fn stream(&mut self, id: u32, dict: &str, data: &[u8]) {
        let mut body = format!("<< {dict} >>\nstream\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.object(id, 0, &body);
    }

    pub fn finish(&mut self, extra: &str) -> u64 {
        let offset = self.bytes.len() as u64;
        let size = self.highest_id + 1;
        self.bytes
            .extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        for (id, &(offset, generation, used)) in &self.entries {
            self.bytes.extend_from_slice(
                format!(
                    "{id} 1\n{offset:010} {generation:05} {} \n",
                    if used { 'n' } else { 'f' }
                )
                .as_bytes(),
            );
        }
        self.bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} {extra} >>\nstartxref\n{offset}\n%%EOF\n").as_bytes(),
        );
        offset
    }

    pub fn finish_stream(
        &mut self,
        extra: &str,
        compressed: &[(u32, u32, u16)],
        predictor: bool,
        sync_flush: bool,
    ) -> u64 {
        let id = self.highest_id + 1;
        let offset = self.bytes.len() as u64;
        let mut rows = Vec::new();
        for number in 0..=id {
            let (kind, location, index) = if number == id {
                (1, offset as u32, 0)
            } else if let Some(&(_, container, index)) =
                compressed.iter().find(|&&(object, _, _)| object == number)
            {
                (2, container, index)
            } else if let Some(&(location, generation, in_use)) = self.entries.get(&number) {
                (u8::from(in_use), location as u32, generation)
            } else {
                (0, 0, 0)
            };
            rows.push(kind);
            rows.extend_from_slice(&location.to_be_bytes());
            rows.extend_from_slice(&index.to_be_bytes());
        }
        let rows = if predictor {
            let mut predicted = Vec::new();
            let mut previous = [0u8; 7];
            for row in rows.chunks(7) {
                predicted.push(2);
                predicted.extend(row.iter().zip(previous).map(|(&a, b)| a.wrapping_sub(b)));
                previous.copy_from_slice(row);
            }
            predicted
        } else {
            rows
        };
        let encoded = if sync_flush {
            let mut encoder = flate2::Compress::new(flate2::Compression::default(), true);
            let mut output = vec![0; rows.len() * 2 + 128];
            encoder
                .compress(&rows, &mut output, flate2::FlushCompress::Sync)
                .unwrap();
            assert_eq!(encoder.total_in(), rows.len() as u64);
            output.truncate(encoder.total_out() as usize);
            output
        } else {
            deflate(&rows)
        };
        let params = if predictor {
            "/DecodeParms << /Predictor 12 /Columns 7 >>"
        } else {
            ""
        };
        self.stream(
            id,
            &format!(
                "/Type /XRef /Size {} /W [1 4 2] /Filter /FlateDecode /Length {} {params} {extra}",
                id + 1,
                encoded.len()
            ),
            &encoded,
        );
        self.bytes
            .extend_from_slice(format!("startxref\n{offset}\n%%EOF\n").as_bytes());
        offset
    }
}

pub fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

pub fn document(catalog: &[u8], extra: &str) -> Vec<u8> {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, catalog);
    pdf.finish(&format!("/Root 1 0 R {extra}"));
    pdf.bytes
}

pub fn metadata_pdf() -> Pdf {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R /AF [4 0 R] >>");
    pdf.stream(2, "/Type /Metadata /Subtype /XML /Length 3 0 R", b"<xmp/>");
    pdf.object(3, 0, b"6");
    pdf.object(4, 0, b"<< /Type /Filespec /Subtype /application#2Fc2pa /AFRelationship /C2PA_Manifest /EF << /F 5 0 R >> >>");
    pdf.stream(5, "/Type /EmbeddedFile /Length 8", b"manifest");
    pdf
}

pub fn parse_error(bytes: Vec<u8>) -> Error {
    match Asset::from_source(Cursor::new(bytes)) {
        Ok(_) => panic!("malformed or unsupported PDF accepted"),
        Err(error) => error,
    }
}
