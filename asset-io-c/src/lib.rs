//! Application C ABI. Handles are cimpl-tracked opaque identifiers, never Rust addresses.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use asset_io::{HandlerRegistry, Name, Role, Structure, Updates};
use cimpl::{
    box_tracked, bytes_or_return, cstr_or_return, deref_mut_or_return, deref_or_return,
    ptr_or_return,
};
use serde_json::{json, Value};
use std::{
    ffi::{c_char, c_void},
    io::{self, Cursor, Read, Seek, SeekFrom},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::Arc,
};

macro_rules! ok_or_return {
    ($result:expr, $failure:expr) => {
        cimpl::ok_or_return!($result, |value| value, $failure)
    };
}

/// Opaque registry of formats compiled into this library.
pub struct AssetIoRegistry(Arc<HandlerRegistry>);
/// Opaque parsed asset, retaining its source.
pub struct AssetIoAsset {
    structure: Arc<Structure>,
    source: Source,
}
/// Opaque immutable structure snapshot, independent of its asset/updates.
pub struct AssetIoStructure(Arc<Structure>);
/// Opaque update builder. Defaults to keeping all metadata.
pub struct AssetIoUpdates(Updates);
/// Opaque owned bytes (also used for UTF-8 JSON, without a terminating NUL).
pub struct AssetIoBuffer(Vec<u8>);

/// Read at absolute offset. Return bytes read, 0 for EOF, or a negative value for failure.
pub type AssetIoReadAt =
    Option<extern "C" fn(context: *mut c_void, offset: u64, data: *mut u8, len: usize) -> i64>;
/// Receive bytes in order, tagged with segment, role and absolute offset.
/// Tags describe the source for reads and the output for writes.
/// Return 0 after consuming the entire chunk, nonzero on failure.
pub type AssetIoChunk = Option<
    extern "C" fn(
        context: *mut c_void,
        segment: usize,
        role: u32,
        offset: u64,
        data: *const u8,
        len: usize,
    ) -> i32,
>;

enum Source {
    Memory(Cursor<Vec<u8>>),
    Callback {
        read: extern "C" fn(*mut c_void, u64, *mut u8, usize) -> i64,
        context: usize,
        size: u64,
        pos: u64,
    },
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Memory(c) => c.read(buf),
            Self::Callback {
                read,
                context,
                size,
                pos,
            } => {
                let len = (*size - (*pos).min(*size)).min(buf.len() as u64) as usize;
                if len == 0 {
                    return Ok(0);
                }
                let n = read(*context as *mut c_void, *pos, buf.as_mut_ptr(), len);
                if n < 0 || n as u64 > len as u64 {
                    return Err(io::Error::other(
                        "read-at callback failed or returned too many bytes",
                    ));
                }
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "read-at ended before known size",
                    ));
                }
                *pos += n as u64;
                Ok(n as usize)
            }
        }
    }
}

impl Seek for Source {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Memory(c) => c.seek(from),
            Self::Callback { size, pos, .. } => {
                let p = match from {
                    SeekFrom::Start(p) => p as i128,
                    SeekFrom::End(d) => *size as i128 + d as i128,
                    SeekFrom::Current(d) => *pos as i128 + d as i128,
                };
                if !(0..=u64::MAX as i128).contains(&p) {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"));
                }
                *pos = p as u64;
                Ok(*pos)
            }
        }
    }
}

#[derive(Debug)]
struct CoreError(asset_io::Error);
impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for CoreError {}
impl From<CoreError> for cimpl::Error {
    fn from(e: CoreError) -> Self {
        // Convert the original error to preserve its variant name.
        cimpl::Error::from_error(e.0)
    }
}

fn boundary<T>(failure: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(_) => {
            cimpl::Error::new("Panic", "Rust operation panicked").set_last();
            failure
        }
    }
}
fn invalid(message: &str) -> cimpl::Error {
    cimpl::Error::new("InvalidArgument", message)
}
fn buffer(bytes: Vec<u8>) -> *mut AssetIoBuffer {
    box_tracked!(AssetIoBuffer(bytes))
}
fn json_buffer(value: Value) -> *mut AssetIoBuffer {
    buffer(value.to_string().into_bytes())
}
fn range(r: asset_io::ByteRange) -> Value {
    json!({"offset": r.offset(), "length": r.len()})
}
fn role(r: Role) -> u32 {
    match r {
        Role::Header => 0,
        Role::Payload => 1,
        Role::Trailer => 2,
    }
}
fn name(n: Name) -> Value {
    match n {
        Name::None => json!({"type":"none"}),
        Name::Marker(n) => json!({"type":"marker", "value":n}),
        Name::FourCC(n) => json!({"type":"fourcc", "bytes":n}),
        Name::Id(n) => json!({"type":"id", "value":n}),
        _ => json!({"type":"future", "debug":format!("{n:?}")}),
    }
}
fn describe(s: &Structure) -> Value {
    json!({
        "schema": 1,
        "container": s.container_id(), "media_type": s.media_type(), "size": s.total_size(),
        "trailing": s.trailing().map(range),
        "segments": s.segments().iter().map(|v| json!({
            "kind":v.kind().as_str(), "name":name(v.name()), "extent":range(v.extent()),
            "box_name":s.box_name(v.name()),
            "regions":v.regions().map(|(r,b)|json!({"role":role(r),"range":range(b)})).collect::<Vec<_>>(),
            "subdivides":v.subdivides()
        })).collect::<Vec<_>>(),
        "entities": s.entity_list().map(|(k,e)|json!({
            "kind":k.as_str(), "label":e.label(), "media_type":e.media_type(), "size":e.len(),
            "jumbf_type":e.jumbf_type(),
            "parts":e.parts().map(|p|json!({"segment":p.segment().index(),"range":range(p.range())})).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "subassets":s.subassets().iter().map(|a|json!({
            "range":range(a.range()),"segment_start":a.segments().start,"segment_end":a.segments().end,
            "media_type":a.media_type(),"primary":a.is_primary()
        })).collect::<Vec<_>>(),
        "regions":s.regions().map(|r|json!({"segment":r.segment.index(),"role":role(r.role),"range":range(r.range)})).collect::<Vec<_>>()
    })
}

fn parse(reg: &HandlerRegistry, format: *const c_char, mut source: Source) -> *mut AssetIoAsset {
    let structure = if format.is_null() {
        ok_or_return!(
            reg.parse_detect(&mut source).map_err(CoreError),
            ptr::null_mut()
        )
    } else {
        let format = cstr_or_return!(format, ptr::null_mut());
        ok_or_return!(
            reg.parse(&format, &mut source).map_err(CoreError),
            ptr::null_mut()
        )
    };
    box_tracked!(AssetIoAsset {
        structure: Arc::new(structure),
        source
    })
}

/// ABI version, independent of enabled format features.
#[no_mangle]
pub extern "C" fn asset_io_abi_version() -> u32 {
    1
}

/// Create a builtin registry (1), or an empty registry (0). Other values fail.
#[no_mangle]
pub extern "C" fn asset_io_registry_new(builtin: u32) -> *mut AssetIoRegistry {
    boundary(ptr::null_mut(), || {
        let r = match builtin {
            0 => Arc::new(HandlerRegistry::new()),
            1 => HandlerRegistry::builtin(),
            _ => {
                invalid("builtin must be 0 or 1").set_last();
                return ptr::null_mut();
            }
        };
        box_tracked!(AssetIoRegistry(r))
    })
}

/// Sorted JSON array of supported format strings (extensions, MIME types, aliases).
#[no_mangle]
pub extern "C" fn asset_io_registry_formats(registry: *mut AssetIoRegistry) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let r = deref_or_return!(registry, AssetIoRegistry, ptr::null_mut());
        json_buffer(json!(r.0.formats()))
    })
}

/// Copy input bytes and parse. NULL format detects; otherwise UTF-8 extension/MIME.
/// NULL data is allowed only when len is zero.
#[no_mangle]
pub extern "C" fn asset_io_parse_memory(
    registry: *mut AssetIoRegistry,
    format: *const c_char,
    data: *const u8,
    len: usize,
) -> *mut AssetIoAsset {
    boundary(ptr::null_mut(), || {
        let r = deref_or_return!(registry, AssetIoRegistry, ptr::null_mut());
        let bytes = if len == 0 {
            &[]
        } else {
            bytes_or_return!(data, len, "data", ptr::null_mut())
        };
        parse(&r.0, format, Source::Memory(Cursor::new(bytes.to_vec())))
    })
}

/// Parse without copying the whole input. Callback/context must outlive the asset.
#[no_mangle]
pub extern "C" fn asset_io_parse_source(
    registry: *mut AssetIoRegistry,
    format: *const c_char,
    size: u64,
    read: AssetIoReadAt,
    context: *mut c_void,
) -> *mut AssetIoAsset {
    boundary(ptr::null_mut(), || {
        let r = deref_or_return!(registry, AssetIoRegistry, ptr::null_mut());
        let read = ok_or_return!(
            read.ok_or_else(|| invalid("read callback is required")),
            ptr::null_mut()
        );
        parse(
            &r.0,
            format,
            Source::Callback {
                read,
                context: context as usize,
                size,
                pos: 0,
            },
        )
    })
}

/// Obtain an owned structure snapshot. It survives freeing the asset.
#[no_mangle]
pub extern "C" fn asset_io_asset_structure(asset: *mut AssetIoAsset) -> *mut AssetIoStructure {
    boundary(ptr::null_mut(), || {
        let a = deref_or_return!(asset, AssetIoAsset, ptr::null_mut());
        box_tracked!(AssetIoStructure(a.structure.clone()))
    })
}

/// Lossless schema-v1 JSON inspection (see README); binary names/UUIDs are byte arrays.
#[no_mangle]
pub extern "C" fn asset_io_structure_json(structure: *mut AssetIoStructure) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let s = deref_or_return!(structure, AssetIoStructure, ptr::null_mut());
        json_buffer(describe(&s.0))
    })
}

/// Read a source range into an owned buffer, subject to the core's in-memory read limit.
#[no_mangle]
pub extern "C" fn asset_io_read_range(
    asset: *mut AssetIoAsset,
    offset: u64,
    len: u64,
) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, ptr::null_mut());
        let range = ok_or_return!(
            asset_io::ByteRange::new(offset, len).map_err(CoreError),
            ptr::null_mut()
        );
        let s = a.structure.clone();
        buffer(ok_or_return!(
            s.read_range(&mut a.source, range).map_err(CoreError),
            ptr::null_mut()
        ))
    })
}

/// Stream every source byte once through the tagged callback, with bounded memory.
/// role: 0=header, 1=payload, 2=trailer. Nonzero callback results stop the read.
#[no_mangle]
pub extern "C" fn asset_io_read_stream(
    asset: *mut AssetIoAsset,
    chunk: AssetIoChunk,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, -1);
        let callback = ok_or_return!(
            chunk.ok_or_else(|| invalid("chunk callback is required")),
            -1
        );
        let s = a.structure.clone();
        let mut sink = |c: asset_io::Chunk<'_>| deliver(callback, context, c);
        ok_or_return!(s.read(&mut a.source, &mut sink).map_err(CoreError), -1);
        0
    })
}

/// Extract entity by declaration-order index. decoded=0 stored parts; 1 container-decoded.
#[no_mangle]
pub extern "C" fn asset_io_entity_bytes(
    asset: *mut AssetIoAsset,
    index: usize,
    decoded: u32,
) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, ptr::null_mut());
        let s = a.structure.clone();
        let e = ok_or_return!(
            s.entity_list()
                .nth(index)
                .ok_or_else(|| invalid("entity index out of range")),
            ptr::null_mut()
        )
        .1;
        let result = match decoded {
            0 => s.read_entity(&mut a.source, e),
            1 => s.read_decoded(&mut a.source, e),
            _ => {
                invalid("decoded must be 0 or 1").set_last();
                return ptr::null_mut();
            }
        };
        buffer(ok_or_return!(result.map_err(CoreError), ptr::null_mut()))
    })
}

/// Extract XMP (kind=1) or logical JUMBF store (kind=2).
/// Absence is a NotFound error, not an empty buffer; ambiguity remains an error.
#[no_mangle]
pub extern "C" fn asset_io_metadata_bytes(
    asset: *mut AssetIoAsset,
    kind: u32,
) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, ptr::null_mut());
        let s = a.structure.clone();
        let result = match kind {
            1 => s.xmp(&mut a.source),
            2 => s.store(&mut a.source),
            _ => {
                invalid("metadata kind must be 1 (XMP) or 2 (JUMBF)").set_last();
                return ptr::null_mut();
            }
        };
        let bytes = ok_or_return!(result.map_err(CoreError), ptr::null_mut());
        buffer(ok_or_return!(
            bytes.ok_or_else(|| cimpl::Error::new("NotFound", "metadata is absent")),
            ptr::null_mut()
        ))
    })
}

/// Read a JUMBF entity's type as 16 bytes; error if not known/a superbox.
#[no_mangle]
pub extern "C" fn asset_io_entity_jumbf_type(
    asset: *mut AssetIoAsset,
    index: usize,
) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, ptr::null_mut());
        let s = a.structure.clone();
        let e = ok_or_return!(
            s.entity_list()
                .nth(index)
                .ok_or_else(|| invalid("entity index out of range")),
            ptr::null_mut()
        )
        .1;
        let ty = ok_or_return!(
            s.jumbf_type(&mut a.source, e).map_err(CoreError),
            ptr::null_mut()
        );
        buffer(
            ok_or_return!(
                ty.ok_or_else(|| cimpl::Error::new("NotFound", "JUMBF type is absent")),
                ptr::null_mut()
            )
            .to_vec(),
        )
    })
}

#[no_mangle]
pub extern "C" fn asset_io_updates_new() -> *mut AssetIoUpdates {
    boundary(ptr::null_mut(), || {
        box_tracked!(AssetIoUpdates(Updates::new()))
    })
}

/// kind: 1=XMP, 2=JUMBF. action: 0=keep, 1=set, 2=remove.
/// Data is copied for set; ignored otherwise. JUMBF set automatically scopes to its superbox type.
#[no_mangle]
pub extern "C" fn asset_io_updates_metadata(
    updates: *mut AssetIoUpdates,
    kind: u32,
    action: u32,
    data: *const u8,
    len: usize,
) -> i32 {
    boundary(-1, || {
        let mut u = deref_mut_or_return!(updates, AssetIoUpdates, -1);
        if !matches!(kind, 1 | 2) || action > 2 {
            invalid("invalid metadata kind/action").set_last();
            return -1;
        }
        let data = if action == 1 && len != 0 {
            bytes_or_return!(data, len, "data", -1).to_vec()
        } else {
            Vec::new()
        };
        let old = std::mem::take(&mut u.0);
        u.0 = match (kind, action) {
            (1, 0) => old.keep_xmp(),
            (1, 1) => old.set_xmp(data),
            (1, 2) => old.remove_xmp(),
            (2, 0) => old.keep_jumbf(),
            (2, 1) => old.set_jumbf(data),
            (2, 2) => old.remove_jumbf(),
            _ => unreachable!(),
        };
        0
    })
}

/// Remove only JUMBF of the supplied 16-byte type. len must equal 16.
#[no_mangle]
pub extern "C" fn asset_io_updates_remove_jumbf_type(
    updates: *mut AssetIoUpdates,
    uuid: *const u8,
    len: usize,
) -> i32 {
    boundary(-1, || {
        let mut u = deref_mut_or_return!(updates, AssetIoUpdates, -1);
        if len != 16 {
            invalid("JUMBF type must be 16 bytes").set_last();
            return -1;
        }
        let bytes = bytes_or_return!(uuid, len, "uuid", -1);
        let mut ty = [0; 16];
        ty.copy_from_slice(bytes);
        u.0 = std::mem::take(&mut u.0).remove_jumbf_of(ty);
        0
    })
}

/// Apply remove-all semantics including container-specific metadata defaults.
#[no_mangle]
pub extern "C" fn asset_io_updates_remove_all(updates: *mut AssetIoUpdates) -> i32 {
    boundary(-1, || {
        let mut u = deref_mut_or_return!(updates, AssetIoUpdates, -1);
        u.0 = Updates::remove_all();
        0
    })
}

/// Mark the currently set JUMBF as an update manifest (core BMFF semantics).
#[no_mangle]
pub extern "C" fn asset_io_updates_as_update(updates: *mut AssetIoUpdates) -> i32 {
    boundary(-1, || {
        let mut u = deref_mut_or_return!(updates, AssetIoUpdates, -1);
        if !matches!(u.0.jumbf(), asset_io::MetadataUpdate::Set(_)) {
            invalid("set JUMBF before as_update").set_last();
            return -1;
        }
        u.0 = std::mem::take(&mut u.0).as_update();
        0
    })
}

/// Inspect planned OUTPUT geometry without writing or retaining borrowed plan steps.
#[no_mangle]
pub extern "C" fn asset_io_plan(
    asset: *mut AssetIoAsset,
    updates: *mut AssetIoUpdates,
) -> *mut AssetIoStructure {
    boundary(ptr::null_mut(), || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, ptr::null_mut());
        let u = deref_or_return!(updates, AssetIoUpdates, ptr::null_mut());
        let s = a.structure.clone();
        let planned = ok_or_return!(
            s.plan(&mut a.source, &u.0).map_err(CoreError),
            ptr::null_mut()
        );
        box_tracked!(AssetIoStructure(Arc::new(planned)))
    })
}

/// Write the entire updated asset into a new owned buffer.
#[no_mangle]
pub extern "C" fn asset_io_write_memory(
    asset: *mut AssetIoAsset,
    updates: *mut AssetIoUpdates,
) -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, ptr::null_mut());
        let u = deref_or_return!(updates, AssetIoUpdates, ptr::null_mut());
        let s = a.structure.clone();
        let mut bytes = Vec::new();
        ok_or_return!(
            s.write(&mut a.source, &mut bytes, &u.0, &mut asset_io::NullSink)
                .map_err(CoreError),
            ptr::null_mut()
        );
        buffer(bytes)
    })
}

/// Stream output once through the tagged callback. No output-sized allocation.
/// role: 0=header, 1=payload, 2=trailer. On failure output may be partial.
#[no_mangle]
pub extern "C" fn asset_io_write_stream(
    asset: *mut AssetIoAsset,
    updates: *mut AssetIoUpdates,
    chunk: AssetIoChunk,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let mut a = deref_mut_or_return!(asset, AssetIoAsset, -1);
        let u = deref_or_return!(updates, AssetIoUpdates, -1);
        let callback = ok_or_return!(
            chunk.ok_or_else(|| invalid("chunk callback is required")),
            -1
        );
        let s = a.structure.clone();
        let mut sink = |c: asset_io::Chunk<'_>| deliver(callback, context, c);
        ok_or_return!(
            s.write(&mut a.source, &mut io::sink(), &u.0, &mut sink)
                .map_err(CoreError),
            -1
        );
        0
    })
}

fn deliver(
    callback: extern "C" fn(*mut c_void, usize, u32, u64, *const u8, usize) -> i32,
    context: *mut c_void,
    c: asset_io::Chunk<'_>,
) -> asset_io::Result<()> {
    if callback(
        context,
        c.segment.index(),
        role(c.role),
        c.offset,
        c.data.as_ptr(),
        c.data.len(),
    ) == 0
    {
        Ok(())
    } else {
        Err(asset_io::Error::Io(io::Error::other(
            "chunk callback failed",
        )))
    }
}

/// Borrow buffer storage until free. out_len is required and zeroed on handle failure.
/// A successful empty buffer has a non-NULL pointer and length zero.
#[no_mangle]
pub extern "C" fn asset_io_buffer_data(
    buffer: *mut AssetIoBuffer,
    out_len: *mut usize,
) -> *const u8 {
    boundary(ptr::null(), || {
        ptr_or_return!(out_len, ptr::null());
        unsafe {
            *out_len = 0;
        }
        let b = deref_or_return!(buffer, AssetIoBuffer, ptr::null());
        unsafe {
            *out_len = b.0.len();
        }
        b.0.as_ptr()
    })
}

/// Copy thread-local last error to owned UTF-8 bytes; NULL if none. Does not clear it.
#[no_mangle]
pub extern "C" fn asset_io_last_error() -> *mut AssetIoBuffer {
    boundary(ptr::null_mut(), || {
        cimpl::Error::last_message()
            .map(|e| buffer(e.into_bytes()))
            .unwrap_or(ptr::null_mut())
    })
}

/// Release any handle/buffer created by THIS library. NULL succeeds; invalid/double free fails.
#[no_mangle]
pub extern "C" fn asset_io_free(handle: *mut c_void) -> i32 {
    boundary(-1, || cimpl::cimpl_free(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_and_panic_boundary() {
        let p = buffer(Vec::new());
        let mut len = usize::MAX;
        assert!(!asset_io_buffer_data(p, &mut len).is_null());
        assert_eq!(len, 0);
        assert!(asset_io_buffer_data(p, ptr::null_mut()).is_null());
        assert_eq!(asset_io_free(p.cast()), 0);
        assert!(asset_io_buffer_data(p, &mut len).is_null());
        assert_eq!(len, 0);
        assert_eq!(boundary(-1, || panic!("test unwind")), -1);
        assert!(cimpl::Error::last_message().unwrap().starts_with("Panic:"));
    }

    #[test]
    fn active_free_defers_destruction_and_invalidates_id() {
        let p = asset_io_updates_new();
        let guard = cimpl::checkout_exclusive::<AssetIoUpdates>(p).unwrap();
        assert_eq!(asset_io_free(p.cast()), 0);
        assert_eq!(asset_io_updates_remove_all(p), -1);
        assert!(matches!(guard.0.xmp(), asset_io::MetadataUpdate::Keep));
        drop(guard);
        assert_eq!(asset_io_free(p.cast()), -1);
    }
}
