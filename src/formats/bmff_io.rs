//! BMFF (ISO Base Media File Format) container I/O implementation
//!
//! Supports multiple media types: HEIC, HEIF, AVIF, MP4, M4A, MOV
//!
//! Reference: ISO/IEC 14496-12:2022

use crate::{
    error::{Error, Result},
    segment::{ByteRange, Segment, SegmentKind},
    structure::Structure,
    Container, ContainerIO, MediaType, Updates,
};
use atree::{Arena, Token};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::{
    collections::HashMap,
    io::{copy, Read, Seek, SeekFrom, Write},
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

    pub fn container_type() -> Container {
        Container::Bmff
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

    pub fn detect(header: &[u8]) -> Option<Container> {
        // BMFF files start with ftyp box
        // Format: size(4) + 'ftyp'(4) + ...
        if header.len() >= 8 {
            let ftyp = &header[4..8];
            if ftyp == b"ftyp" {
                return Some(Container::Bmff);
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
            return Err(Error::InvalidFormat("Not a BMFF file (missing ftyp box)".into()));
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
        build_bmff_tree(source, file_size, &mut bmff_tree, &root_token, &mut bmff_map)?;

        // Create structure
        let mut structure = Structure::new(Container::Bmff, media_type);
        structure.total_size = file_size;

        // Find XMP UUID boxes
        if let Some(uuid_list) = bmff_map.get("/uuid") {
            for uuid_token in uuid_list {
                let box_info = &bmff_tree[*uuid_token];
                if let Some(uuid) = &box_info.data.user_type {
                    if uuid.as_slice() == &XMP_UUID {
                        // XMP UUID box found
                        // Skip UUID (16) + version/flags (4) to get to data
                        let data_offset = box_info.data.offset + HEADER_SIZE + 16 + 4;
                        let data_size = box_info.data.size - HEADER_SIZE - 16 - 4;
                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(data_offset, data_size)],
                            SegmentKind::Xmp,
                            Some("uuid/xmp".to_string()),
                        ));
                    } else if uuid.as_slice() == &C2PA_UUID {
                        // C2PA UUID box found (contains JUMBF)
                        // Skip UUID (16) + version/flags (4) + purpose + null (varies) to get to data
                        let data_offset = box_info.data.offset + HEADER_SIZE + 16 + 4;
                        let data_size = box_info.data.size - HEADER_SIZE - 16 - 4;
                        structure.add_segment(Segment::with_ranges(
                            vec![ByteRange::new(data_offset, data_size)],
                            SegmentKind::Jumbf,
                            Some("uuid/c2pa".to_string()),
                        ));
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
    fn container_type() -> Container {
        Container::Bmff
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

    fn detect(header: &[u8]) -> Option<Container> {
        Self::detect(header)
    }

    fn parse<R: Read + Seek>(&self, source: &mut R) -> Result<Structure> {
        self.parse_impl(source)
    }

    fn extract_xmp<R: Read + Seek>(
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

    fn extract_jumbf<R: Read + Seek>(
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
        structure: &Structure,
        source: &mut R,
        writer: &mut W,
        updates: &Updates,
    ) -> Result<()> {
        // For now, implement a simple copy
        // TODO: Implement proper C2PA UUID box insertion/update
        source.seek(SeekFrom::Start(0))?;
        copy(source, writer)?;
        
        // Warn if updates are provided but not implemented
        match (&updates.xmp, &updates.jumbf) {
            (crate::MetadataUpdate::Keep, crate::MetadataUpdate::Keep) => {
                // Just copying - this is fine
            }
            _ => {
                // TODO: Implement proper BMFF writing with metadata updates
                eprintln!("Warning: BMFF write with metadata updates not yet fully implemented");
            }
        }

        Ok(())
    }

    fn calculate_updated_structure(
        &self,
        source_structure: &Structure,
        _updates: &Updates,
    ) -> Result<Structure> {
        // For now, return a copy of source structure
        // TODO: Implement proper calculation of updated structure
        let mut new_structure = Structure::new(source_structure.container, source_structure.media_type);
        new_structure.total_size = source_structure.total_size;
        
        // Copy segments
        for segment in &source_structure.segments {
            new_structure.add_segment(segment.clone());
        }
        
        Ok(new_structure)
    }

    #[cfg(feature = "exif")]
    fn extract_embedded_thumbnail<R: Read + Seek>(
        &self,
        _structure: &Structure,
        _source: &mut R,
    ) -> Result<Option<crate::EmbeddedThumbnail>> {
        // TODO: Implement BMFF thumbnail extraction
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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

        assert_eq!(BmffIO::detect(&data), Some(Container::Bmff));
    }
}

