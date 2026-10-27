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

//! Writing PDF metadata as appended revisions, on synthetic documents. The cases are
//! those of the 0.1 suite, with its chunk callbacks and path-based writes replaced by
//! sinks and in-memory streams.

mod pdf_support;

use asset_io_core::{Chunk, Error, ItemKind, Role, Segment, SegmentKind, Sink, Updates};
use pdf_support::*;
use std::io::Cursor;

#[test]
fn unregistered_attachment_edits_fail_but_noop_copies_exactly() {
    let mut pdf = metadata_pdf();
    pdf.finish("/Root 1 0 R");
    let original = pdf.bytes.clone();
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    let mut output = Vec::new();
    asset.write(&mut output, &Updates::new()).unwrap();
    assert_eq!(output, original);

    // Replacing a store that is not registered in the name tree is refused, and
    // nothing is written or delivered.
    struct Called(bool);
    impl Sink for Called {
        fn chunk(&mut self, _: Chunk<'_>) -> asset_io_core::Result<()> {
            self.0 = true;
            Ok(())
        }
    }
    let mut output = b"unchanged".to_vec();
    let mut called = Called(false);
    assert!(matches!(
        asset.write_with_sink(
            &mut output,
            &Updates::new().set_jumbf(b"replacement".to_vec()),
            &mut called
        ),
        Err(Error::UnsupportedFeature(_))
    ));
    assert!(!called.0);
    assert_eq!(output, b"unchanged");

    let mut file = Cursor::new(original.clone());
    assert!(matches!(
        asset
            .structure()
            .update_segment(&mut file, ItemKind::Xmp, b"bad".to_vec()),
        Err(Error::UnsupportedFeature(_))
    ));
    assert!(matches!(
        asset
            .structure()
            .update_segment(&mut file, ItemKind::Jumbf, b"bad".to_vec()),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(file.get_ref(), &original);

    // Reading is unaffected.
    let mut seen = Vec::new();
    struct Collect<'a>(&'a mut Vec<u8>);
    impl Sink for Collect<'_> {
        fn chunk(&mut self, c: Chunk<'_>) -> asset_io_core::Result<()> {
            self.0.extend_from_slice(c.data);
            Ok(())
        }
    }
    asset
        .structure
        .read(&mut asset.source, &mut Collect(&mut seen))
        .unwrap();
    assert_eq!(seen, original);
}

#[test]
fn a_sink_sees_the_output_with_superseded_data_as_ordinary_bytes() {
    // What a hash over the written file is: every byte, with the store the catalog now
    // points at excluded, and the one it replaced counted as ordinary data.
    struct SkipStores(Vec<u8>);
    impl Sink for SkipStores {
        fn wants(&self, segment: &Segment, role: Role) -> bool {
            !(segment.kind() == SegmentKind::Metadata && role == Role::Payload)
        }
        fn chunk(&mut self, c: Chunk<'_>) -> asset_io_core::Result<()> {
            self.0.extend_from_slice(c.data);
            Ok(())
        }
    }

    let mut pdf = writable_pdf();
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    let updates = Updates::new().set_jumbf(b"new-manifest".to_vec());
    let mut output = Vec::new();
    let mut sink = SkipStores(Vec::new());
    let structure = asset
        .write_with_sink(&mut output, &updates, &mut sink)
        .unwrap();

    let reparsed = Asset::from_source(Cursor::new(&output)).unwrap();
    same_layout(&structure, reparsed.structure());
    let mut expected = Vec::new();
    for kind in [ItemKind::Xmp, ItemKind::Jumbf] {
        let range = payload_range(reparsed.structure(), kind);
        // The ranges are collected, then removed from the end backwards below.
        expected.push((range.offset as usize, range.end_offset() as usize));
    }
    expected.sort();
    let mut walked = output.clone();
    for (from, to) in expected.into_iter().rev() {
        walked.drain(from..to);
    }
    assert_eq!(sink.0, walked);
    // The superseded manifest is ordinary data in the prefix, not silently excluded.
    assert!(sink.0.windows(8).any(|bytes| bytes == b"manifest"));
}

#[test]
fn signed_certified_and_unsupported_attachment_trees_reject_before_output() {
    for catalog in [
        b"<< /Type /Catalog /Perms << /DocMDP 2 0 R >> >>".as_slice(),
        b"<< /Type /Catalog /AcroForm << /Fields [<< /FT /Sig /V << /Type /Sig >> >>] >> >>",
        b"<< /Type /Catalog /Names << /EmbeddedFiles << /Kids [2 0 R] >> >> >>",
        b"<< /Type /Catalog /Names << /EmbeddedFiles << /Names [(Content Credentials) 2 0 R] >> >> >>",
    ] {
        let mut asset = Asset::from_source(Cursor::new(document(catalog, ""))).unwrap();
        let updates = Updates::new().set_jumbf(b"new".to_vec());
        let mut output = b"unchanged".to_vec();
        struct Called(bool);
        impl Sink for Called {
            fn chunk(&mut self, _: Chunk<'_>) -> asset_io_core::Result<()> {
                self.0 = true;
                Ok(())
            }
        }
        let mut called = Called(false);
        assert!(asset
            .write_with_sink(&mut output, &updates, &mut called)
            .is_err());
        assert!(!called.0);
        assert_eq!(output, b"unchanged");
    }
}

#[test]
fn writer_propagates_sink_cancellation() {
    struct Cancel;
    impl Sink for Cancel {
        fn chunk(&mut self, _: Chunk<'_>) -> asset_io_core::Result<()> {
            Err(Error::UserCanceled)
        }
    }
    let mut asset = Asset::from_source(Cursor::new(document(b"<< /Type /Catalog >>", ""))).unwrap();
    assert!(matches!(
        asset.write_with_sink(
            &mut Vec::new(),
            &Updates::new().set_xmp(b"new".to_vec()),
            &mut Cancel
        ),
        Err(Error::UserCanceled)
    ));
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
    let asset = Asset::from_source(Cursor::new(pdf.bytes.clone())).unwrap();
    let mut output = Cursor::new(b"unchanged".to_vec());
    assert!(matches!(
        asset
            .structure()
            .update_segment(&mut output, ItemKind::Xmp, vec![0; encoded.len()]),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(output.get_ref(), b"unchanged");

    // A source that is not the one that was parsed is caught before anything is written.
    let structure = asset.structure;
    let mut shorter = Cursor::new(pdf.bytes[..10].to_vec());
    let mut output = b"unchanged".to_vec();
    assert!(structure
        .write(
            &mut shorter,
            &mut output,
            &Updates::new().set_jumbf(b"new".to_vec()),
            &mut asset_io_core::NullSink
        )
        .is_err());
    assert_eq!(output, b"unchanged");
}

/// The write plan is the same whether the source is read blocking or by a driver.
#[test]
fn the_written_file_reparses_to_what_was_planned_and_to_what_the_sink_saw() {
    let mut pdf = writable_pdf();
    pdf.finish("/Root 1 0 R");
    let mut asset = Asset::from_source(Cursor::new(pdf.bytes)).unwrap();
    let updates = Updates::new()
        .set_xmp(b"<new/>".to_vec())
        .set_jumbf(b"new-manifest".to_vec());
    let planned = asset.structure.plan(&mut asset.source, &updates).unwrap();
    let mut output = Vec::new();
    let written = asset.write(&mut output, &updates).unwrap();
    same_layout(&planned, &written);
    same_layout(
        &planned,
        Asset::from_source(Cursor::new(output)).unwrap().structure(),
    );
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
fn new_metadata_and_multiple_revisions_round_trip_and_empty_payloads_read_as_absent() {
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
        // An empty stream holds nothing to expose, so it reads as absent.
        let expected = (!data.is_empty()).then_some(data);
        assert_eq!(asset.xmp().unwrap().as_deref(), expected);
        assert_eq!(asset.jumbf().unwrap().as_deref(), expected);
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
            .update_segment(&mut output, ItemKind::Jumbf, vec![1; length])
            .is_err());
        assert_eq!(&before, output.get_ref());
    }
    let before = output.get_ref().clone();
    let range = payload_range(&structure, ItemKind::Jumbf);
    structure
        .update_segment(&mut output, ItemKind::Jumbf, vec![7; 32])
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
        .update_segment_in_place(ItemKind::Xmp, b"short".to_vec())
        .is_err());
    asset
        .update_segment_in_place(ItemKind::Xmp, b"<new/>".to_vec())
        .unwrap();
    assert_eq!(asset.xmp().unwrap().unwrap(), b"<new/>");
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
                .update_segment(&mut output, ItemKind::Jumbf, vec![1; 32])
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
            // There is no store to patch, and the document could not be edited anyway.
            assert!(asset
                .structure()
                .update_segment(&mut output, ItemKind::Jumbf, vec![0; 32])
                .is_err());
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
fn write_rejects_revision_overflow_without_touching_output() {
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
    assert_eq!(output.get_ref(), b"unchanged");
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

#[test]
fn every_kind_of_document_conforms() {
    use asset_io_core::testing::Conformance;

    let c = Conformance::new(asset_io_pdf::handler()).jumbf(Some(b"STORE".to_vec()));
    let finished = |mut pdf: Pdf| {
        pdf.finish("/Root 1 0 R /ID [<0102> <0304>] /Info 8 0 R");
        pdf.bytes
    };
    // Editable, with a registered store, other attachments and unknown entries.
    c.check("writable", &finished(writable_pdf()));
    // Has metadata, but the store is not registered in a name tree: reads, copies,
    // and refuses to edit.
    let mut plain = metadata_pdf();
    plain.finish("/Root 1 0 R");
    c.check("unregistered attachment", &plain.bytes);
    c.check("no metadata", &document(b"<< /Type /Catalog >>", ""));

    // Cross-reference streams, with and without predictors.
    for predictor in [false, true] {
        let mut pdf = writable_pdf();
        pdf.finish_stream("/Root 1 0 R", &[], predictor, false);
        c.check(&format!("xref stream, predictor {predictor}"), &pdf.bytes);
    }

    // A Flate-compressed XMP stream and a compressed catalog in an object stream.
    let mut pdf = Pdf::new();
    let encoded = deflate(b"1 0 << /Type /Catalog /Metadata 2 0 R >>");
    pdf.stream(
        3,
        &format!(
            "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {}",
            encoded.len()
        ),
        &encoded,
    );
    let xmp = deflate(b"<compressed-xmp/>");
    pdf.stream(
        2,
        &format!(
            "/Type /Metadata /Subtype /XML /Filter /FlateDecode /Length {}",
            xmp.len()
        ),
        &xmp,
    );
    pdf.finish_stream("/Root 1 0 R", &[(1, 3, 0)], true, false);
    c.check("compressed catalog and metadata", &pdf.bytes);

    // Forms: ordinary and unsigned are editable; populated signatures are not.
    c.check(
        "unsigned form",
        &form_pdf(
            b"<< /Fields [4 0 R] >>",
            &[(4, b"<< /FT /Tx /V (x) >>")],
            false,
        ),
    );
    c.check(
        "signed form",
        &form_pdf(
            b"<< /Fields [4 0 R] >>",
            &[(4, b"<< /FT /Sig /V << /Type /Sig >> >>")],
            false,
        ),
    );

    // Several revisions.
    let mut pdf = writable_pdf();
    let previous = pdf.finish("/Root 1 0 R");
    pdf.entries.clear();
    pdf.object(8, 0, b"<< /Producer (second) >>");
    pdf.finish(&format!("/Root 1 0 R /Prev {previous}"));
    c.check("two revisions", &pdf.bytes);
}
