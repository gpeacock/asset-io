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
        "/Filter /FlateDecode",
        "/Filter [/ASCII85Decode]",
        "/F (remote.pdf)",
        "/DecodeParms << /Predictor 12 >>",
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
    assert!(matches!(parse_error(bytes), Error::UnsupportedFeature(_)));
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
fn writes_and_in_place_updates_fail_without_touching_output() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let original = pdf.bytes.clone();
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    let mut output = Cursor::new(b"unchanged".to_vec());
    assert!(matches!(
        asset.write(&mut output, &Updates::new()),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(output.get_ref(), b"unchanged");
    let mut called = false;
    assert!(matches!(
        asset.write_with_processing(
            &mut output,
            &Updates::new(),
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
    asset.write(&mut copied, &Updates::new()).unwrap_err();
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
fn write_to_does_not_truncate_or_create_files() {
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
        asset.write_to(&output, &Updates::new()),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(std::fs::read(&output).unwrap(), b"unchanged");
    assert!(asset.write_to(&absent, &Updates::new()).is_err());
    assert!(!absent.exists());
}

struct Counted {
    source: Cursor<Vec<u8>>,
    read: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Read for Counted {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read(buffer)?;
        self.read.set(self.read.get() + count);
        Ok(count)
    }
}

impl Seek for Counted {
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
