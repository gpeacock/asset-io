#![cfg(feature = "pdf")]

use asset_io::{Asset, ContainerKind, Error, ExclusionMode, SegmentKind, Updates};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom};

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
    assert_eq!(asset.container(), ContainerKind::Pdf);
    assert_eq!(asset.container().to_mime(), "application/pdf");
    assert_eq!(asset.media_type().to_extension(), "pdf");
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
            let segment = &asset.structure().segments()[asset.structure().xmp_index().unwrap()];
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
    let asset = unsafe { Asset::open_with_mmap(&path) }.unwrap();
    let structure = asset.structure();
    let segment = &structure.segments()[structure.xmp_index().unwrap()];
    assert_eq!(
        structure.get_mmap_slice(segment.location()).unwrap(),
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
fn unregistered_attachment_edits_fail_but_noop_copies_exactly() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let original = pdf.bytes.clone();
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    let mut output = Cursor::new(b"unchanged".to_vec());
    asset.write(&mut output, &Updates::new()).unwrap();
    assert_eq!(output.get_ref(), &original);
    output = Cursor::new(b"unchanged".to_vec());
    let mut called = false;
    assert!(matches!(
        asset.write_with_processing(
            &mut output,
            &Updates::new().set_jumbf(b"replacement".to_vec()),
            &mut |_: &dyn asset_io::ProcessChunk| {
                called = true;
                Ok(())
            }
        ),
        Err(Error::UnsupportedFeature(_))
    ));
    assert!(!called);
    assert!(matches!(
        asset.update_segment_in_place(SegmentKind::Xmp, b"bad".to_vec()),
        Err(Error::UnsupportedFeature(_))
    ));
    assert!(matches!(
        asset
            .structure()
            .update_segment(&mut output, SegmentKind::Jumbf, b"bad".to_vec()),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(output.get_ref(), b"unchanged");
    let mut copied = Cursor::new(Vec::new());
    asset.write(&mut copied, &Updates::new()).unwrap();
    assert_eq!(copied.get_ref(), &original);
    let mut bytes = Vec::new();
    asset
        .read_with_processing(&Updates::new(), &mut |chunk: &[u8]| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(bytes, original);
}

#[test]
fn unsupported_write_to_does_not_truncate_or_create_files() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.pdf");
    let output = dir.path().join("output.pdf");
    let absent = dir.path().join("absent.pdf");
    std::fs::write(&input, &pdf.bytes).unwrap();
    std::fs::write(&output, b"unchanged").unwrap();
    let mut asset = Asset::open(input).unwrap();
    assert!(matches!(
        asset.write_to(&output, &Updates::new().set_jumbf(b"new".to_vec())),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(std::fs::read(&output).unwrap(), b"unchanged");
    assert!(asset
        .write_to(&absent, &Updates::new().remove_jumbf())
        .is_err());
    assert!(!absent.exists());
}

fn writable_pdf() -> Pdf {
    let mut pdf = metadata_pdf();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R /AF [4 0 R 6 0 R] /Names << /Unrelated (kept) /EmbeddedFiles << /Names [(credentials) 4 0 R (other.txt) 6 0 R] /Limits [(credentials) (other.txt)] /Custom true >> >> /Unknown [false -1.25 (a\\(b\\)\\053) <00ff>] >>");
    pdf.object(
        6,
        0,
        b"<< /Type /Filespec /AFRelationship /Data /F (other.txt) /EF << /F 7 0 R >> >>",
    );
    pdf.stream(7, "/Type /EmbeddedFile /Length 5", b"other");
    pdf.object(8, 0, b"<< /Producer (asset-io test) >>");
    pdf
}

fn same_layout(expected: &asset_io::Structure, actual: &asset_io::Structure) {
    assert_eq!(expected.total_size, actual.total_size);
    let layout = |structure: &asset_io::Structure| {
        structure
            .segments()
            .iter()
            .map(|segment| {
                (
                    segment.kind,
                    segment.ranges.clone(),
                    segment.path.clone(),
                    segment.metadata.is_some(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(layout(expected), layout(actual));
}

#[test]
fn incremental_write_combinations_preserve_prefix_and_predict_exact_layout() {
    for xmp in 0..3 {
        for jumbf in 0..3 {
            let mut pdf = writable_pdf();
            pdf.finish("/Root 1 0 R /ID [<0102> <0304>] /Info 8 0 R /Custom (kept)");
            let original = pdf.bytes;
            let mut asset = Asset::from_source(Cursor::new(&original)).unwrap();
            let mut updates = match xmp {
                0 => Updates::new(),
                1 => Updates::new().remove_xmp(),
                _ => Updates::new().set_xmp(b"<new-xmp/>".to_vec()),
            };
            updates = match jumbf {
                0 => updates,
                1 => updates.remove_jumbf(),
                _ => updates.set_jumbf(b"new-manifest".to_vec()),
            };
            let mut output = Cursor::new(Vec::new());
            let predicted = asset.write(&mut output, &updates).unwrap();
            let bytes = output.into_inner();
            assert!(bytes.starts_with(&original));
            let mut reparsed = Asset::from_source(Cursor::new(&bytes)).unwrap();
            same_layout(&predicted, reparsed.structure());
            let expected_xmp = [
                Some(b"<xmp/>".as_slice()),
                None,
                Some(b"<new-xmp/>".as_slice()),
            ][xmp];
            let expected_jumbf = [
                Some(b"manifest".as_slice()),
                None,
                Some(b"new-manifest".as_slice()),
            ][jumbf];
            assert_eq!(reparsed.xmp().unwrap().as_deref(), expected_xmp);
            assert_eq!(reparsed.jumbf().unwrap().as_deref(), expected_jumbf);
            if xmp != 0 || jumbf != 0 {
                let append = &bytes[original.len()..];
                let text = String::from_utf8_lossy(append);
                assert!(text.contains("/Unknown [false -1.25 <612862292B> <00FF>"));
                assert!(text.contains("/ID [<0102> <0304>"));
                assert!(text.contains("/Info 8 0 R"));
                assert!(text.contains("/Custom <6B657074>"));
                if jumbf != 0 {
                    assert!(text.contains("<6F746865722E747874> 6 0 R"));
                    assert!(text.contains("/AF [6 0 R"));
                    assert!(text.contains("/Unrelated <6B657074>"));
                    assert!(text.contains("/Custom true"));
                }
            } else {
                assert_eq!(bytes, original);
            }
        }
    }
}

#[test]
fn new_metadata_and_multiple_revisions_round_trip_even_empty_payloads() {
    let mut bytes = document(b"<< /Type /Catalog >>", "");
    for data in [b"first".as_slice(), b"second".as_slice(), b"".as_slice()] {
        let mut asset = Asset::from_source(Cursor::new(&bytes)).unwrap();
        let mut output = Cursor::new(Vec::new());
        let structure = asset
            .write(
                &mut output,
                &Updates::new()
                    .set_xmp(data.to_vec())
                    .set_jumbf(data.to_vec()),
            )
            .unwrap();
        assert!(output.get_ref().starts_with(&bytes));
        bytes = output.into_inner();
        let mut asset = Asset::from_source(Cursor::new(&bytes)).unwrap();
        same_layout(&structure, asset.structure());
        assert_eq!(asset.xmp().unwrap().as_deref(), Some(data));
        assert_eq!(asset.jumbf().unwrap().as_deref(), Some(data));
    }
    let mut asset = Asset::from_source(Cursor::new(&bytes)).unwrap();
    let mut output = Cursor::new(Vec::new());
    let structure = asset
        .write(&mut output, &Updates::new().remove_xmp().remove_jumbf())
        .unwrap();
    assert!(output.get_ref().starts_with(&bytes));
    let mut asset = Asset::from_source(output).unwrap();
    same_layout(&structure, asset.structure());
    assert!(asset.xmp().unwrap().is_none());
    assert!(asset.jumbf().unwrap().is_none());
}

#[test]
fn writer_accepts_compressed_catalog_and_keeps_or_replaces_flate_metadata() {
    for replace in [false, true] {
        let mut pdf = Pdf::new();
        let catalog = b"1 0 << /Type /Catalog /Metadata 2 0 R /Unknown (preserved) >>";
        let encoded = deflate(catalog);
        pdf.stream(
            3,
            &format!(
                "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {}",
                encoded.len()
            ),
            &encoded,
        );
        let xmp = deflate(b"compressed-xmp");
        pdf.stream(
            2,
            &format!(
                "/Type /Metadata /Subtype /XML /Filter /FlateDecode /Length {}",
                xmp.len()
            ),
            &xmp,
        );
        pdf.finish_stream("/Root 1 0 R", &[(1, 3, 0)], true, false);
        let original = pdf.bytes;
        let mut asset = Asset::from_source(Cursor::new(&original)).unwrap();
        let updates = Updates::new().set_jumbf(b"new-manifest".to_vec());
        let updates = if replace {
            updates.set_xmp(b"plain-xmp".to_vec())
        } else {
            updates
        };
        let mut output = Cursor::new(Vec::new());
        let predicted = asset.write(&mut output, &updates).unwrap();
        assert!(output.get_ref().starts_with(&original));
        let mut reparsed = Asset::from_source(output).unwrap();
        same_layout(&predicted, reparsed.structure());
        assert_eq!(
            reparsed.xmp().unwrap().unwrap(),
            if replace {
                b"plain-xmp".as_slice()
            } else {
                b"compressed-xmp".as_slice()
            }
        );
        assert_eq!(reparsed.jumbf().unwrap().unwrap(), b"new-manifest");
    }
}

#[test]
fn write_processing_matches_final_ranges_including_historical_payloads() {
    for mode in [ExclusionMode::DataOnly, ExclusionMode::EntireSegment] {
        for size in [1, 7, 65536, usize::MAX] {
            let mut pdf = writable_pdf();
            pdf.finish("/Root 1 0 R");
            let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
            let updates = Updates::new()
                .set_jumbf(b"new-manifest".to_vec())
                .exclude_from_processing(vec![SegmentKind::Jumbf, SegmentKind::Xmp], mode)
                .with_chunk_size(size);
            let mut written = Vec::new();
            let mut boundaries = Vec::new();
            let mut output = Cursor::new(Vec::new());
            let structure = asset
                .write_with_processing(
                    &mut output,
                    &updates,
                    &mut |chunk: &dyn asset_io::ProcessChunk| {
                        assert!(chunk.data().len() <= size);
                        written.extend_from_slice(chunk.data());
                        if let Some(segment) = chunk.segment() {
                            boundaries.push((segment.kind, segment.location()));
                        }
                        Ok(())
                    },
                )
                .unwrap();
            let mut reparsed = Asset::from_source(output).unwrap();
            same_layout(&structure, reparsed.structure());
            let mut read = Vec::new();
            reparsed
                .read_with_processing(&updates, &mut |chunk: &[u8]| {
                    read.extend_from_slice(chunk);
                    Ok(())
                })
                .unwrap();
            assert_eq!(written, read);
            // Superseded C2PA is ordinary historical data, not silently excluded.
            assert!(written
                .windows(b"manifest".len())
                .any(|bytes| bytes == b"manifest"));
            assert!(!boundaries.is_empty());
        }
    }
}

#[test]
fn exact_length_finalization_changes_only_reserved_bytes() {
    let original = document(b"<< /Type /Catalog >>", "");
    let mut asset = Asset::from_source(Cursor::new(original)).unwrap();
    let mut output = Cursor::new(Vec::new());
    let structure = asset
        .write(
            &mut output,
            &Updates::new()
                .set_jumbf(vec![0; 32])
                .set_xmp(b"<xmp/>".to_vec()),
        )
        .unwrap();
    for length in [0, 31, 33] {
        let before = output.get_ref().clone();
        assert!(structure
            .update_segment(&mut output, SegmentKind::Jumbf, vec![1; length])
            .is_err());
        assert_eq!(&before, output.get_ref());
    }
    let before = output.get_ref().clone();
    let range = structure.segments()[structure.c2pa_jumbf_index().unwrap()].location();
    structure
        .update_segment(&mut output, SegmentKind::Jumbf, vec![7; 32])
        .unwrap();
    assert_eq!(
        &before[..range.offset as usize],
        &output.get_ref()[..range.offset as usize]
    );
    assert_eq!(
        &before[range.end_offset() as usize..],
        &output.get_ref()[range.end_offset() as usize..]
    );
    let mut asset = Asset::from_source(output).unwrap();
    assert_eq!(asset.jumbf().unwrap().unwrap(), vec![7; 32]);
    assert!(asset
        .update_segment_in_place(SegmentKind::Xmp, b"short".to_vec())
        .is_err());
    asset
        .update_segment_in_place(SegmentKind::Xmp, b"<new/>".to_vec())
        .unwrap();
    assert_eq!(asset.xmp().unwrap().unwrap(), b"<new/>");
}

#[test]
fn signed_certified_and_unsupported_attachment_trees_reject_before_output() {
    for catalog in [
        b"<< /Type /Catalog /Perms << /DocMDP 2 0 R >> >>".as_slice(),
        b"<< /Type /Catalog /AcroForm << /Fields [<< /FT /Sig /V << /Type /Sig >> >>] >> >>",
        b"<< /Type /Catalog /Names << /EmbeddedFiles << /Kids [2 0 R] >> >> >>",
        b"<< /Type /Catalog /Names << /EmbeddedFiles << /Names [(Content Credentials) 2 0 R] >> >> >>",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.pdf");
        std::fs::write(&path, b"unchanged").unwrap();
        let input = dir.path().join("input.pdf");
        std::fs::write(&input, document(catalog, "")).unwrap();
        let mut asset = Asset::open(&input).unwrap();
        let updates = Updates::new().set_jumbf(b"new".to_vec());
        let mut output = Cursor::new(b"unchanged".to_vec());
        let mut called = false;
        assert!(asset.write_with_processing(&mut output, &updates, &mut |_: &dyn asset_io::ProcessChunk| { called = true; Ok(()) }).is_err());
        assert!(!called);
        assert_eq!(output.get_ref(), b"unchanged");
        assert!(asset.write_to(&path, &updates).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

#[test]
fn write_to_safely_replaces_source_path_or_hard_link_and_cleans_temporary_file() {
    for hard_link in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.pdf");
        let original = document(b"<< /Type /Catalog >>", "");
        std::fs::write(&path, &original).unwrap();
        let output = if hard_link {
            let link = dir.path().join("linked.pdf");
            std::fs::hard_link(&path, &link).unwrap();
            link
        } else {
            path.clone()
        };
        let mut asset = Asset::open(&path).unwrap();
        let predicted = asset
            .write_to(&output, &Updates::new().set_jumbf(b"new-manifest".to_vec()))
            .unwrap();
        assert!(std::fs::read(&output).unwrap().starts_with(&original));
        let mut asset = Asset::open(&output).unwrap();
        same_layout(&predicted, asset.structure());
        assert_eq!(asset.jumbf().unwrap().unwrap(), b"new-manifest");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            if hard_link { 2 } else { 1 }
        );
        if hard_link {
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
}

fn form_pdf(form: &[u8], fields: &[(u32, &[u8])], compressed: bool) -> Vec<u8> {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog /AcroForm 2 0 R >>");
    pdf.object(2, 0, form);
    for &(id, body) in fields {
        pdf.object(id, 0, body);
    }
    if compressed {
        let mut header = Vec::new();
        let mut body = Vec::new();
        for &(id, value) in fields {
            header.extend_from_slice(format!("{id} {} ", body.len()).as_bytes());
            body.extend_from_slice(value);
            body.push(b' ');
        }
        let first = header.len();
        header.extend_from_slice(&body);
        let encoded = deflate(&header);
        let container = pdf.highest_id + 1;
        pdf.stream(
            container,
            &format!(
                "/Type /ObjStm /N {} /First {first} /Filter /FlateDecode /Length {}",
                fields.len(),
                encoded.len()
            ),
            &encoded,
        );
        let entries: Vec<_> = fields
            .iter()
            .enumerate()
            .map(|(index, &(id, _))| (id, container, index as u16))
            .collect();
        pdf.finish_stream("/Root 1 0 R", &entries, false, false);
    } else {
        pdf.finish("/Root 1 0 R");
    }
    pdf.bytes
}

#[test]
fn unsigned_forms_and_empty_signature_fields_are_preserved() {
    for compressed in [false, true] {
        for value in [
            b"<< /Fields 3 0 R /SigFlags 3 >>".as_slice(),
            b"<< /Fields [4 0 R] >>",
        ] {
            let original = form_pdf(
                value,
                &[
                    (3, b"[4 0 R]"),
                    (4, b"<< /FT /Sig /Kids 5 0 R >>"),
                    (5, b"[6 0 R 7 0 R]"),
                    (6, b"<< /Parent 4 0 R /V 8 0 R >>"),
                    (7, b"<< /Parent 4 0 R /FT /Tx /V (ordinary text) >>"),
                    (8, b"null"),
                ],
                compressed,
            );
            let mut asset = Asset::from_source(Cursor::new(&original)).unwrap();
            let mut output = Cursor::new(Vec::new());
            let predicted = asset
                .write(&mut output, &Updates::new().set_jumbf(vec![0; 32]))
                .unwrap();
            assert!(output.get_ref().starts_with(&original));
            predicted
                .update_segment(&mut output, SegmentKind::Jumbf, vec![1; 32])
                .unwrap();
            let mut reparsed = Asset::from_source(output).unwrap();
            same_layout(&predicted, reparsed.structure());
            assert_eq!(reparsed.jumbf().unwrap().unwrap(), vec![1; 32]);
        }
    }
    for form in [
        b"<< /Fields [] >>".as_slice(),
        b"<< /Fields [<< /FT /Tx /V (hello) >> << /FT /Sig >>] >>",
    ] {
        let mut asset = Asset::from_source(Cursor::new(form_pdf(form, &[], false))).unwrap();
        asset
            .write(
                &mut Cursor::new(Vec::new()),
                &Updates::new().set_xmp(b"<xmp/>".to_vec()),
            )
            .unwrap();
    }
    let fields = format!("<< /Fields [{}] >>", "<< /FT /Tx >> ".repeat(4096));
    let mut asset =
        Asset::from_source(Cursor::new(form_pdf(fields.as_bytes(), &[], false))).unwrap();
    asset
        .write(
            &mut Cursor::new(Vec::new()),
            &Updates::new().set_xmp(b"ok".to_vec()),
        )
        .unwrap();
}

#[test]
fn populated_signature_fields_including_inherited_types_block_edits_and_patches() {
    for compressed in [false, true] {
        for field in [
            b"<< /FT /Sig /V << /Type /Sig >> >>".as_slice(),
            b"<< /FT /Sig /V 7 0 R >>",
            b"<< /FT /Sig /Kids [5 0 R] >>",
            b"<< /FT 6 0 R /Kids [5 0 R] >>",
            b"<< /Kids [<< /FT /Sig /V << /Type /Sig >> >>] >>",
        ] {
            let bytes = form_pdf(
                b"<< /Fields [4 0 R] >>",
                &[
                    (4, field),
                    (5, b"<< /Parent 4 0 R /V 7 0 R >>"),
                    (6, b"/Sig"),
                    (7, b"<< /Type /Sig /Contents <0123> >>"),
                ],
                compressed,
            );
            let mut asset = Asset::from_source(Cursor::new(bytes)).unwrap();
            let mut output = Cursor::new(b"unchanged".to_vec());
            let error = asset
                .write(&mut output, &Updates::new().set_jumbf(vec![0; 32]))
                .unwrap_err();
            assert!(
                matches!(error, Error::UnsupportedFeature(reason) if reason.contains("populated signature")),
                "{error}"
            );
            assert!(matches!(
                asset
                    .structure()
                    .update_segment(&mut output, SegmentKind::Jumbf, vec![0; 32]),
                Err(Error::UnsupportedFeature(_))
            ));
            assert_eq!(output.get_ref(), b"unchanged");
        }
    }
}

#[test]
fn malformed_cyclic_and_deep_forms_block_edits_without_blocking_reads() {
    let mut cases = vec![
        form_pdf(b"<< >>", &[], false),
        form_pdf(b"<< /Fields null >>", &[], false),
        form_pdf(b"<< /Fields [4 0 R] >>", &[(4, b"<< /FT 10 >>")], false),
        form_pdf(
            b"<< /Fields [4 0 R] >>",
            &[(4, b"<< /Parent 5 0 R >>")],
            false,
        ),
        form_pdf(
            b"<< /Fields [4 0 R] >>",
            &[(4, b"<< /Kids [4 0 R] >>")],
            false,
        ),
        form_pdf(
            b"<< /Fields [4 0 R] >>",
            &[(4, b"<< /Kids 5 0 R >>"), (5, b"5 0 R")],
            false,
        ),
    ];
    for depth in [32, 33] {
        let mut pdf = Pdf::new();
        pdf.object(
            1,
            0,
            b"<< /Type /Catalog /AcroForm << /Fields [2 0 R] >> >>",
        );
        for level in 0..depth {
            let id = level + 2;
            let field = if level == depth - 1 {
                b"<< /FT /Tx >>".to_vec()
            } else {
                format!("<< /Kids [{} 0 R] >>", id + 1).into_bytes()
            };
            pdf.object(id, 0, &field);
        }
        pdf.finish("/Root 1 0 R");
        if depth == 32 {
            let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
            asset
                .write(
                    &mut Cursor::new(Vec::new()),
                    &Updates::new().set_xmp(b"ok".to_vec()),
                )
                .unwrap();
        } else {
            cases.push(pdf.bytes);
        }
    }
    let count = format!("<< /Fields [{}] >>", "<< /FT /Tx >> ".repeat(4097));
    cases.push(form_pdf(count.as_bytes(), &[], false));
    for bytes in cases {
        let mut asset = Asset::from_source(Cursor::new(bytes)).unwrap();
        assert!(asset.xmp().unwrap().is_none());
        let mut output = Cursor::new(b"unchanged".to_vec());
        assert!(matches!(
            asset.write(&mut output, &Updates::new().set_xmp(b"new".to_vec())),
            Err(Error::InvalidFormat(_))
        ));
        assert_eq!(output.get_ref(), b"unchanged");
    }
    let mut asset = Asset::from_source(Cursor::new(form_pdf(
        b"<< /Fields [] /XFA [(template) 3 0 R] >>",
        &[],
        false,
    )))
    .unwrap();
    assert!(
        matches!(asset.write(&mut Cursor::new(Vec::new()), &Updates::new().remove_xmp()), Err(Error::UnsupportedFeature(reason)) if reason.contains("XFA"))
    );
}

#[test]
fn write_rejects_revision_overflow_and_zero_chunk_size_without_touching_output() {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog >>");
    let mut previous = pdf.finish("/Root 1 0 R");
    pdf.entries.clear();
    for _ in 1..128 {
        previous = pdf.finish(&format!("/Prev {previous}"));
    }
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    let mut output = Cursor::new(b"unchanged".to_vec());
    assert!(asset
        .write(&mut output, &Updates::new().remove_xmp())
        .is_err());
    assert_eq!(output.get_ref(), b"unchanged");
    assert!(asset
        .write(&mut output, &Updates::new().with_chunk_size(0))
        .is_err());
    assert_eq!(output.get_ref(), b"unchanged");
}

#[test]
fn writer_propagates_processor_cancellation() {
    let mut asset = Asset::from_source(Cursor::new(document(b"<< /Type /Catalog >>", ""))).unwrap();
    assert!(matches!(
        asset.write_with_processing(
            &mut Cursor::new(Vec::new()),
            &Updates::new().set_xmp(b"new".to_vec()),
            &mut |_: &dyn asset_io::ProcessChunk| Err(Error::UserCanceled)
        ),
        Err(Error::UserCanceled)
    ));
}

#[test]
fn write_preflight_enforces_xref_node_and_object_number_limits() {
    let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 100000\n0000000000 65535 f\n0000000009 00000 n\n");
    for _ in 2..100000 {
        bytes.extend_from_slice(b"0000000000 00000 f\n");
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 100000 /Root 1 0 R >>\nstartxref\n{offset}\n%%EOF\n").as_bytes(),
    );
    let nodes = format!("<< /Type /Catalog /Ignored [{}] >>", "null ".repeat(99_990));
    let mut overflow = Pdf::new();
    overflow.object(1, 0, b"<< /Type /Catalog >>");
    overflow.highest_id = u32::MAX - 1;
    overflow.finish("/Root 1 0 R");
    for source in [bytes, document(nodes.as_bytes(), ""), overflow.bytes] {
        let mut asset = Asset::from_source(Cursor::new(source)).unwrap();
        let mut output = Cursor::new(b"unchanged".to_vec());
        assert!(asset
            .write(&mut output, &Updates::new().set_xmp(b"new".to_vec()))
            .is_err());
        assert_eq!(output.get_ref(), b"unchanged");
    }
}

#[test]
fn compressed_patches_and_changed_source_length_are_rejected_before_output() {
    let mut pdf = Pdf::new();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R >>");
    let encoded = deflate(b"<xmp/>");
    pdf.stream(
        2,
        &format!(
            "/Type /Metadata /Subtype /XML /Filter /FlateDecode /Length {}",
            encoded.len()
        ),
        &encoded,
    );
    pdf.finish("/Root 1 0 R");
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.pdf");
    std::fs::write(&input, &pdf.bytes).unwrap();
    let mut asset = Asset::open(&input).unwrap();
    let mut output = Cursor::new(b"unchanged".to_vec());
    assert!(matches!(
        asset
            .structure()
            .update_segment(&mut output, SegmentKind::Xmp, vec![0; encoded.len()]),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(output.get_ref(), b"unchanged");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&input)
        .unwrap()
        .set_len(10)
        .unwrap();
    assert!(asset
        .write(&mut output, &Updates::new().set_jumbf(b"new".to_vec()))
        .is_err());
    assert_eq!(output.get_ref(), b"unchanged");
}

#[test]
fn malformed_attachment_tree_does_not_hide_readable_metadata_but_blocks_writing() {
    let mut pdf = metadata_pdf();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R /AF [4 0 R] /Names << /EmbeddedFiles << /Names [(missing-value)] >> >> >>");
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    assert_eq!(asset.jumbf().unwrap().unwrap(), b"manifest");
    assert_eq!(asset.xmp().unwrap().unwrap(), b"<xmp/>");
    let mut output = Cursor::new(b"unchanged".to_vec());
    assert!(matches!(
        asset.write(&mut output, &Updates::new().remove_jumbf()),
        Err(Error::InvalidFormat(_))
    ));
    assert_eq!(output.get_ref(), b"unchanged");
}

struct Counted<R> {
    source: R,
    read: std::rc::Rc<std::cell::Cell<usize>>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read(buffer)?;
        self.read.set(self.read.get() + count);
        Ok(count)
    }
}

impl<R: Seek> Seek for Counted<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.source.seek(position)
    }
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
    assert!(asset.structure().total_size > 1024 * 1024 * 1024);
    assert!(read.get() < 64 * 1024, "read {} bytes", read.get());
}

#[test]
fn processing_exclusions_match_discovered_ranges() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes.clone())).unwrap();
    let (offset, length) = asset
        .structure()
        .exclusion_range_for_segment(SegmentKind::Jumbf)
        .unwrap();
    let mut expected = pdf.bytes[..offset as usize].to_vec();
    expected.extend_from_slice(&pdf.bytes[(offset + length) as usize..]);
    let updates =
        Updates::new().exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
    let mut actual = Vec::new();
    asset
        .read_with_processing(&updates, &mut |chunk: &[u8]| {
            actual.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, expected);
}
