//! BMFF (ISO Base Media File Format) container I/O implementation
//!
//! Supports multiple media types: HEIC, HEIF, AVIF, MP4, M4A, MOV
//!
//! Reference: ISO/IEC 14496-12:2022

use super::{ContainerIO, ContainerKind};
use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind},
    structure::Structure,
    MediaType, Updates,
};
use atree::{Arena, Token};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
};

// BMFF constants
const HEADER_SIZE: u64 = 8; // 4 byte type + 4 byte size
const HEADER_SIZE_LARGE: u64 = 16; // 4 byte type + 4 byte size + 8 byte large size

const C2PA_UUID: [u8; 16] = [
    0xd8, 0xfe, 0xc3, 0xd6, 0x1b, 0x0e, 0x48, 0x3c, 0x92, 0x97, 0x58, 0x28, 0x87, 0x7e, 0xc4, 0x81,
];

const XMP_UUID: [u8; 16] = [
    0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac,
];

// ISO IEC 14496-12_2022 FullBoxes
const FULL_BOX_TYPES: &[&str; 80] = &[
    "pdin", "mvhd", "tkhd", "mdhd", "hdlr", "nmhd", "elng", "stsd", "stdp", "stts", "ctts", "cslg",
    "stss", "stsh", "stdp", "elst", "dref", "stsz", "stz2", "stsc", "stco", "co64", "padb", "subs",
    "saiz", "saio", "mehd", "trex", "mfhd", "tfhd", "trun", "tfra", "mfro", "tfdt", "leva", "trep",
    "assp", "sbgp", "sgpd", "csgp", "cprt", "tsel", "kind", "meta", "xml ", "bxml", "iloc", "pitm",
    "ipro", "infe", "iinf", "iref", "ipma", "schm", "fiin", "fpar", "fecr", "gitn", "fire", "stri",
    "stsg", "stvi", "csch", "sidx", "ssix", "prft", "srpp", "vmhd", "smhd", "srat", "chnl", "dmix",
    "txtC", "mime", "uri ", "uriI", "hmhd", "sthd", "vvhd", "medc",
];

/// Box type enum for common BMFF boxes
macro_rules! boxtype {
    ($( $name:ident => $value:expr ),*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum BoxType {
            $( $name, )*
            UnknownBox(u32),
        }

        impl From<u32> for BoxType {
            fn from(t: u32) -> BoxType {
                match t {
                    $( $value => BoxType::$name, )*
                    _ => BoxType::UnknownBox(t),
                }
            }
        }

        impl From<BoxType> for u32 {
            fn from(t: BoxType) -> u32 {
                match t {
                    $( BoxType::$name => $value, )*
                    BoxType::UnknownBox(t) => t,
                }
            }
        }
    }
}

boxtype! {
    Empty => 0x0000_0000,
    UuidBox => 0x75756964,
    FtypBox => 0x66747970,
    MvhdBox => 0x6d766864,
    MfhdBox => 0x6d666864,
    FreeBox => 0x66726565,
    MdatBox => 0x6d646174,
    MoovBox => 0x6d6f6f76,
    MvexBox => 0x6d766578,
    MehdBox => 0x6d656864,
    TrexBox => 0x74726578,
    EmsgBox => 0x656d7367,
    MoofBox => 0x6d6f6f66,
    TkhdBox => 0x746b6864,
    TfhdBox => 0x74666864,
    EdtsBox => 0x65647473,
    MdiaBox => 0x6d646961,
    ElstBox => 0x656c7374,
    MfraBox => 0x6d667261,
    MdhdBox => 0x6d646864,
    HdlrBox => 0x68646c72,
    MinfBox => 0x6d696e66,
    VmhdBox => 0x766d6864,
    StblBox => 0x7374626c,
    StsdBox => 0x73747364,
    SttsBox => 0x73747473,
    CttsBox => 0x63747473,
    StssBox => 0x73747373,
    StscBox => 0x73747363,
    StszBox => 0x7374737A,
    StcoBox => 0x7374636F,
    Co64Box => 0x636F3634,
    TrakBox => 0x7472616b,
    TrafBox => 0x74726166,
    TrefBox => 0x74726566,
    TregBox => 0x74726567,
    TrunBox => 0x7472756E,
    UdtaBox => 0x75647461,
    DinfBox => 0x64696e66,
    DrefBox => 0x64726566,
    UrlBox  => 0x75726C20,
    SmhdBox => 0x736d6864,
    Avc1Box => 0x61766331,
    AvcCBox => 0x61766343,
    Hev1Box => 0x68657631,
    HvcCBox => 0x68766343,
    Mp4aBox => 0x6d703461,
    EsdsBox => 0x65736473,
    Tx3gBox => 0x74783367,
    VpccBox => 0x76706343,
    Vp09Box => 0x76703039,
    MetaBox => 0x6D657461,
    SchiBox => 0x73636869,
    IlocBox => 0x696C6F63,
    MfroBox => 0x6d66726f,
    TfraBox => 0x74667261,
    SaioBox => 0x7361696f
}

/// Lightweight box header for efficient parsing
struct BoxHeaderLite {
    pub name: BoxType,
    pub size: u64,
    pub fourcc: String,
    pub large_size: bool,
}

impl BoxHeaderLite {
    pub fn new(name: BoxType, size: u64, fourcc: &str) -> Self {
        Self {
            name,
            size,
            fourcc: fourcc.to_string(),
            large_size: false,
        }
    }

    pub fn read<R: Read + Seek + ?Sized>(reader: &mut R) -> Result<Self> {
        let box_start = reader.stream_position()?;

        // Create and read to buf.
        let mut buf = [0u8; 8]; // 8 bytes for box header.
        reader.read_exact(&mut buf)?;

        // Get size.
        let mut s = [0u8; 4];
        s.clone_from_slice(&buf[0..4]);
        let size = u32::from_be_bytes(s);

        // Get box type string.
        let mut t = [0u8; 4];
        t.clone_from_slice(&buf[4..8]);
        let fourcc = String::from_utf8_lossy(&buf[4..8]).to_string();
        let typ = u32::from_be_bytes(t);

        // Get largesize if size is 1
        if size == 1 {
            reader.read_exact(&mut buf)?;
            let largesize = u64::from_be_bytes(buf);

            Ok(BoxHeaderLite {
                name: BoxType::from(typ),
                size: largesize,
                fourcc,
                large_size: true,
            })
        } else if size == 0 {
            // special case to indicate the size goes to the end of the file
            let current_pos = reader.stream_position()?;
            reader.seek(SeekFrom::End(0))?;
            let end_of_stream = reader.stream_position()?;
            reader.seek(SeekFrom::Start(current_pos))?;
            let actual_size = end_of_stream - box_start;

            Ok(BoxHeaderLite {
                name: BoxType::from(typ),
                size: actual_size,
                fourcc,
                large_size: false,
            })
        } else {
            Ok(BoxHeaderLite {
                name: BoxType::from(typ),
                size: size as u64,
                fourcc,
                large_size: false,
            })
        }
    }

    pub fn write<W: Write>(&self, writer: &mut W) -> Result<u64> {
        if self.size > u32::MAX as u64 {
            writer.write_u32::<BigEndian>(1)?;
            writer.write_u32::<BigEndian>(self.name.into())?;
            writer.write_u64::<BigEndian>(self.size)?;
            Ok(16)
        } else {
            writer.write_u32::<BigEndian>(self.size as u32)?;
            writer.write_u32::<BigEndian>(self.name.into())?;
            Ok(8)
        }
    }
}

/// Box information stored in the tree structure
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BoxInfo {
    path: String,
    parent: Option<Token>,
    pub offset: u64,
    pub size: u64,
    box_type: BoxType,
    user_type: Option<Vec<u8>>,
    version: Option<u8>,
    flags: Option<u32>,
}

fn read_box_header_ext<R: Read + Seek + ?Sized>(reader: &mut R) -> Result<(u8, u32)> {
    let version = reader.read_u8()?;
    let flags = reader.read_u24::<BigEndian>()?;
    Ok((version, flags))
}

fn write_box_header_ext<W: Write>(w: &mut W, v: u8, f: u32) -> Result<u64> {
    w.write_u8(v)?;
    w.write_u24::<BigEndian>(f)?;
    Ok(4)
}

fn write_box_uuid_extension<W: Write>(w: &mut W, uuid: &[u8; 16]) -> Result<u64> {
    w.write_all(uuid)?;
    Ok(16)
}

/// Write a C2PA UUID box with purpose, merkle data, and JUMBF content
pub(crate) fn write_c2pa_box<W: Write>(
    w: &mut W,
    data: &[u8],
    purpose: &str,
    merkle_data: &[u8],
    merkle_offset: u64,
) -> Result<()> {
    #[allow(dead_code)]
    const MANIFEST: &str = "manifest";
    const MERKLE: &str = "merkle";

    let purpose_size = purpose.len() + 1;

    let box_size = if purpose == MERKLE {
        merkle_data.len()
    } else {
        8 // merkle offset (u64)
    };
    let size = 8 + 16 + 4 + purpose_size + box_size + data.len(); // header + UUID + version/flags + purpose + merkle + data
    let bh = BoxHeaderLite::new(BoxType::UuidBox, size as u64, "uuid");

    // write out header
    bh.write(w)?;

    // write out c2pa extension UUID
    write_box_uuid_extension(w, &C2PA_UUID)?;

    // write out version and flags
    let version: u8 = 0;
    let flags: u32 = 0;
    write_box_header_ext(w, version, flags)?;

    // write with appropriate purpose
    w.write_all(purpose.as_bytes())?;
    w.write_u8(0)?; // null terminator

    if purpose == MERKLE {
        // write merkle cbor
        w.write_all(merkle_data)?;
    } else {
        // write merkle offset
        w.write_u64::<BigEndian>(merkle_offset)?;
    }

    // write out data
    w.write_all(data)?;

    Ok(())
}

/// Write an XMP UUID box
fn write_xmp_box<W: Write>(w: &mut W, data: &[u8]) -> Result<()> {
    let size = 8 + 16 + data.len(); // header + UUID + data (no version/flags for XMP)
    let bh = BoxHeaderLite::new(BoxType::UuidBox, size as u64, "uuid");

    // write out header
    bh.write(w)?;

    // write out XMP extension UUID
    write_box_uuid_extension(w, &XMP_UUID)?;

    // write out data
    w.write_all(data)?;

    Ok(())
}

fn box_start<R: Read + Seek + ?Sized>(reader: &mut R, is_large: bool) -> Result<u64> {
    if is_large {
        Ok(reader.stream_position()? - HEADER_SIZE_LARGE)
    } else {
        Ok(reader.stream_position()? - HEADER_SIZE)
    }
}

fn skip_bytes_to<R: Read + Seek + ?Sized>(reader: &mut R, pos: u64) -> Result<u64> {
    let pos = reader.seek(SeekFrom::Start(pos))?;
    Ok(pos)
}

fn add_token_to_cache(bmff_path_map: &mut HashMap<String, Vec<Token>>, path: String, token: Token) {
    if let Some(token_list) = bmff_path_map.get_mut(&path) {
        token_list.push(token);
    } else {
        let token_list = vec![token];
        bmff_path_map.insert(path, token_list);
    }
}

fn path_from_token(bmff_tree: &Arena<BoxInfo>, current_node_token: &Token) -> Result<String> {
    let ancestors = current_node_token.ancestors(bmff_tree);
    let mut path = bmff_tree[*current_node_token].data.path.clone();

    for parent in ancestors {
        path = format!("{}/{}", parent.data.path, path);
    }

    if path.is_empty() {
        path = "/".to_string();
    }

    Ok(path)
}

/// Build a tree structure representing the BMFF box hierarchy
pub(crate) fn build_bmff_tree<R: Read + Seek + ?Sized>(
    reader: &mut R,
    end: u64,
    bmff_tree: &mut Arena<BoxInfo>,
    current_node: &Token,
    bmff_path_map: &mut HashMap<String, Vec<Token>>,
) -> Result<()> {
    let start = reader.stream_position()?;

    let mut current = start;
    while current < end {
        // Get box header.
        let header = BoxHeaderLite::read(reader)
            .map_err(|err| Error::InvalidFormat(format!("Bad BMFF: {}", err)))?;

        // Break if size zero BoxHeader
        let s = header.size;
        if s == 0 {
            break;
        }

        // Match and parse the supported atom boxes.
        match header.name {
            BoxType::UuidBox => {
                let start = box_start(reader, header.large_size)?;

                let mut extended_type = [0u8; 16]; // 16 bytes of UUID
                reader.read_exact(&mut extended_type)?;

                let (version, flags) = read_box_header_ext(reader)?;

                let b = BoxInfo {
                    path: header.fourcc.clone(),
                    offset: start,
                    size: s,
                    box_type: BoxType::UuidBox,
                    parent: Some(*current_node),
                    user_type: Some(extended_type.to_vec()),
                    version: Some(version),
                    flags: Some(flags),
                };

                let new_token = current_node.append(bmff_tree, b);

                let path = path_from_token(bmff_tree, &new_token)?;
                add_token_to_cache(bmff_path_map, path, new_token);

                // position seek pointer
                skip_bytes_to(reader, start + s)?;
            }
            // container box types
            BoxType::MoovBox
            | BoxType::TrakBox
            | BoxType::MdiaBox
            | BoxType::MinfBox
            | BoxType::StblBox
            | BoxType::MoofBox
            | BoxType::TrafBox
            | BoxType::EdtsBox
            | BoxType::UdtaBox
            | BoxType::DinfBox
            | BoxType::TrefBox
            | BoxType::TregBox
            | BoxType::MvexBox
            | BoxType::MfraBox
            | BoxType::MetaBox
            | BoxType::SchiBox => {
                let start = box_start(reader, header.large_size)?;

                let b = if FULL_BOX_TYPES.contains(&header.fourcc.as_str()) {
                    let (version, flags) = read_box_header_ext(reader)?; // box extensions
                    BoxInfo {
                        path: header.fourcc.clone(),
                        offset: start,
                        size: s,
                        box_type: header.name,
                        parent: Some(*current_node),
                        user_type: None,
                        version: Some(version),
                        flags: Some(flags),
                    }
                } else {
                    BoxInfo {
                        path: header.fourcc.clone(),
                        offset: start,
                        size: s,
                        box_type: header.name,
                        parent: Some(*current_node),
                        user_type: None,
                        version: None,
                        flags: None,
                    }
                };

                let new_token = bmff_tree.new_node(b);
                current_node
                    .append_node(bmff_tree, new_token)
                    .map_err(|_err| Error::InvalidFormat("Bad BMFF Graph".to_string()))?;

                let path = path_from_token(bmff_tree, &new_token)?;
                add_token_to_cache(bmff_path_map, path, new_token);

                // consume all sub-boxes
                let mut current = reader.stream_position()?;
                let end = start + s;
                while current < end {
                    build_bmff_tree(reader, end, bmff_tree, &new_token, bmff_path_map)?;
                    current = reader.stream_position()?;
                }

                // position seek pointer
                skip_bytes_to(reader, start + s)?;
            }
            _ => {
                let start = box_start(reader, header.large_size)?;

                let b = if FULL_BOX_TYPES.contains(&header.fourcc.as_str()) {
                    let (version, flags) = read_box_header_ext(reader)?; // box extensions
                    BoxInfo {
                        path: header.fourcc.clone(),
                        offset: start,
                        size: s,
                        box_type: header.name,
                        parent: Some(*current_node),
                        user_type: None,
                        version: Some(version),
                        flags: Some(flags),
                    }
                } else {
                    BoxInfo {
                        path: header.fourcc.clone(),
                        offset: start,
                        size: s,
                        box_type: header.name,
                        parent: Some(*current_node),
                        user_type: None,
                        version: None,
                        flags: None,
                    }
                };

                let new_token = current_node.append(bmff_tree, b);

                let path = path_from_token(bmff_tree, &new_token)?;
                add_token_to_cache(bmff_path_map, path, new_token);

                // position seek pointer
                skip_bytes_to(reader, start + s)?;
            }
        }
        current = reader.stream_position()?;
    }

    Ok(())
}

/// Get UUID box by UUID and optional purpose
#[allow(dead_code)]
fn get_uuid_token(
    bmff_tree: &Arena<BoxInfo>,
    bmff_map: &HashMap<String, Vec<Token>>,
    uuid: &[u8; 16],
) -> Result<Token> {
    if let Some(uuid_list) = bmff_map.get("/uuid") {
        for uuid_token in uuid_list {
            let box_info = &bmff_tree[*uuid_token];

            // make sure it is UUID box
            if box_info.data.box_type == BoxType::UuidBox {
                if let Some(found_uuid) = &box_info.data.user_type {
                    // make sure uuids match
                    if uuid == found_uuid.as_slice() {
                        return Ok(*uuid_token);
                    }
                }
            }
        }
    }
    Err(Error::InvalidFormat("UUID box not found".to_string()))
}

/// Detect media type from ftyp box
fn detect_media_type_from_ftyp(major_brand: &[u8]) -> MediaType {
    match major_brand {
        b"heic" | b"heix" | b"heim" | b"heis" => MediaType::Heic,
        b"avif" | b"avis" => MediaType::Avif,
        b"mif1" | b"msf1" => MediaType::Heif,
        b"isom" | b"mp41" | b"mp42" => MediaType::Mp4Video,
        b"M4A " | b"M4B " => MediaType::Mp4Audio,
        b"qt  " => MediaType::QuickTime,
        _ => MediaType::Mp4Video, // Default fallback
    }
}

/// BMFF container I/O implementation
pub struct BmffIO;

impl BmffIO {
    pub fn new() -> Self {
        Self
    }

    #[allow(dead_code)]
    pub fn container_type() -> ContainerKind {
        ContainerKind::Bmff
    }

    pub fn supported_media_types() -> &'static [MediaType] {
        &[
            MediaType::Heic,
            MediaType::Heif,
            MediaType::Avif,
            MediaType::Mp4Video,
            MediaType::Mp4Audio,
            MediaType::QuickTime,
        ]
    }

    pub fn extensions() -> &'static [&'static str] {
        &["heic", "heif", "avif", "mp4", "m4a", "m4v", "mov"]
    }

    pub fn mime_types() -> &'static [&'static str] {
        &[
            "image/heic",
            "image/heif",
            "image/avif",
            "video/mp4",
            "audio/mp4",
            "video/quicktime",
            "video/x-m4v",
            "application/mp4",
        ]
    }

    pub fn detect(header: &[u8]) -> Option<ContainerKind> {
        // BMFF files start with ftyp box
        // Format: size(4) + 'ftyp'(4) + ...
        if header.len() >= 8 {
            let ftyp = &header[4..8];
            if ftyp == b"ftyp" {
                return Some(ContainerKind::Bmff);
            }
        }
        None
    }

    fn parse_impl<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        source.seek(SeekFrom::Start(0))?;

        // Get file size
        let file_size = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;

        // Read ftyp box to determine media type
        let mut buf = [0u8; 8];
        source.read_exact(&mut buf)?;

        if &buf[4..8] != b"ftyp" {
            return Err(Error::InvalidFormat(
                "Not a BMFF file (missing ftyp box)".into(),
            ));
        }

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if size < 12 {
            return Err(Error::InvalidFormat("Invalid ftyp box size".into()));
        }

        // Read major brand
        let mut major_brand = [0u8; 4];
        source.read_exact(&mut major_brand)?;

        let media_type = detect_media_type_from_ftyp(&major_brand);

        // Reset to start
        source.seek(SeekFrom::Start(0))?;

        // Create root node
        let root_box = BoxInfo {
            path: "".to_string(),
            offset: 0,
            size: file_size,
            box_type: BoxType::Empty,
            parent: None,
            user_type: None,
            version: None,
            flags: None,
        };

        let (mut bmff_tree, root_token) = Arena::with_data(root_box);
        let mut bmff_map: HashMap<String, Vec<Token>> = HashMap::new();

        // Build layout of the BMFF structure
        build_bmff_tree(
            source,
            file_size,
            &mut bmff_tree,
            &root_token,
            &mut bmff_map,
        )?;

        // Create structure
        let mut structure = Structure::new(ContainerKind::Bmff, media_type);
        structure.total_size = file_size;

        // Find XMP UUID boxes
        if let Some(uuid_list) = bmff_map.get("/uuid") {
            for uuid_token in uuid_list {
                let box_info = &bmff_tree[*uuid_token];
                if let Some(uuid) = &box_info.data.user_type {
                    if uuid.as_slice() == XMP_UUID {
                        // XMP UUID box found
                        // XMP boxes DON'T have version/flags, data starts right after UUID
                        // box_info.data.offset points to START of box (before size+type header)
                        // After header (8) + UUID (16) = 24 bytes, XMP data starts
                        let data_offset = box_info.data.offset + 8 + 16;
                        let data_size = box_info.data.size - 8 - 16;
                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(data_offset, data_size)],
                            SegmentKind::Xmp,
                            Some("uuid/xmp".to_string()),
                        ));
                    } else if uuid.as_slice() == C2PA_UUID {
                        // C2PA UUID box found (contains JUMBF)
                        // Structure: header(8) + uuid(16) + version/flags(4) + purpose(var+\0) + merkle_offset(8) + data
                        //
                        // box_info.data.offset points to START of box (before size+type header)
                        // After header (8) + UUID (16) + version/flags (4) = 28 bytes, we have:
                        // - purpose string (variable, null-terminated)
                        // - merkle offset (8 bytes)
                        // - JUMBF data

                        let box_offset = box_info.data.offset;
                        let box_size = box_info.data.size;

                        // Seek to after header + UUID + version/flags
                        let version_flags_offset = box_offset + 8 + 16 + 4;
                        source.seek(SeekFrom::Start(version_flags_offset))?;

                        // Read purpose string (null-terminated)
                        let mut purpose_bytes = Vec::new();
                        loop {
                            let mut byte = [0u8; 1];
                            if source.read_exact(&mut byte).is_err() {
                                break;
                            }
                            if byte[0] == 0 {
                                break; // Found null terminator
                            }
                            purpose_bytes.push(byte[0]);
                            // Sanity check: purpose string shouldn't be longer than 256 bytes
                            if purpose_bytes.len() > 256 {
                                break;
                            }
                        }

                        // Skip merkle offset (8 bytes)
                        source.seek(SeekFrom::Current(8))?;

                        // Current position is start of JUMBF data
                        let data_offset = source.stream_position()?;
                        let header_overhead = (data_offset - box_offset) as u64;
                        let data_size = box_size.saturating_sub(header_overhead);

                        // Store JUMBF data location (for reading/writing manifest)
                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(data_offset, data_size)],
                            SegmentKind::Jumbf,
                            Some(format!(
                                "uuid/c2pa/{}",
                                String::from_utf8_lossy(&purpose_bytes)
                            )),
                        ));
                    }
                }
            }
        }

        // Find EXIF items in HEIF structure (if exif feature enabled)
        #[cfg(feature = "exif")]
        {
            if let Ok(Some((meta_offset, meta_size))) = find_meta_box(source, file_size) {
                // Parse iinf to find Exif items
                if let Ok(exif_item_ids) = parse_iinf_for_exif(source, meta_offset, meta_size) {
                    if !exif_item_ids.is_empty() {
                        // Parse iloc to get locations
                        if let Ok(locations) = parse_iloc(source, meta_offset, meta_size) {
                            for exif_id in exif_item_ids {
                                if let Some((offset, size)) = locations.get(&exif_id) {
                                    // HEIF EXIF has a 4-byte header before TIFF data
                                    // We store the raw item location; exif_info() will handle the header
                                    structure.add_segment(Segment::with_ranges(
                                        vec![ByteRange::new(*offset, *size)],
                                        SegmentKind::Exif,
                                        Some("meta/Exif".to_string()),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(structure)
    }
}

impl Default for BmffIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerIO for BmffIO {
    fn container_type() -> ContainerKind {
        ContainerKind::Bmff
    }

    fn supported_media_types() -> &'static [MediaType] {
        Self::supported_media_types()
    }

    fn extensions() -> &'static [&'static str] {
        Self::extensions()
    }

    fn mime_types() -> &'static [&'static str] {
        Self::mime_types()
    }

    fn detect(header: &[u8]) -> Option<ContainerKind> {
        Self::detect(header)
    }

    fn parse<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        self.parse_impl(source)
    }

    fn read_xmp<R: Read + Seek>(
        &self,
        _structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        let Some(index) = _structure.xmp_index() else {
            return Ok(None);
        };

        let segment = &_structure.segments()[index];
        if !segment.is_xmp() {
            return Ok(None);
        }

        // Read XMP data from ranges
        if segment.ranges.len() == 1 {
            let range = segment.ranges[0];
            source.seek(SeekFrom::Start(range.offset))?;
            let mut xmp_data = vec![0u8; range.size as usize];
            source.read_exact(&mut xmp_data)?;
            return Ok(Some(xmp_data));
        }

        Ok(None)
    }

    fn read_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        if structure.jumbf_indices().is_empty() {
            return Ok(None);
        }

        // For BMFF, we need to parse the C2PA UUID box structure
        // The JUMBF data is inside the C2PA UUID box
        for &index in structure.jumbf_indices() {
            let segment = &structure.segments()[index];
            if !segment.is_jumbf() {
                continue;
            }

            if segment.ranges.len() == 1 {
                let range = segment.ranges[0];
                source.seek(SeekFrom::Start(range.offset))?;

                // Read C2PA box structure:
                // purpose (null-terminated string) + merkle_offset (8 bytes) + JUMBF data

                // Read purpose string (scan for null terminator)
                let mut purpose_bytes = Vec::new();
                loop {
                    let mut buf = [0u8; 1];
                    source.read_exact(&mut buf)?;
                    if buf[0] == 0 {
                        break;
                    }
                    purpose_bytes.push(buf[0]);
                    if purpose_bytes.len() > 64 {
                        // Safety check
                        return Err(Error::InvalidFormat("C2PA purpose string too long".into()));
                    }
                }

                // Skip merkle offset (8 bytes)
                let mut merkle_offset_buf = [0u8; 8];
                source.read_exact(&mut merkle_offset_buf)?;

                // Calculate remaining JUMBF data size
                let bytes_read = purpose_bytes.len() as u64 + 1 + 8;
                let jumbf_size = range.size.saturating_sub(bytes_read);

                if jumbf_size > 0 {
                    let mut jumbf_data = vec![0u8; jumbf_size as usize];
                    source.read_exact(&mut jumbf_data)?;
                    return Ok(Some(jumbf_data));
                }
            }
        }

        Ok(None)
    }

    fn write<R: Read + Seek, W: Write>(
        &self,
        _structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        use crate::MetadataUpdate;

        source.seek(SeekFrom::Start(0))?;

        // Get file size
        let file_size = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;

        // Parse BMFF structure to find insertion points
        let root_box = BoxInfo {
            path: "".to_string(),
            offset: 0,
            size: file_size,
            box_type: BoxType::Empty,
            parent: None,
            user_type: None,
            version: None,
            flags: None,
        };

        let (mut bmff_tree, root_token) = Arena::with_data(root_box);
        let mut bmff_map: HashMap<String, Vec<Token>> = HashMap::new();
        build_bmff_tree(
            source,
            file_size,
            &mut bmff_tree,
            &root_token,
            &mut bmff_map,
        )?;

        // Find ftyp box (required to be first)
        let ftyp_token = bmff_map
            .get("/ftyp")
            .and_then(|v| v.first())
            .ok_or_else(|| Error::InvalidFormat("Missing ftyp box".to_string()))?;
        let ftyp_info = &bmff_tree[*ftyp_token].data;
        let ftyp_end = ftyp_info.offset + ftyp_info.size;

        // Determine what to do with XMP and JUMBF
        let write_xmp = matches!(updates.xmp, MetadataUpdate::Set(_));
        let write_jumbf = matches!(updates.jumbf, MetadataUpdate::Set(_));
        let remove_xmp = matches!(updates.xmp, MetadataUpdate::Remove);
        let remove_jumbf = matches!(updates.jumbf, MetadataUpdate::Remove);

        // Find existing UUID boxes
        let existing_xmp_token = if let Some(uuid_list) = bmff_map.get("/uuid") {
            uuid_list
                .iter()
                .find(|&&token| {
                    let box_info = &bmff_tree[token];
                    box_info
                        .data
                        .user_type
                        .as_ref()
                        .map(|uuid| uuid.as_slice() == XMP_UUID)
                        .unwrap_or(false)
                })
                .copied()
        } else {
            None
        };

        let existing_c2pa_token = if let Some(uuid_list) = bmff_map.get("/uuid") {
            uuid_list
                .iter()
                .find(|&&token| {
                    let box_info = &bmff_tree[token];
                    box_info
                        .data
                        .user_type
                        .as_ref()
                        .map(|uuid| uuid.as_slice() == C2PA_UUID)
                        .unwrap_or(false)
                })
                .copied()
        } else {
            None
        };

        // Simple strategy: Copy up to ftyp end, insert/skip UUIDs, copy rest
        source.seek(SeekFrom::Start(0))?;

        // Copy ftyp box
        let mut buffer = vec![0u8; ftyp_end as usize];
        source.read_exact(&mut buffer)?;
        writer.write_all(&buffer)?;

        // Write new XMP UUID if needed
        if write_xmp {
            if let MetadataUpdate::Set(ref xmp_data) = updates.xmp {
                write_xmp_box(writer, xmp_data)?;
            }
        } else if !remove_xmp {
            // Keep existing XMP
            if let Some(token) = existing_xmp_token {
                let box_info = &bmff_tree[token].data;
                source.seek(SeekFrom::Start(box_info.offset))?;
                let mut box_data = vec![0u8; box_info.size as usize];
                source.read_exact(&mut box_data)?;
                writer.write_all(&box_data)?;
            }
        }

        // Write new C2PA UUID if needed
        if write_jumbf {
            if let MetadataUpdate::Set(ref jumbf_data) = updates.jumbf {
                write_c2pa_box(writer, jumbf_data, "manifest", &[], 0)?;
            }
        } else if !remove_jumbf {
            // Keep existing C2PA
            if let Some(token) = existing_c2pa_token {
                let box_info = &bmff_tree[token].data;
                source.seek(SeekFrom::Start(box_info.offset))?;
                let mut box_data = vec![0u8; box_info.size as usize];
                source.read_exact(&mut box_data)?;
                writer.write_all(&box_data)?;
            }
        }

        // Copy remaining boxes (skip existing UUID boxes)
        source.seek(SeekFrom::Start(ftyp_end))?;
        let mut current_pos = ftyp_end;

        while current_pos < file_size {
            // Read box header to determine if we should skip it
            let box_start = current_pos;
            let header = BoxHeaderLite::read(source)?;

            // Check if this is a UUID box we already wrote
            let should_skip = if header.name == BoxType::UuidBox {
                let mut uuid_bytes = [0u8; 16];
                source.read_exact(&mut uuid_bytes)?;
                source.seek(SeekFrom::Start(box_start))?; // Reset for potential copy

                (uuid_bytes == XMP_UUID && (write_xmp || remove_xmp))
                    || (uuid_bytes == C2PA_UUID && (write_jumbf || remove_jumbf))
            } else {
                false
            };

            if should_skip {
                // Skip this box
                source.seek(SeekFrom::Start(box_start + header.size))?;
            } else {
                // Copy this box
                source.seek(SeekFrom::Start(box_start))?;
                let mut box_data = vec![0u8; header.size as usize];
                source.read_exact(&mut box_data)?;
                writer.write_all(&box_data)?;
            }

            current_pos = box_start + header.size;
            source.seek(SeekFrom::Start(current_pos))?;
        }

        Ok(())
    }

    fn calculate_updated_structure(
        &self,
        source_structure: &Structure,
        updates: &Updates,
    ) -> Result<Structure> {
        use crate::MetadataUpdate;

        let mut new_structure =
            Structure::new(source_structure.container, source_structure.media_type);

        // Calculate sizes for new UUID boxes
        let xmp_box_size = match &updates.xmp {
            MetadataUpdate::Set(data) => Some(8 + 16 + data.len()), // header + UUID + data
            MetadataUpdate::Remove => None,
            MetadataUpdate::Keep => {
                // Find existing XMP segment size
                source_structure
                    .xmp_index()
                    .and_then(|idx| source_structure.segments().get(idx))
                    .map(|seg| {
                        let data_size: u64 = seg.ranges.iter().map(|r| r.size).sum();
                        (8 + 16 + data_size) as usize // Reconstruct full box size
                    })
            }
        };

        let jumbf_box_size = match &updates.jumbf {
            MetadataUpdate::Set(data) => {
                // C2PA box: header + UUID + version/flags + purpose + null + merkle_offset + data
                Some(8 + 16 + 4 + "manifest".len() + 1 + 8 + data.len())
            }
            MetadataUpdate::Remove => None,
            MetadataUpdate::Keep => {
                // Find existing JUMBF segment size
                source_structure
                    .jumbf_indices()
                    .first()
                    .and_then(|&idx| source_structure.segments().get(idx))
                    .map(|seg| {
                        let data_size: u64 = seg.ranges.iter().map(|r| r.size).sum();
                        (8 + 16 + 4 + "manifest".len() + 1 + 8) + data_size as usize
                    })
            }
        };

        // Start with ftyp box (assume it exists and comes first)
        // In a real file, we'd parse to find ftyp, but for structure calculation we can estimate
        let mut current_offset = 0u64;

        // Add ftyp (typically ~32 bytes, but we should get this from source)
        // For now, estimate based on common size
        let ftyp_size = 32u64;
        current_offset += ftyp_size;

        // Add XMP UUID box if present
        if let Some(size) = xmp_box_size {
            let data_offset = current_offset + 8 + 16; // Skip header + UUID
            let data_size = size - 8 - 16;
            new_structure.add_segment(Segment::with_ranges(
                vec![ByteRange::new(data_offset, data_size as u64)],
                SegmentKind::Xmp,
                Some("uuid/xmp".to_string()),
            ));
            current_offset += size as u64;
        }

        // Add C2PA UUID box if present
        if let Some(size) = jumbf_box_size {
            // Data starts after: header + UUID + version/flags + purpose + null + merkle_offset
            let data_offset = current_offset + 8 + 16 + 4 + "manifest".len() as u64 + 1 + 8;
            let data_size = size - (8 + 16 + 4 + "manifest".len() + 1 + 8);
            new_structure.add_segment(Segment::with_ranges(
                vec![ByteRange::new(data_offset, data_size as u64)],
                SegmentKind::Jumbf,
                Some("uuid/c2pa".to_string()),
            ));
            current_offset += size as u64;
        }

        // Add remaining boxes size (moov, mdat, etc.)
        // This is approximate - in reality we'd need to parse the full source structure
        current_offset += source_structure.total_size - ftyp_size;

        new_structure.total_size = current_offset;
        Ok(new_structure)
    }

    #[cfg(feature = "exif")]
    fn read_embedded_thumbnail_info<R: Read + Seek>(
        &self,
        _structure: &Structure,
        source: &mut R,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
        // HEIF/HEIC thumbnail extraction
        // Thumbnails are stored as separate items with 'thmb' reference to the primary item
        extract_heif_thumbnail_info(source)
    }

    #[cfg(feature = "exif")]
    fn read_exif_info<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<crate::tiff::ExifInfo>> {
        use std::io::SeekFrom;

        // Find EXIF segment
        let exif_segment = structure
            .segments()
            .iter()
            .find(|s| s.is_type(SegmentKind::Exif));

        let segment = match exif_segment {
            Some(s) => s,
            None => return Ok(None),
        };

        // Read the EXIF data
        let location = segment.location();
        source.seek(SeekFrom::Start(location.offset))?;
        let mut data = vec![0u8; location.size as usize];
        source.read_exact(&mut data)?;

        // HEIF: Exif item has 4-byte tiff_header_offset prefix, then "Exif\0\0", then TIFF
        // The 4 bytes are typically 0x00000006 (offset to TIFF data from start of Exif block)
        let exif_data = if data.len() > 10 && &data[4..10] == b"Exif\0\0" {
            &data[10..]
        } else if data.len() > 4 {
            // Some files may have different structure, try to find TIFF header
            if data[4..].starts_with(b"II") || data[4..].starts_with(b"MM") {
                &data[4..]
            } else {
                &data
            }
        } else {
            return Ok(None);
        };

        crate::tiff::parse_exif_info(exif_data)
    }

    fn exclusion_range_for_segment(
        structure: &Structure,
        kind: SegmentKind,
    ) -> Option<(u64, u64)> {
        let segment = match kind {
            SegmentKind::Jumbf => structure
                .c2pa_jumbf_index()
                .map(|i| &structure.segments()[i]),
            SegmentKind::Xmp => structure.xmp_index().map(|i| &structure.segments()[i]),
            _ => None,
        }?;

        // Return the data range (ranges[0])
        // Box headers are included in hash per C2PA spec
        let location = segment.location();
        Some((location.offset, location.size))
    }
}

// ============================================================================
// HEIF Thumbnail Extraction
// ============================================================================

/// Extract thumbnail info from a HEIF/HEIC file
#[cfg(feature = "exif")]
fn extract_heif_thumbnail_info<R: Read + Seek>(
    source: &mut R,
) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
    use crate::thumbnail::{EmbeddedThumbnailInfo, ThumbnailKind};

    source.seek(SeekFrom::Start(0))?;
    let file_size = source.seek(SeekFrom::End(0))?;
    source.seek(SeekFrom::Start(0))?;

    // Find and parse the meta box
    let meta_info = find_meta_box(source, file_size)?;
    let Some((meta_offset, meta_size)) = meta_info else {
        return Ok(None);
    };

    // Parse pitm (primary item ID)
    let primary_item_id = parse_pitm(source, meta_offset, meta_size)?;
    let Some(primary_id) = primary_item_id else {
        return Ok(None);
    };

    // Parse iref to find thumbnail items
    let thumbnail_item_ids = parse_iref_for_thumbnails(source, meta_offset, meta_size, primary_id)?;
    if thumbnail_item_ids.is_empty() {
        return Ok(None);
    }

    // Parse iloc to get thumbnail location
    let iloc_entries = parse_iloc(source, meta_offset, meta_size)?;

    // Find the first thumbnail's location
    for thumb_id in thumbnail_item_ids {
        if let Some((offset, size)) = iloc_entries.get(&thumb_id) {
            // Try to detect format from first bytes
            source.seek(SeekFrom::Start(*offset))?;
            let mut magic = [0u8; 4];
            if source.read_exact(&mut magic).is_ok() {
                let format = if magic[0..2] == [0xFF, 0xD8] {
                    ThumbnailKind::Jpeg
                } else {
                    ThumbnailKind::Other
                };

                return Ok(Some(EmbeddedThumbnailInfo {
                    offset: *offset,
                    size: *size,
                    format,
                    width: None,
                    height: None,
                }));
            }
        }
    }

    Ok(None)
}

/// Find the meta box offset and size
#[cfg(feature = "exif")]
fn find_meta_box<R: Read + Seek>(source: &mut R, file_size: u64) -> Result<Option<(u64, u64)>> {
    source.seek(SeekFrom::Start(0))?;

    let mut pos = 0u64;
    while pos < file_size {
        source.seek(SeekFrom::Start(pos))?;

        let mut buf = [0u8; 8];
        if source.read_exact(&mut buf).is_err() {
            break;
        }

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let box_type = &buf[4..8];

        let actual_size = if size == 1 {
            // Extended size
            let mut ext = [0u8; 8];
            source.read_exact(&mut ext)?;
            u64::from_be_bytes(ext)
        } else if size == 0 {
            file_size - pos
        } else {
            size
        };

        if box_type == b"meta" {
            return Ok(Some((pos, actual_size)));
        }

        if actual_size == 0 {
            break;
        }
        pos += actual_size;
    }

    Ok(None)
}

/// Parse pitm box to get primary item ID
#[cfg(feature = "exif")]
fn parse_pitm<R: Read + Seek>(
    source: &mut R,
    meta_offset: u64,
    meta_size: u64,
) -> Result<Option<u32>> {
    let meta_end = meta_offset + meta_size;

    // Skip meta box header (8 bytes) + version/flags (4 bytes)
    source.seek(SeekFrom::Start(meta_offset + 12))?;

    while source.stream_position()? < meta_end {
        let box_start = source.stream_position()?;

        let mut buf = [0u8; 8];
        if source.read_exact(&mut buf).is_err() {
            break;
        }

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let box_type = &buf[4..8];

        if size == 0 {
            break;
        }

        if box_type == b"pitm" {
            // pitm: version (1) + flags (3) + item_id (2 or 4 depending on version)
            let version = source.read_u8()?;
            source.read_u24::<BigEndian>()?; // flags

            let item_id = if version == 0 {
                source.read_u16::<BigEndian>()? as u32
            } else {
                source.read_u32::<BigEndian>()?
            };

            return Ok(Some(item_id));
        }

        source.seek(SeekFrom::Start(box_start + size))?;
    }

    Ok(None)
}

/// Parse iref box to find items with 'thmb' reference to the primary item
#[cfg(feature = "exif")]
fn parse_iref_for_thumbnails<R: Read + Seek>(
    source: &mut R,
    meta_offset: u64,
    meta_size: u64,
    primary_id: u32,
) -> Result<Vec<u32>> {
    let meta_end = meta_offset + meta_size;
    let mut thumbnail_ids = Vec::new();

    // Skip meta box header (8 bytes) + version/flags (4 bytes)
    source.seek(SeekFrom::Start(meta_offset + 12))?;

    while source.stream_position()? < meta_end {
        let box_start = source.stream_position()?;

        let mut buf = [0u8; 8];
        if source.read_exact(&mut buf).is_err() {
            break;
        }

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let box_type = &buf[4..8];

        if size == 0 {
            break;
        }

        if box_type == b"iref" {
            let iref_end = box_start + size;

            // version (1) + flags (3)
            let version = source.read_u8()?;
            source.read_u24::<BigEndian>()?; // flags

            // Parse reference entries
            while source.stream_position()? < iref_end {
                let entry_start = source.stream_position()?;

                let mut entry_buf = [0u8; 8];
                if source.read_exact(&mut entry_buf).is_err() {
                    break;
                }

                let entry_size = u32::from_be_bytes([entry_buf[0], entry_buf[1], entry_buf[2], entry_buf[3]]) as u64;
                let ref_type = &entry_buf[4..8];

                if entry_size == 0 {
                    break;
                }

                if ref_type == b"thmb" {
                    // This is a thumbnail reference
                    // from_item_ID (2 or 4 bytes) + reference_count (2) + to_item_IDs
                    let from_item_id = if version == 0 {
                        source.read_u16::<BigEndian>()? as u32
                    } else {
                        source.read_u32::<BigEndian>()?
                    };

                    let ref_count = source.read_u16::<BigEndian>()?;

                    for _ in 0..ref_count {
                        let to_item_id = if version == 0 {
                            source.read_u16::<BigEndian>()? as u32
                        } else {
                            source.read_u32::<BigEndian>()?
                        };

                        // If this thumbnail points to the primary item, record the thumbnail ID
                        if to_item_id == primary_id {
                            thumbnail_ids.push(from_item_id);
                        }
                    }
                }

                source.seek(SeekFrom::Start(entry_start + entry_size))?;
            }

            return Ok(thumbnail_ids);
        }

        source.seek(SeekFrom::Start(box_start + size))?;
    }

    Ok(thumbnail_ids)
}

/// Parse iinf box to find items with type "Exif"
#[cfg(feature = "exif")]
fn parse_iinf_for_exif<R: Read + Seek>(
    source: &mut R,
    meta_offset: u64,
    meta_size: u64,
) -> Result<Vec<u32>> {
    let meta_end = meta_offset + meta_size;
    let mut exif_item_ids = Vec::new();

    // Skip meta box header (8 bytes) + version/flags (4 bytes)
    source.seek(SeekFrom::Start(meta_offset + 12))?;

    while source.stream_position()? < meta_end {
        let box_start = source.stream_position()?;

        let mut buf = [0u8; 8];
        if source.read_exact(&mut buf).is_err() {
            break;
        }

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let box_type = &buf[4..8];

        if size == 0 {
            break;
        }

        if box_type == b"iinf" {
            let iinf_end = box_start + size;

            // version (1) + flags (3)
            let version = source.read_u8()?;
            source.read_u24::<BigEndian>()?; // flags

            // entry_count
            let entry_count = if version == 0 {
                source.read_u16::<BigEndian>()? as u32
            } else {
                source.read_u32::<BigEndian>()?
            };

            // Parse infe entries
            for _ in 0..entry_count {
                if source.stream_position()? >= iinf_end {
                    break;
                }

                let infe_start = source.stream_position()?;

                let mut infe_buf = [0u8; 8];
                if source.read_exact(&mut infe_buf).is_err() {
                    break;
                }

                let infe_size = u32::from_be_bytes([infe_buf[0], infe_buf[1], infe_buf[2], infe_buf[3]]) as u64;
                let infe_type = &infe_buf[4..8];

                if infe_size == 0 || infe_type != b"infe" {
                    source.seek(SeekFrom::Start(infe_start + infe_size.max(8)))?;
                    continue;
                }

                // infe: version (1) + flags (3)
                let infe_version = source.read_u8()?;
                source.read_u24::<BigEndian>()?; // flags

                if infe_version >= 2 {
                    // item_ID
                    let item_id = if infe_version == 2 {
                        source.read_u16::<BigEndian>()? as u32
                    } else {
                        source.read_u32::<BigEndian>()?
                    };

                    // item_protection_index (2 bytes)
                    source.read_u16::<BigEndian>()?;

                    // item_type (4 bytes) - this is what we're looking for!
                    let mut item_type = [0u8; 4];
                    source.read_exact(&mut item_type)?;

                    if &item_type == b"Exif" {
                        exif_item_ids.push(item_id);
                    }
                }

                source.seek(SeekFrom::Start(infe_start + infe_size))?;
            }

            return Ok(exif_item_ids);
        }

        source.seek(SeekFrom::Start(box_start + size))?;
    }

    Ok(exif_item_ids)
}

/// Parse iloc box to get item locations
/// Returns a map of item_id -> (offset, size)
#[cfg(feature = "exif")]
fn parse_iloc<R: Read + Seek>(
    source: &mut R,
    meta_offset: u64,
    meta_size: u64,
) -> Result<std::collections::HashMap<u32, (u64, u64)>> {
    let meta_end = meta_offset + meta_size;
    let mut locations = std::collections::HashMap::new();

    // Skip meta box header (8 bytes) + version/flags (4 bytes)
    source.seek(SeekFrom::Start(meta_offset + 12))?;

    while source.stream_position()? < meta_end {
        let box_start = source.stream_position()?;

        let mut buf = [0u8; 8];
        if source.read_exact(&mut buf).is_err() {
            break;
        }

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let box_type = &buf[4..8];

        if size == 0 {
            break;
        }

        if box_type == b"iloc" {
            // version (1) + flags (3)
            let version = source.read_u8()?;
            source.read_u24::<BigEndian>()?; // flags

            // offset_size (4 bits) + length_size (4 bits) + base_offset_size (4 bits) + index_size/reserved (4 bits)
            let sizes1 = source.read_u8()?;
            let sizes2 = source.read_u8()?;

            let offset_size = (sizes1 >> 4) & 0x0F;
            let length_size = sizes1 & 0x0F;
            let base_offset_size = (sizes2 >> 4) & 0x0F;
            let _index_size = if version >= 1 { sizes2 & 0x0F } else { 0 };

            // item_count
            let item_count = if version < 2 {
                source.read_u16::<BigEndian>()? as u32
            } else {
                source.read_u32::<BigEndian>()?
            };

            for _ in 0..item_count {
                // item_ID
                let item_id = if version < 2 {
                    source.read_u16::<BigEndian>()? as u32
                } else {
                    source.read_u32::<BigEndian>()?
                };

                // construction_method (version >= 1)
                if version >= 1 {
                    source.read_u16::<BigEndian>()?; // construction_method + reserved
                }

                // data_reference_index
                source.read_u16::<BigEndian>()?;

                // base_offset
                let base_offset = read_variable_int(source, base_offset_size)?;

                // extent_count
                let extent_count = source.read_u16::<BigEndian>()?;

                let mut total_size = 0u64;
                let mut first_offset = 0u64;

                for i in 0..extent_count {
                    // extent_index (version >= 1 and index_size > 0) - skip for simplicity
                    if version >= 1 && _index_size > 0 {
                        read_variable_int(source, _index_size)?;
                    }

                    // extent_offset
                    let extent_offset = read_variable_int(source, offset_size)?;

                    // extent_length
                    let extent_length = read_variable_int(source, length_size)?;

                    if i == 0 {
                        first_offset = base_offset + extent_offset;
                    }
                    total_size += extent_length;
                }

                if extent_count > 0 {
                    locations.insert(item_id, (first_offset, total_size));
                }
            }

            return Ok(locations);
        }

        source.seek(SeekFrom::Start(box_start + size))?;
    }

    Ok(locations)
}

/// Read a variable-length integer based on size specifier
#[cfg(feature = "exif")]
fn read_variable_int<R: Read>(source: &mut R, size: u8) -> Result<u64> {
    match size {
        0 => Ok(0),
        1 => Ok(source.read_u8()? as u64),
        2 => Ok(source.read_u16::<BigEndian>()? as u64),
        4 => Ok(source.read_u32::<BigEndian>()? as u64),
        8 => Ok(source.read_u64::<BigEndian>()?),
        _ => Err(Error::InvalidFormat(format!(
            "Invalid iloc size specifier: {}",
            size
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bmff_detect() {
        // Minimal BMFF ftyp box
        let data = vec![
            0x00, 0x00, 0x00, 0x18, // size = 24
            b'f', b't', b'y', b'p', // type = ftyp
            b'h', b'e', b'i', b'c', // major brand = heic
            0x00, 0x00, 0x00, 0x00, // minor version
            b'h', b'e', b'i', b'c', // compatible brand
            b'm', b'i', b'f', b'1', // compatible brand
        ];

        assert_eq!(BmffIO::detect(&data), Some(ContainerKind::Bmff));
    }
}
