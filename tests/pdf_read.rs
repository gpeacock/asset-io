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

//! Reading PDF metadata, on synthetic documents built here. The cases are those of the
//! 0.1 suite; `Asset` below is a thin stand-in for what they called.

#![cfg(feature = "pdf")]

use asset_io::{Error, HandlerRegistry, Segment, SegmentKind, Structure};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom};

/// A parsed document and the stream it came from.
struct Asset<R> {
    source: R,
    structure: Structure,
}

impl<R: Read + Seek> Asset<R> {
    fn from_source(mut source: R) -> asset_io::Result<Self> {
        let structure = HandlerRegistry::builtin().parse("pdf", &mut source)?;
        Ok(Self { source, structure })
    }

    fn from_file(file: R) -> asset_io::Result<Self> {
        Self::from_source(file)
    }

    fn structure(&self) -> &Structure {
        &self.structure
    }

    fn xmp(&mut self) -> asset_io::Result<Option<Vec<u8>>> {
        self.structure.xmp(&mut self.source)
    }

    fn jumbf(&mut self) -> asset_io::Result<Option<Vec<u8>>> {
        self.structure.store(&mut self.source)
    }
}

struct Location {
    offset: u64,
    size: u64,
}

impl Location {
    fn end_offset(&self) -> u64 {
        self.offset + self.size
    }
}

trait SegmentExt {
    fn location(&self) -> Location;
    fn is_type(&self, kind: SegmentKind) -> bool;
}

impl SegmentExt for Segment {
    fn location(&self) -> Location {
        Location {
            offset: self.extent().offset(),
            size: self.extent().len(),
        }
    }

    fn is_type(&self, kind: SegmentKind) -> bool {
        self.kind() == kind
    }
}

/// How many bytes a source has been asked for.
struct Counted<R> {
    source: R,
    read: std::rc::Rc<std::cell::Cell<usize>>,
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

struct Pdf {
    bytes: Vec<u8>,
    entries: BTreeMap<u32, (u64, u16, bool)>,
    highest_id: u32,
}

impl Pdf {
    fn new() -> Self {
        Self {
            bytes: b"%PDF-1.7\n".to_vec(),
            entries: BTreeMap::new(),
            highest_id: 0,
        }
    }

    fn object(&mut self, id: u32, generation: u16, body: &[u8]) {
        self.highest_id = self.highest_id.max(id);
        self.entries
            .insert(id, (self.bytes.len() as u64, generation, true));
        self.bytes
            .extend_from_slice(format!("{id} {generation} obj\n").as_bytes());
        self.bytes.extend_from_slice(body);
        self.bytes.extend_from_slice(b"\nendobj\n");
    }

    fn stream(&mut self, id: u32, dict: &str, data: &[u8]) {
        let mut body = format!("<< {dict} >>\nstream\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.object(id, 0, &body);
    }

    fn finish(&mut self, extra: &str) -> u64 {
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

    fn finish_stream(
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

fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn document(catalog: &[u8], extra: &str) -> Vec<u8> {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, catalog);
    pdf.finish(&format!("/Root 1 0 R {extra}"));
    pdf.bytes
}

fn metadata_pdf() -> Pdf {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R /AF [4 0 R] >>");
    pdf.stream(2, "/Type /Metadata /Subtype /XML /Length 3 0 R", b"<xmp/>");
    pdf.object(3, 0, b"6");
    pdf.object(4, 0, b"<< /Type /Filespec /Subtype /application#2Fc2pa /AFRelationship /C2PA_Manifest /EF << /F 5 0 R >> >>");
    pdf.stream(5, "/Type /EmbeddedFile /Length 8", b"manifest");
    pdf
}

fn parse_error(bytes: Vec<u8>) -> Error {
    match Asset::from_source(Cursor::new(bytes)) {
        Ok(_) => panic!("malformed or unsupported PDF accepted"),
        Err(error) => error,
    }
}

#[test]
fn detects_pdf_and_extracts_exact_payload_ranges() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes.clone())).unwrap();
    assert_eq!(asset.structure().media_type(), "application/pdf");
    assert_eq!(
        HandlerRegistry::builtin().extension_for("application/pdf"),
        Some("pdf")
    );
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"<xmp/>".as_slice()));
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"manifest".as_slice())
    );
    let structure = asset.structure();
    let mut end = 0;
    for segment in structure.segments() {
        let range = segment.location();
        assert_eq!(range.offset, end);
        end = range.end_offset();
        if segment.is_type(SegmentKind::Xmp) {
            assert_eq!(&pdf.bytes[range.offset as usize..end as usize], b"<xmp/>");
        }
    }
    assert_eq!(end, pdf.bytes.len() as u64);
}

#[test]
fn flate_metadata_is_decoded_only_on_request() {
    let mut pdf = metadata_pdf();
    for (id, dictionary, payload) in [
        (
            2,
            "/Type /Metadata /Subtype /XML",
            b"<compressed-xmp/>".as_slice(),
        ),
        (5, "/Type /EmbeddedFile", b"compressed manifest".as_slice()),
    ] {
        let data = deflate(payload);
        pdf.stream(
            id,
            &format!(
                "{dictionary} /Length {} /Filter [/FlateDecode] /DecodeParms [null]",
                data.len()
            ),
            &data,
        );
    }
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(
        asset.xmp().unwrap().as_deref(),
        Some(b"<compressed-xmp/>".as_slice())
    );
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"compressed manifest".as_slice())
    );

    let mut pdf = metadata_pdf();
    pdf.stream(
        2,
        "/Type /Metadata /Subtype /XML /Length 7 /Filter /FlateDecode",
        b"invalid",
    );
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert!(matches!(asset.xmp(), Err(Error::InvalidFormat(_))));
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"manifest".as_slice())
    );
}

#[test]
fn compressed_cross_references_support_predictors_and_legacy_sync_flush() {
    for predictor in [false, true] {
        for sync_flush in [false, true] {
            let mut pdf = metadata_pdf();
            pdf.finish_stream("/Root 1 0 R", &[], predictor, sync_flush);
            let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
            assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"<xmp/>".as_slice()));
            assert_eq!(
                asset.jumbf().unwrap().as_deref(),
                Some(b"manifest".as_slice())
            );
        }
    }
}

#[test]
fn compact_xref_table_handles_customer_scale_without_expanding_objects() {
    let mut pdf = metadata_pdf();
    pdf.highest_id = 196_127;
    pdf.finish_stream("/Root 1 0 R", &[], true, false);
    let read = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut asset = Asset::from_source(Counted {
        source: Cursor::new(pdf.bytes),
        read: read.clone(),
    })
    .unwrap();
    assert!(read.get() < 64 * 1024, "read {} bytes", read.get());
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"manifest".as_slice())
    );
}

#[test]
fn compressed_object_stream_resolves_catalog_file_spec_and_integer_length() {
    let mut pdf = metadata_pdf();
    let objects = [
        (1, "<< /Type /Catalog /Metadata 2 0 R /AF [4 0 R] >>"),
        (3, "6"),
        (
            4,
            "<< /Subtype /application#2Fc2pa /AFRelationship /C2PA_Manifest /EF << /F 5 0 R >> >>",
        ),
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (id, value) in objects {
        header.push_str(&format!("{id} {} ", body.len()));
        body.push_str(value);
        body.push(' ');
    }
    let data = deflate(format!("{header}{body}").as_bytes());
    pdf.stream(
        6,
        &format!(
            "/Type /ObjStm /N 3 /First {} /Length {} /Filter /FlateDecode",
            header.len(),
            data.len()
        ),
        &data,
    );
    pdf.finish_stream(
        "/Root 1 0 R",
        &[(1, 6, 0), (3, 6, 1), (4, 6, 2)],
        false,
        false,
    );
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"<xmp/>".as_slice()));
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"manifest".as_slice())
    );
}

#[test]
fn unreferenced_object_streams_are_never_decompressed() {
    let mut pdf = metadata_pdf();
    let data = deflate(&vec![b'x'; 9 * 1024 * 1024]);
    pdf.stream(
        6,
        &format!(
            "/Type /ObjStm /N 1 /First 4 /Length {} /Filter /FlateDecode",
            data.len()
        ),
        &data,
    );
    pdf.finish_stream("/Root 1 0 R", &[], false, false);
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"<xmp/>".as_slice()));
}

#[test]
fn rejects_object_stream_bomb_before_parsing_its_objects() {
    let mut pdf = metadata_pdf();
    let data = deflate(&vec![b'x'; 8 * 1024 * 1024 + 1]);
    pdf.stream(
        6,
        &format!(
            "/Type /ObjStm /N 1 /First 4 /Length {} /Filter /FlateDecode",
            data.len()
        ),
        &data,
    );
    pdf.finish_stream("/Root 1 0 R", &[(1, 6, 0)], false, false);
    assert!(parse_error(pdf.bytes)
        .to_string()
        .contains("decoded stream size limit"));
}

#[test]
fn rejects_wrong_compressed_object_identity_and_container_cycles() {
    for container in [1, 6] {
        let mut pdf = metadata_pdf();
        let data = deflate(b"99 0 << /Type /Catalog >>");
        pdf.stream(
            6,
            &format!(
                "/Type /ObjStm /N 1 /First 5 /Length {} /Filter /FlateDecode",
                data.len()
            ),
            &data,
        );
        pdf.finish_stream("/Root 1 0 R", &[(1, container, 0)], false, false);
        assert!(matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)));
    }
}

#[test]
fn mixed_classic_and_stream_revisions_preserve_latest_object_semantics() {
    let mut pdf = metadata_pdf();
    let previous = pdf.finish_stream("/Root 1 0 R", &[], true, false);
    pdf.entries.clear();
    pdf.object(1, 0, b"<< /Type /Catalog >>");
    pdf.finish(&format!("/Prev {previous}"));
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert!(asset.xmp().unwrap().is_none());
    assert!(asset.jumbf().unwrap().is_none());

    let mut pdf = metadata_pdf();
    let previous = pdf.finish("/Root 1 0 R");
    pdf.stream(2, "/Type /Metadata /Subtype /XML /Length 3", b"new");
    pdf.finish_stream(&format!("/Prev {previous}"), &[], false, false);
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"new".as_slice()));
}

fn sparse_xref_document(dictionary: &str, data: &[u8]) -> Vec<u8> {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog >>");
    let offset = pdf.bytes.len();
    let encoded = deflate(data);
    pdf.stream(
        2,
        &format!(
            "/Type /XRef /Root 1 0 R /Filter /FlateDecode /Length {} {dictionary}",
            encoded.len()
        ),
        &encoded,
    );
    pdf.bytes
        .extend_from_slice(format!("startxref\n{offset}\n%%EOF\n").as_bytes());
    pdf.bytes
}

#[test]
fn sparse_xrefs_and_zero_width_type_fields_are_supported() {
    let bytes = sparse_xref_document("/Size 3 /W [0 4 2] /Index [1 1]", &[0, 0, 0, 9, 0, 0]);
    let mut asset = Asset::from_source(Cursor::new(bytes)).unwrap();
    assert!(asset.jumbf().unwrap().is_none());
}

#[test]
fn malformed_compressed_xrefs_fail_explicitly() {
    for dictionary in [
        "/Size 3 /W [9 0 0] /Index [1 1]",
        "/Size 3 /W [0 0 0] /Index [1 1]",
        "/Size 3 /W [1 4] /Index [1 1]",
        "/Size 3 /W [1 4 2] /Index [1]",
        "/Size 3 /W [1 4 2] /Index [1 1 1 1]",
        "/Size 3 /W [1 4 2] /Index [3 1]",
        "/Size 3 /W [1 4 2] /Index [18446744073709551615 2]",
        "/Size 3 /W [1 4 2] /Index [1 2]",
        "/Size 3 /W [1 4 2] /Index [1 1] /Encrypt 9 0 R",
        "/Size 3 /W [1 4 2] /Index [1 1] /XRefStm 9",
    ] {
        assert!(
            Asset::from_source(Cursor::new(sparse_xref_document(
                dictionary,
                &[1, 0, 0, 0, 9, 0, 0]
            )))
            .is_err(),
            "{dictionary}"
        );
    }
    let bytes = sparse_xref_document("/Size 3 /W [1 4 2] /Index [1 1]", &[9, 0, 0, 0, 9, 0, 0]);
    assert!(matches!(parse_error(bytes), Error::InvalidFormat(_)));
}

#[test]
fn compact_xref_entry_limit_is_exact() {
    for count in [1_000_000, 1_000_001] {
        let mut pdf = Pdf::new();
        pdf.object(1, 0, b"<< /Type /Catalog >>");
        pdf.highest_id = count - 2;
        pdf.finish_stream("/Root 1 0 R", &[], false, false);
        let result = Asset::from_source(Cursor::new(pdf.bytes));
        assert_eq!(result.is_ok(), count == 1_000_000);
    }
}

#[test]
fn compressed_integer_at_end_of_object_stream_can_resolve_length() {
    let mut pdf = metadata_pdf();
    let data = deflate(b"3 0 6");
    pdf.stream(
        6,
        &format!(
            "/Type /ObjStm /N 1 /First 4 /Length {} /Filter /FlateDecode",
            data.len()
        ),
        &data,
    );
    pdf.finish_stream("/Root 1 0 R", &[(3, 6, 0)], false, false);
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"<xmp/>".as_slice()));
}

#[test]
fn no_metadata_is_not_an_error() {
    let mut asset = Asset::from_source(Cursor::new(document(b"<< /Type /Catalog >>", ""))).unwrap();
    assert!(asset.xmp().unwrap().is_none());
    assert!(asset.jumbf().unwrap().is_none());
}

#[test]
fn only_catalog_referenced_metadata_is_exposed() {
    let mut pdf = metadata_pdf();
    pdf.object(1, 0, b"<< /Type /Catalog >>");
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert!(asset.xmp().unwrap().is_none());
    assert!(asset.jumbf().unwrap().is_none());
}

#[test]
fn latest_revision_wins_and_inherits_trailer_root() {
    let mut pdf = metadata_pdf();
    let previous = pdf.finish("/Root 1 0 R");
    pdf.entries.clear();
    pdf.stream(2, "/Type /Metadata /Subtype /XML /Length 3", b"new");
    pdf.finish(&format!("/Prev {previous}"));
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"new".as_slice()));
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"manifest".as_slice())
    );
}

#[test]
fn latest_catalog_can_remove_old_metadata() {
    let mut pdf = metadata_pdf();
    let previous = pdf.finish("/Root 1 0 R");
    pdf.entries.clear();
    pdf.object(1, 0, b"<< /Type /Catalog >>");
    pdf.finish(&format!("/Root 1 0 R /Prev {previous}"));
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert!(asset.xmp().unwrap().is_none());
    assert!(asset.jumbf().unwrap().is_none());
}

#[test]
fn free_entries_mask_older_objects() {
    let mut pdf = metadata_pdf();
    let previous = pdf.finish("/Root 1 0 R");
    pdf.entries.clear();
    pdf.entries.insert(2, (0, 1, false));
    pdf.finish(&format!("/Prev {previous}"));
    assert!(matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)));
}

#[test]
fn names_comments_strings_and_nested_values_are_lexed_not_scanned() {
    let catalog = b"<< /Type /Cat#61log % /Metadata 2 0 R\n /Ignored [(fake /Metadata 2 0 R) <2f4d65746164617461> << /AF null >> -3 0 1.5 0 .5 0 +1.5 0 -1.5 true] >>";
    let mut asset = Asset::from_source(Cursor::new(document(catalog, ""))).unwrap();
    assert!(asset.xmp().unwrap().is_none());
    assert!(asset.jumbf().unwrap().is_none());
}

#[test]
fn af_can_be_an_indirect_array_or_a_file_specification() {
    for af in ["6 0 R", "4 0 R"] {
        let mut pdf = metadata_pdf();
        pdf.object(1, 0, format!("<< /Type /Catalog /AF {af} >>").as_bytes());
        pdf.object(6, 0, b"[4 0 R]");
        pdf.finish("/Root 1 0 R");
        let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
        assert_eq!(
            asset.jumbf().unwrap().as_deref(),
            Some(b"manifest".as_slice())
        );
    }
}

#[test]
fn accepts_crlf_and_binary_payload_with_fake_keywords() {
    let payload = b"\0endstream\nendobj\nxref\n%%EOF\xff";
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R >>");
    let mut body = format!(
        "<< /Type /Metadata /Subtype /XML /Length {} >>\r\nstream\r\n",
        payload.len()
    )
    .into_bytes();
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\r\nendstream");
    pdf.object(2, 0, &body);
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(payload.as_slice()));
}

#[test]
fn rejects_unsupported_features_explicitly() {
    for extra in ["/Encrypt 9 0 R", "/XRefStm 9"] {
        assert!(matches!(
            parse_error(document(b"<< /Type /Catalog >>", extra)),
            Error::UnsupportedFeature(_)
        ));
    }
    for extra in [
        "/Filter [/ASCII85Decode]",
        "/F (remote.pdf)",
        "/Filter /FlateDecode /DecodeParms << /Predictor 2 >>",
    ] {
        let mut pdf = metadata_pdf();
        pdf.stream(
            2,
            &format!("/Type /Metadata /Subtype /XML /Length 6 {extra}"),
            b"<xmp/>",
        );
        pdf.finish("/Root 1 0 R");
        assert!(matches!(
            parse_error(pdf.bytes),
            Error::UnsupportedFeature(_)
        ));
    }
    let bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /XRef >>\nendobj\nstartxref\n9\n%%EOF\n".to_vec();
    assert!(matches!(parse_error(bytes), Error::InvalidFormat(_)));
}

#[test]
fn rejects_malformed_or_ambiguous_catalogs() {
    for body in [
        "<< /Type /Catalog /Type /Catalog >>",
        "<< /Type /Pages >>",
        "<< /Type /Catalog /Metadata 99 0 R >>",
        "<< /Type /Catalog /Metadata 2 1 R >>",
        "<< /Type /Catalog /AF [4 0 R 4 0 R] >>",
        "<< /Type /Catalog /Metadata 1 0 R >>",
        "<< /Type /Catalog /AF 3 0 R >>",
    ] {
        let mut pdf = metadata_pdf();
        pdf.object(1, 0, body.as_bytes());
        pdf.finish("/Root 1 0 R");
        assert!(
            matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)),
            "{body}"
        );
    }
}

#[test]
fn rejects_bad_lengths_and_stream_terminators() {
    for length in [
        "-1",
        "1.5",
        "18446744073709551616",
        "18446744073709551615",
        "268435457",
        "999999",
        "2 0 R",
        "0",
        "8",
    ] {
        let mut pdf = metadata_pdf();
        pdf.stream(
            2,
            &format!("/Type /Metadata /Subtype /XML /Length {length}"),
            b"<xmp/>",
        );
        pdf.finish("/Root 1 0 R");
        assert!(
            matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)),
            "{length}"
        );
    }
    let mut pdf = metadata_pdf();
    pdf.object(
        2,
        0,
        b"<< /Type /Metadata /Subtype /XML /Length 6 >>\nstream\n<xmp/>\nendobj",
    );
    pdf.finish("/Root 1 0 R");
    assert!(matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)));
}

#[test]
fn rejects_cycles_and_deep_syntax() {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"2 0 R");
    pdf.object(2, 0, b"1 0 R");
    pdf.finish("/Root 1 0 R");
    assert!(matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)));
    let body = format!(
        "<< /Type /Catalog /Ignored {}0{} >>",
        "[".repeat(33),
        "]".repeat(33)
    );
    assert!(matches!(
        parse_error(document(body.as_bytes(), "")),
        Error::InvalidFormat(_)
    ));
    assert!(matches!(
        parse_error(document(
            b"<< /Type /Catalog >>",
            "/Prev 18446744073709551615"
        )),
        Error::InvalidFormat(_)
    ));
}

#[test]
fn rejects_oversized_xrefs_and_tokens() {
    let bytes = b"%PDF-1.7\nxref\n0 100001\nstartxref\n9\n%%EOF\n".to_vec();
    assert!(matches!(parse_error(bytes), Error::InvalidFormat(_)));
    let body = format!("<< /Type /Catalog /{} 0 >>", "A".repeat(4097));
    assert!(matches!(
        parse_error(document(body.as_bytes(), "")),
        Error::InvalidFormat(_)
    ));
}

#[test]
fn accepts_committed_fuzz_seed() {
    let mut asset = Asset::from_source(Cursor::new(
        include_bytes!("fixtures/pdf_metadata.pdf").as_slice(),
    ))
    .unwrap();
    assert_eq!(asset.xmp().unwrap().as_deref(), Some(b"<xmp/>".as_slice()));
    assert_eq!(
        asset.jumbf().unwrap().as_deref(),
        Some(b"manifest".as_slice())
    );
}

#[test]
fn rejects_conflicting_embedded_files() {
    let mut pdf = metadata_pdf();
    pdf.object(4, 0, b"<< /Subtype /application#2Fc2pa /AFRelationship /C2PA_Manifest /EF << /F 5 0 R /UF 2 0 R >> >>");
    pdf.finish("/Root 1 0 R");
    assert!(matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)));
}

#[test]
fn exact_metadata_size_limit_is_enforced_without_payload_allocation() {
    use std::io::Write;
    for length in [256u64 * 1024 * 1024, 256 * 1024 * 1024 + 1] {
        let mut prefix =
            b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Metadata 2 0 R >>\nendobj\n".to_vec();
        let metadata_offset = prefix.len();
        prefix.extend_from_slice(
            format!("2 0 obj\n<< /Type /Metadata /Subtype /XML /Length {length} >>\nstream\n")
                .as_bytes(),
        );
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&prefix).unwrap();
        file.seek(SeekFrom::Current(length as i64)).unwrap();
        file.write_all(b"\nendstream\nendobj\n").unwrap();
        let xref = file.stream_position().unwrap();
        write!(file, "xref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n{metadata_offset:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
        let asset = Asset::from_file(file);
        if length == 256 * 1024 * 1024 {
            let asset = asset.unwrap();
            let segment = asset
                .structure()
                .segments()
                .iter()
                .find(|s| s.kind() == SegmentKind::Xmp)
                .unwrap();
            assert_eq!(segment.location().size, length);
        } else {
            assert!(matches!(asset, Err(Error::InvalidFormat(_))));
        }
    }
}

#[test]
fn exact_revision_limit_is_enforced() {
    for revisions in [128, 129] {
        let mut pdf = Pdf::new();
        pdf.object(1, 0, b"<< /Type /Catalog >>");
        let mut previous = pdf.finish("/Root 1 0 R");
        pdf.entries.clear();
        for _ in 1..revisions {
            previous = pdf.finish(&format!("/Prev {previous}"));
        }
        let result = Asset::from_source(Cursor::new(pdf.bytes));
        assert_eq!(result.is_ok(), revisions == 128);
    }
}

#[test]
fn exact_object_resolution_limit_is_enforced() {
    for references in [4095, 4096] {
        let mut pdf = Pdf::new();
        let body = format!("<< /Type /Catalog /AF [{}] >>", "2 0 R ".repeat(references));
        pdf.object(1, 0, body.as_bytes());
        pdf.object(2, 0, b"<< /AFRelationship /Data >>");
        pdf.finish("/Root 1 0 R");
        let result = Asset::from_source(Cursor::new(pdf.bytes));
        assert_eq!(result.is_ok(), references == 4095);
    }
}

#[test]
fn exact_indirect_reference_depth_is_enforced() {
    for depth in [32, 33] {
        let mut pdf = Pdf::new();
        for id in 1..depth {
            pdf.object(id, 0, format!("{} 0 R", id + 1).as_bytes());
        }
        pdf.object(depth, 0, b"<< /Type /Catalog >>");
        pdf.finish("/Root 1 0 R");
        let result = Asset::from_source(Cursor::new(pdf.bytes));
        assert_eq!(result.is_ok(), depth == 32);
    }
}

#[test]
fn syntax_work_limit_includes_ignored_strings_and_comments() {
    for ignored in [
        format!("({})", "x".repeat(16 * 1024 * 1024)),
        format!("%{}\nnull", "x".repeat(16 * 1024 * 1024)),
    ] {
        let body = format!("<< /Type /Catalog /Ignored {ignored} >>");
        let error = parse_error(document(body.as_bytes(), ""));
        assert!(error.to_string().contains("syntax work limit"), "{error}");
    }
}

#[test]
fn exact_xref_entry_limit_is_enforced() {
    for count in [100_000, 100_001] {
        let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {count}\n0000000000 65535 f\n0000000009 00000 n\n").as_bytes(),
        );
        for _ in 2..count {
            bytes.extend_from_slice(b"0000000000 00000 f\n");
        }

        bytes.extend_from_slice(
            format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        let result = Asset::from_source(Cursor::new(bytes));
        assert_eq!(result.is_ok(), count == 100_000);
    }
}

#[test]
fn exact_syntax_node_limit_is_enforced() {
    for count in [99_990, 99_991] {
        let body = format!("<< /Type /Catalog /Ignored [{}] >>", "null ".repeat(count));
        let result = Asset::from_source(Cursor::new(document(body.as_bytes(), "")));
        assert_eq!(result.is_ok(), count == 99_990);
    }
}

#[test]
fn exact_name_and_nesting_limits_are_enforced() {
    for count in [4096, 4097] {
        let body = format!("<< /Type /Catalog /{} null >>", "x".repeat(count));
        let result = Asset::from_source(Cursor::new(document(body.as_bytes(), "")));
        assert_eq!(result.is_ok(), count == 4096);
    }
    for count in [30, 31] {
        let body = format!(
            "<< /Type /Catalog /Ignored {}null{} >>",
            "[".repeat(count),
            "]".repeat(count)
        );
        let result = Asset::from_source(Cursor::new(document(body.as_bytes(), "")));
        assert_eq!(result.is_ok(), count == 30);
    }
}

#[cfg(feature = "memory-mapped")]
#[test]
fn mmap_exposes_borrowed_metadata_bytes() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.pdf");
    std::fs::write(&path, &pdf.bytes).unwrap();
    // This test owns the file and does not modify it while mapped.
    let file = std::fs::File::open(&path).unwrap();
    let mapped = unsafe { asset_io::MappedFile::map(&file) }.unwrap();
    let structure = mapped.parse_detect(&HandlerRegistry::builtin()).unwrap();
    let xmp = structure.require_entity(SegmentKind::Xmp).unwrap();
    assert_eq!(
        &*structure.slice_entity(mapped.as_bytes(), xmp).unwrap(),
        b"<xmp/>"
    );
}

#[test]
fn io_errors_are_propagated() {
    struct Broken(Cursor<Vec<u8>>);
    impl Read for Broken {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.0.position() > 16 {
                return Err(std::io::Error::other("injected read failure"));
            }
            self.0.read(bytes)
        }
    }
    impl Seek for Broken {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(position)
        }
    }
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    assert!(matches!(
        Asset::from_source(Broken(Cursor::new(pdf.bytes))),
        Err(Error::Io(_))
    ));
}

#[test]
fn rejects_xref_object_identity_mismatch() {
    let mut pdf = metadata_pdf();
    let offset = pdf.entries[&2].0;
    pdf.bytes[offset as usize] = b'9';
    pdf.finish("/Root 1 0 R");
    assert!(matches!(parse_error(pdf.bytes), Error::InvalidFormat(_)));
}

#[test]
fn rejects_truncation_and_trailing_garbage_without_panicking() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    for end in 0..pdf.bytes.len() - 1 {
        assert!(
            Asset::from_source(Cursor::new(&pdf.bytes[..end])).is_err(),
            "truncation at {end}"
        );
    }
    pdf.bytes.extend_from_slice(b"garbage");
    assert!(Asset::from_source(Cursor::new(pdf.bytes)).is_err());
}

#[test]
fn parse_io_is_bounded_independently_of_payload_size() {
    for length in [1024 * 1024, 8 * 1024 * 1024] {
        let mut pdf = Pdf::new();
        pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R >>");
        pdf.stream(
            2,
            &format!("/Type /Metadata /Subtype /XML /Length {length}"),
            &vec![b'x'; length],
        );
        pdf.finish("/Root 1 0 R");
        let read = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut asset = Asset::from_source(Counted {
            source: Cursor::new(pdf.bytes),
            read: read.clone(),
        })
        .unwrap();
        assert!(
            read.get() < 64 * 1024,
            "parse read {} bytes for {length}-byte payload",
            read.get()
        );
        assert_eq!(asset.xmp().unwrap().unwrap().len(), length);
    }
}

#[test]
fn sparse_gigabyte_pdf_retrieves_compressed_xmp_with_bounded_io() {
    use std::io::Write;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"%PDF-1.7\n").unwrap();
    file.seek(SeekFrom::Start(1024 * 1024 * 1024)).unwrap();
    let catalog = file.stream_position().unwrap();
    file.write_all(b"1 0 obj\n<< /Type /Catalog /Metadata 2 0 R >>\nendobj\n")
        .unwrap();
    let metadata = file.stream_position().unwrap();
    let encoded = deflate(b"<large-document-xmp/>");
    write!(
        file,
        "2 0 obj\n<< /Type /Metadata /Subtype /XML /Filter /FlateDecode /Length {} >>\nstream\n",
        encoded.len()
    )
    .unwrap();
    file.write_all(&encoded).unwrap();
    file.write_all(b"\nendstream\nendobj\n").unwrap();
    let xref = file.stream_position().unwrap();
    let mut rows = Vec::new();
    for offset in [catalog, metadata, xref] {
        rows.push(1);
        rows.extend_from_slice(&(offset as u32).to_be_bytes());
        rows.extend_from_slice(&[0, 0]);
    }
    let encoded = deflate(&rows);
    write!(file, "3 0 obj\n<< /Type /XRef /Size 4 /Index [1 3] /W [1 4 2] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n", encoded.len()).unwrap();
    file.write_all(&encoded).unwrap();
    write!(file, "\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").unwrap();
    let read = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut asset = Asset::from_source(Counted {
        source: file,
        read: read.clone(),
    })
    .unwrap();
    assert_eq!(
        asset.xmp().unwrap().as_deref(),
        Some(b"<large-document-xmp/>".as_slice())
    );
    assert!(asset.jumbf().unwrap().is_none());
    assert!(asset.structure().total_size() > 1024 * 1024 * 1024);
    assert!(read.get() < 64 * 1024, "read {} bytes", read.get());
}

#[test]
fn a_sink_that_skips_the_store_sees_exactly_the_rest() {
    use asset_io::{Chunk, Role, Sink};

    /// Wants everything but the payload of a C2PA store.
    struct SkipStore(Vec<u8>);
    impl Sink for SkipStore {
        fn wants(&self, segment: &Segment, role: Role) -> bool {
            !(segment.kind() == SegmentKind::Jumbf && role == Role::Payload)
        }
        fn chunk(&mut self, chunk: Chunk<'_>) -> asset_io::Result<()> {
            self.0.extend_from_slice(chunk.data);
            Ok(())
        }
    }

    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes.clone())).unwrap();
    let store = asset
        .structure()
        .require_entity(SegmentKind::Jumbf)
        .unwrap()
        .parts()
        .next()
        .unwrap()
        .range();
    let (offset, end) = (store.offset() as usize, store.end() as usize);
    let mut expected = pdf.bytes[..offset].to_vec();
    expected.extend_from_slice(&pdf.bytes[end..]);

    let mut sink = SkipStore(Vec::new());
    asset.structure.read(&mut asset.source, &mut sink).unwrap();
    assert_eq!(sink.0, expected);
}
