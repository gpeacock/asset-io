use asset_io_c::*;
use std::{
    ffi::{c_void, CString},
    ptr,
};

struct Handle<T>(*mut T);
impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        assert_eq!(asset_io_free(self.0.cast()), 0);
    }
}
fn owned<T>(p: *mut T) -> Handle<T> {
    assert!(!p.is_null(), "{}", error());
    Handle(p)
}
fn bytes(p: *mut AssetIoBuffer) -> Vec<u8> {
    let mut n = 0;
    let data = asset_io_buffer_data(p, &mut n);
    assert!(!data.is_null());
    unsafe { std::slice::from_raw_parts(data, n).to_vec() }
}
fn error() -> String {
    let p = asset_io_last_error();
    if p.is_null() {
        return String::new();
    }
    let b = bytes(p);
    asset_io_free(p.cast());
    String::from_utf8(b).unwrap()
}
fn registry() -> Handle<AssetIoRegistry> {
    owned(asset_io_registry_new(1))
}
fn parse(r: &Handle<AssetIoRegistry>, data: &[u8]) -> Handle<AssetIoAsset> {
    owned(asset_io_parse_memory(
        r.0,
        ptr::null(),
        data.as_ptr(),
        data.len(),
    ))
}
fn inspect(a: *mut AssetIoAsset) -> serde_json::Value {
    let s = owned(asset_io_asset_structure(a));
    let b = owned(asset_io_structure_json(s.0));
    serde_json::from_slice(&bytes(b.0)).unwrap()
}
fn jumbf(ty: u8) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(33u32.to_be_bytes());
    b.extend(b"jumb");
    b.extend(25u32.to_be_bytes());
    b.extend(b"jumd");
    b.extend([ty; 16]);
    b.push(0);
    b
}
fn fixtures() -> Vec<&'static [u8]> {
    vec![
        #[cfg(feature = "jpeg")]
        &b"\xff\xd8\xff\xd9"[..],
        #[cfg(feature = "png")]
        &b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xaeB`\x82"[..],
    ]
}

#[test]
fn roundtrip_and_geometry() {
    let r = registry();
    for input in fixtures() {
        let a = parse(&r, input);
        let u = owned(asset_io_updates_new());
        let no_op = owned(asset_io_write_memory(a.0, u.0));
        assert_eq!(bytes(no_op.0), input);
        assert_eq!(
            asset_io_updates_metadata(u.0, 1, 1, b"<xmp/>".as_ptr(), 6),
            0
        );
        let store = jumbf(1);
        assert_eq!(
            asset_io_updates_metadata(u.0, 2, 1, store.as_ptr(), store.len()),
            0
        );
        let plan = owned(asset_io_plan(a.0, u.0));
        let planned_json = owned(asset_io_structure_json(plan.0));
        let planned: serde_json::Value = serde_json::from_slice(&bytes(planned_json.0)).unwrap();
        let output = owned(asset_io_write_memory(a.0, u.0));
        let b = parse(&r, &bytes(output.0));
        assert_eq!(planned, inspect(b.0));
        let xmp = owned(asset_io_metadata_bytes(b.0, 1));
        assert_eq!(bytes(xmp.0), b"<xmp/>");
        let j = owned(asset_io_metadata_bytes(b.0, 2));
        assert_eq!(bytes(j.0), store);
        for (i, e) in inspect(b.0)["entities"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let raw = owned(asset_io_entity_bytes(b.0, i, 0));
            assert!(!bytes(raw.0).is_empty());
            if e["kind"] == "jumbf" {
                let ty = owned(asset_io_entity_jumbf_type(b.0, i));
                assert_eq!(bytes(ty.0), vec![1; 16]);
            }
        }
        assert_eq!(asset_io_updates_remove_all(u.0), 0);
        let stripped = owned(asset_io_write_memory(b.0, u.0));
        let stripped = parse(&r, &bytes(stripped.0));
        assert!(inspect(stripped.0)["entities"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn errors_handles_features_and_lifetimes() {
    let r = registry();
    let formats = owned(asset_io_registry_formats(r.0));
    let f: Vec<String> = serde_json::from_slice(&bytes(formats.0)).unwrap();
    assert_eq!(f.iter().any(|s| s == "jpg"), cfg!(feature = "jpeg"));
    assert_eq!(f.iter().any(|s| s == "png"), cfg!(feature = "png"));
    assert_eq!(f.iter().any(|s| s == "heic"), cfg!(feature = "bmff"));
    assert_eq!(f.iter().any(|s| s == "webp"), cfg!(feature = "riff"));
    assert_eq!(f.iter().any(|s| s == "svg"), cfg!(feature = "svg"));
    assert_eq!(f.iter().any(|s| s == "pdf"), cfg!(feature = "pdf"));
    let empty = owned(asset_io_registry_new(0));
    let f = owned(asset_io_registry_formats(empty.0));
    assert_eq!(bytes(f.0), b"[]");
    assert!(asset_io_registry_new(3).is_null());
    assert!(error().starts_with("InvalidArgument:"));
    assert!(asset_io_parse_memory(ptr::null_mut(), ptr::null(), ptr::null(), 0).is_null());
    assert!(asset_io_registry_formats(f.0.cast()).is_null());
    assert!(asset_io_parse_memory(r.0, ptr::null(), ptr::null(), 1).is_null());
    assert!(asset_io_parse_memory(r.0, ptr::null(), ptr::null(), 0).is_null());
    let invalid = [255u8, 0];
    assert!(asset_io_parse_memory(r.0, invalid.as_ptr().cast(), ptr::null(), 0).is_null());
    assert!(asset_io_parse_memory(r.0, ptr::null(), ptr::dangling(), usize::MAX).is_null());
    let unsupported = CString::new("not-a-format").unwrap();
    assert!(asset_io_parse_memory(r.0, unsupported.as_ptr(), ptr::null(), 0).is_null());
    assert!(error().starts_with("UnsupportedFormat:"));
    let p = asset_io_updates_new();
    assert_eq!(asset_io_updates_metadata(p, 99, 1, ptr::null(), 0), -1);
    assert_eq!(asset_io_updates_metadata(p, 1, 99, ptr::null(), 0), -1);
    assert_eq!(asset_io_updates_metadata(p, 1, 1, ptr::null(), 1), -1);
    assert_eq!(asset_io_updates_as_update(p), -1);
    assert_eq!(asset_io_updates_remove_jumbf_type(p, ptr::null(), 0), -1);
    assert_eq!(asset_io_free(p.cast()), 0);
    assert_eq!(asset_io_free(p.cast()), -1);
    assert_eq!(asset_io_updates_remove_all(p), -1);
    assert_eq!(asset_io_free(ptr::null_mut()), 0);
    for input in fixtures() {
        let a = parse(&r, input);
        let all = owned(asset_io_read_range(a.0, 0, input.len() as u64));
        assert_eq!(bytes(all.0), input);
        let empty_range = owned(asset_io_read_range(a.0, input.len() as u64, 0));
        assert!(bytes(empty_range.0).is_empty());
        assert!(asset_io_read_range(a.0, 0, input.len() as u64 + 1).is_null());
        assert!(asset_io_read_range(a.0, u64::MAX, 1).is_null());
        let s = owned(asset_io_asset_structure(a.0));
        assert!(asset_io_entity_bytes(a.0, usize::MAX, 1).is_null());
        assert!(asset_io_metadata_bytes(a.0, 1).is_null());
        assert!(error().starts_with("NotFound:"));
        drop(a);
        let b = owned(asset_io_structure_json(s.0));
        assert!(!bytes(b.0).is_empty());
    }
}

struct Context {
    input: Vec<u8>,
    output: Vec<u8>,
    fail: bool,
    overread: bool,
    asset: *mut AssetIoAsset,
    reenter: bool,
    reads: usize,
}
extern "C" fn read(ctx: *mut c_void, offset: u64, out: *mut u8, len: usize) -> i64 {
    let c = unsafe { &mut *ctx.cast::<Context>() };
    c.reads += 1;
    if c.fail {
        return -1;
    }
    if c.overread {
        return len as i64 + 1;
    }
    let data = c.input.get(offset as usize..).unwrap_or(&[]);
    let n = len.min(data.len()).min(3);
    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), out, n);
    }
    n as i64
}
extern "C" fn chunk(
    ctx: *mut c_void,
    _segment: usize,
    role: u32,
    offset: u64,
    data: *const u8,
    len: usize,
) -> i32 {
    let c = unsafe { &mut *ctx.cast::<Context>() };
    if c.fail {
        return 1;
    }
    if c.reenter {
        // Exclusive borrow on asset must reject reentry rather than aliasing it.
        let p = asset_io_metadata_bytes(c.asset, 1);
        if !p.is_null() {
            return 2;
        }
    }
    if role > 2 || offset != c.output.len() as u64 {
        return 4;
    }
    c.output
        .extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    0
}
#[test]
fn callbacks_short_reads_failures_stream_and_reentry() {
    let r = registry();
    for input in fixtures() {
        let mut c = Context {
            input: input.to_vec(),
            output: vec![],
            fail: false,
            overread: false,
            asset: ptr::null_mut(),
            reenter: false,
            reads: 0,
        };
        let ctx = (&mut c as *mut Context).cast();
        let a = owned(asset_io_parse_source(
            r.0,
            ptr::null(),
            input.len() as u64,
            Some(read),
            ctx,
        ));
        c.asset = a.0;
        assert!(c.reads > 1);
        let u = owned(asset_io_updates_new());
        c.reenter = true;
        assert_eq!(
            asset_io_read_stream(a.0, Some(chunk), ctx),
            0,
            "{}",
            error()
        );
        assert_eq!(c.output, input);
        c.output.clear();
        assert_eq!(
            asset_io_write_stream(a.0, u.0, Some(chunk), ctx),
            0,
            "{}",
            error()
        );
        assert_eq!(c.output, input);
        c.output.clear();
        c.reenter = false;
        assert_eq!(
            asset_io_updates_metadata(u.0, 1, 1, b"<xmp/>".as_ptr(), 6),
            0
        );
        let expected = owned(asset_io_write_memory(a.0, u.0));
        assert_eq!(asset_io_write_stream(a.0, u.0, Some(chunk), ctx), 0);
        assert_eq!(c.output, bytes(expected.0));
        let memory_asset = parse(&r, input);
        c.fail = true;
        assert_eq!(asset_io_read_stream(a.0, Some(chunk), ctx), -1);
        // Reject the output callback independently of failures reading the source.
        assert_eq!(
            asset_io_write_stream(memory_asset.0, u.0, Some(chunk), ctx),
            -1
        );
        assert_eq!(asset_io_write_stream(a.0, u.0, Some(chunk), ctx), -1);
        assert!(
            asset_io_parse_source(r.0, ptr::null(), input.len() as u64, Some(read), ctx).is_null()
        );
        c.fail = false;
        c.overread = true;
        assert!(
            asset_io_parse_source(r.0, ptr::null(), input.len() as u64, Some(read), ctx).is_null()
        );
        assert!(asset_io_parse_source(r.0, ptr::null(), 0, None, ctx).is_null());
        assert_eq!(asset_io_write_stream(a.0, u.0, None, ctx), -1);
        assert_eq!(asset_io_read_stream(a.0, None, ctx), -1);
        c.overread = false;
        c.input.clear();
        assert!(
            asset_io_parse_source(r.0, ptr::null(), input.len() as u64, Some(read), ctx).is_null()
        );
        assert!(error().contains("ended before known size"));
    }
}

#[test]
fn enabled_container_fixtures_parse() {
    let r = registry();
    let fixtures: &[(&[u8], &str)] = &[
        #[cfg(feature = "jpeg")]
        (
            include_bytes!("../../tests/fixtures/Designer.jpeg"),
            "image/jpeg",
        ),
        #[cfg(feature = "png")]
        (
            include_bytes!("../../tests/fixtures/sample1.png"),
            "image/png",
        ),
        #[cfg(feature = "bmff")]
        (
            include_bytes!("../../tests/fixtures/sample1.heic"),
            "image/heif",
        ),
        #[cfg(feature = "riff")]
        (
            include_bytes!("../../tests/fixtures/sample1.webp"),
            "image/webp",
        ),
        #[cfg(feature = "svg")]
        (
            include_bytes!("../../tests/fixtures/sample1.svg"),
            "image/svg+xml",
        ),
        #[cfg(feature = "pdf")]
        (
            include_bytes!("../../tests/fixtures/pdf_metadata.pdf"),
            "application/pdf",
        ),
    ];
    for (input, media_type) in fixtures {
        let format = CString::new(*media_type).unwrap();
        let asset = asset_io_parse_memory(r.0, format.as_ptr(), input.as_ptr(), input.len());
        assert!(!asset.is_null(), "{media_type}: {}", error());
        let a = owned(asset);
        let geometry = inspect(a.0);
        assert_eq!(geometry["media_type"], *media_type);
        assert_eq!(geometry["size"], input.len() as u64);
        assert!(geometry["segments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| { s.get("box_name").is_some() }));
    }
}

#[test]
#[cfg(feature = "jpeg")]
fn typed_jumbf_scoping() {
    let r = registry();
    let a = parse(&r, b"\xff\xd8\xff\xd9");
    let u = owned(asset_io_updates_new());
    let first = jumbf(1);
    asset_io_updates_metadata(u.0, 2, 1, first.as_ptr(), first.len());
    let b = owned(asset_io_write_memory(a.0, u.0));
    let a = parse(&r, &bytes(b.0));
    let second = jumbf(2);
    asset_io_updates_metadata(u.0, 2, 1, second.as_ptr(), second.len());
    let b = owned(asset_io_write_memory(a.0, u.0));
    let a = parse(&r, &bytes(b.0));
    assert_eq!(inspect(a.0)["entities"].as_array().unwrap().len(), 2);
    assert!(asset_io_metadata_bytes(a.0, 2).is_null());
    assert!(error().starts_with("Ambiguous:"));
    let ty = [1u8; 16];
    assert_eq!(asset_io_updates_remove_jumbf_type(u.0, ty.as_ptr(), 16), 0);
    let b = owned(asset_io_write_memory(a.0, u.0));
    let a = parse(&r, &bytes(b.0));
    let store = owned(asset_io_metadata_bytes(a.0, 2));
    assert_eq!(bytes(store.0), second);
}
