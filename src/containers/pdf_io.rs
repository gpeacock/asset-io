//! Bounded, read-only PDF metadata access. Payloads are never scanned or decoded.

use super::{ContainerIO, ContainerKind};
use crate::{ByteRange, Error, MediaType, Result, Segment, SegmentKind, Structure, Updates};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};

const MAX_ENTRIES: usize = 100_000;
const MAX_REVISIONS: usize = 128;
const MAX_OBJECTS: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 100_000;
const MAX_TOKEN: usize = 4096;
const MAX_SYNTAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const READ_ONLY: &str = "PDF is read-only";

fn invalid(message: &str) -> Error {
    Error::InvalidFormat(format!("PDF: {message}"))
}

type Dictionary = BTreeMap<Vec<u8>, Value>;
type Reference = (u32, u16);

#[derive(Debug)]
enum Value {
    Integer(u64),
    Reference(Reference),
    Name(Vec<u8>),
    Dictionary(Dictionary),
    Array(Vec<Value>),
    Null,
    Other,
}

impl Value {
    fn dictionary(self) -> Result<Dictionary> {
        match self {
            Self::Dictionary(dict) => Ok(dict),
            _ => Err(invalid("expected dictionary")),
        }
    }

    fn integer(self) -> Result<u64> {
        match self {
            Self::Integer(value) => Ok(value),
            _ => Err(invalid("expected nonnegative integer")),
        }
    }

    fn is_name(&self, name: &[u8]) -> bool {
        matches!(self, Self::Name(value) if value == name)
    }
}

fn required(dict: &mut Dictionary, key: &[u8]) -> Result<Value> {
    dict.remove(key)
        .ok_or_else(|| invalid("missing required dictionary key"))
}

fn whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

fn delimiter(byte: u8) -> bool {
    whitespace(byte) || b"()<>[]{}/%".contains(&byte)
}

fn unsigned(bytes: &[u8]) -> Result<u64> {
    let bytes = bytes.strip_prefix(b"+").unwrap_or(bytes);
    if bytes.is_empty() {
        return Err(invalid("empty integer"));
    }
    bytes.iter().try_fold(0u64, |value, &byte| {
        if !byte.is_ascii_digit() {
            return Err(invalid("invalid integer"));
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| invalid("integer overflow"))
    })
}

fn hex(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(invalid("invalid hexadecimal digit")),
    }
}

struct Object {
    value: Value,
    stream: Option<u64>,
}

struct Entry {
    offset: u64,
    generation: u16,
    in_use: bool,
}

struct Parser<R> {
    input: BufReader<R>,
    size: u64,
    budget: u64,
    nodes: usize,
    objects: usize,
    entries: BTreeMap<u32, Entry>,
}

impl<R: Read + Seek> Parser<R> {
    fn position(&mut self) -> Result<u64> {
        Ok(self.input.stream_position()?)
    }

    fn seek(&mut self, offset: u64) -> Result<()> {
        if offset >= self.size {
            return Err(invalid("offset outside file"));
        }
        // Retain the buffer for local lexer lookahead.
        let current = self.position()?;
        if let Ok(delta) = i64::try_from(i128::from(offset) - i128::from(current)) {
            self.input.seek_relative(delta)?;
        } else {
            self.input.seek(SeekFrom::Start(offset))?;
        }
        Ok(())
    }

    fn peek(&mut self) -> Result<Option<u8>> {
        Ok(self.input.fill_buf()?.first().copied())
    }

    fn byte(&mut self) -> Result<u8> {
        self.budget = self
            .budget
            .checked_sub(1)
            .ok_or_else(|| invalid("syntax work limit exceeded"))?;
        let byte = self
            .peek()?
            .ok_or_else(|| invalid("unexpected end of file"))?;
        self.input.consume(1);
        Ok(byte)
    }

    fn skip_space(&mut self) -> Result<()> {
        loop {
            match self.peek()? {
                Some(byte) if whitespace(byte) => {
                    self.byte()?;
                }
                Some(b'%') => {
                    while let Some(byte) = self.peek()? {
                        self.byte()?;
                        if matches!(byte, b'\r' | b'\n') {
                            break;
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn token(&mut self) -> Result<Vec<u8>> {
        self.skip_space()?;
        let mut token = Vec::new();
        while let Some(byte) = self.peek()? {
            if delimiter(byte) {
                break;
            }
            if token.len() == MAX_TOKEN {
                return Err(invalid("token limit exceeded"));
            }
            token.push(self.byte()?);
        }
        if token.is_empty() {
            return Err(invalid("expected token"));
        }
        Ok(token)
    }

    fn expect(&mut self, expected: &[u8]) -> Result<()> {
        if self.token()? != expected {
            return Err(invalid("unexpected keyword"));
        }
        Ok(())
    }

    fn number(&mut self) -> Result<u64> {
        unsigned(&self.token()?)
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth >= MAX_DEPTH || self.nodes >= MAX_NODES {
            return Err(invalid("object complexity limit exceeded"));
        }
        self.nodes += 1;
        self.skip_space()?;
        match self.peek()?.ok_or_else(|| invalid("missing value"))? {
            b'/' => {
                self.byte()?;
                let mut name = Vec::new();
                while let Some(byte) = self.peek()? {
                    if delimiter(byte) {
                        break;
                    }
                    if name.len() == MAX_TOKEN {
                        return Err(invalid("name limit exceeded"));
                    }
                    let byte = self.byte()?;
                    name.push(if byte == b'#' {
                        (hex(self.byte()?)? << 4) | hex(self.byte()?)?
                    } else {
                        byte
                    });
                }
                Ok(Value::Name(name))
            }
            b'[' => {
                self.byte()?;
                let mut values = Vec::new();
                loop {
                    self.skip_space()?;
                    if self.peek()? == Some(b']') {
                        self.byte()?;
                        break;
                    }
                    values.push(self.value(depth + 1)?);
                }
                Ok(Value::Array(values))
            }
            b'<' => {
                self.byte()?;
                if self.peek()? != Some(b'<') {
                    loop {
                        let byte = self.byte()?;
                        if byte == b'>' {
                            break;
                        }
                        if !whitespace(byte) {
                            hex(byte)?;
                        }
                    }
                    return Ok(Value::Other);
                }
                self.byte()?;
                let mut dict = Dictionary::new();
                loop {
                    self.skip_space()?;
                    if self.peek()? == Some(b'>') {
                        self.byte()?;
                        if self.byte()? != b'>' {
                            return Err(invalid("unterminated dictionary"));
                        }
                        break;
                    }
                    let Value::Name(key) = self.value(depth + 1)? else {
                        return Err(invalid("dictionary key is not a name"));
                    };
                    let value = self.value(depth + 1)?;
                    if dict.insert(key, value).is_some() {
                        return Err(invalid("duplicate dictionary key"));
                    }
                }
                Ok(Value::Dictionary(dict))
            }
            b'(' => {
                self.byte()?;
                let mut nesting = 1;
                while nesting != 0 {
                    match self.byte()? {
                        b'\\' => {
                            self.byte()?;
                        }
                        b'(' => {
                            nesting += 1;
                            if nesting > MAX_DEPTH {
                                return Err(invalid("string nesting limit exceeded"));
                            }
                        }
                        b')' => nesting -= 1,
                        _ => {}
                    }
                }
                Ok(Value::Other)
            }
            _ => {
                let token = self.token()?;
                if token == b"null" {
                    return Ok(Value::Null);
                }
                if token == b"true" || token == b"false" {
                    return Ok(Value::Other);
                }
                let digits = token.strip_prefix(b"+").unwrap_or(&token);
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                    let number = unsigned(&token)?;
                    let end = self.position()?;
                    self.skip_space()?;
                    if matches!(self.peek()?, Some(b'0'..=b'9')) {
                        let generation = self.token()?;
                        self.skip_space()?;
                        if generation.iter().all(u8::is_ascii_digit) && self.peek()? == Some(b'R') {
                            self.expect(b"R")?;
                            return Ok(Value::Reference((
                                u32::try_from(number)
                                    .map_err(|_| invalid("object number overflow"))?,
                                u16::try_from(unsigned(&generation)?)
                                    .map_err(|_| invalid("generation overflow"))?,
                            )));
                        }
                    }
                    self.seek(end)?;
                    return Ok(Value::Integer(number));
                }
                let digits = token
                    .strip_prefix(b"-")
                    .or_else(|| token.strip_prefix(b"+"))
                    .unwrap_or(&token);
                if digits.iter().any(u8::is_ascii_digit)
                    && digits.iter().all(|b| b.is_ascii_digit() || *b == b'.')
                    && digits.iter().filter(|&&b| b == b'.').count() <= 1
                {
                    return Ok(Value::Other);
                }
                Err(invalid("invalid value"))
            }
        }
    }

    fn xrefs(&mut self, mut offset: u64) -> Result<Reference> {
        let mut root = None;
        let mut entry_count = 0usize;
        for _ in 0..MAX_REVISIONS {
            self.seek(offset)?;
            if self.token()? != b"xref" {
                return Err(Error::UnsupportedFeature(
                    "PDF cross-reference/object streams",
                ));
            }
            let mut seen = BTreeSet::new();
            loop {
                self.skip_space()?;
                if self.peek()? == Some(b't') {
                    break;
                }
                let first = self.number()?;
                let count = self.number()?;
                let count = usize::try_from(count).map_err(|_| invalid("xref count overflow"))?;
                entry_count = entry_count
                    .checked_add(count)
                    .filter(|n| *n <= MAX_ENTRIES)
                    .ok_or_else(|| invalid("cross-reference entry limit exceeded"))?;
                let end = first
                    .checked_add(count as u64)
                    .filter(|end| *end <= u64::from(u32::MAX))
                    .ok_or_else(|| invalid("xref object number overflow"))?;
                for id in first..end {
                    let object_offset = self.number()?;
                    let generation = u16::try_from(self.number()?)
                        .map_err(|_| invalid("generation overflow"))?;
                    let state = self.token()?;
                    if state != b"n" && state != b"f" {
                        return Err(invalid("invalid xref entry"));
                    }
                    if state == b"n" && object_offset >= self.size {
                        return Err(invalid("object outside file"));
                    }
                    let id = id as u32;
                    if !seen.insert(id) {
                        return Err(invalid("overlapping xref subsections"));
                    }
                    // Newer free entries must mask older live objects.
                    self.entries.entry(id).or_insert(Entry {
                        offset: object_offset,
                        generation,
                        in_use: state == b"n",
                    });
                }
            }
            self.expect(b"trailer")?;
            let mut trailer = self.value(0)?.dictionary()?;
            let size = required(&mut trailer, b"Size")?.integer()?;
            if size > u64::from(u32::MAX) || seen.last().is_some_and(|id| u64::from(*id) >= size) {
                return Err(invalid("invalid trailer Size"));
            }
            if trailer
                .get(b"Encrypt".as_slice())
                .is_some_and(|v| !matches!(v, Value::Null))
            {
                return Err(Error::UnsupportedFeature("encrypted PDF"));
            }
            if trailer.contains_key(b"XRefStm".as_slice()) {
                return Err(Error::UnsupportedFeature(
                    "PDF hybrid cross-reference streams",
                ));
            }
            if let Some(value) = trailer.remove(b"Root".as_slice()) {
                let Value::Reference(reference) = value else {
                    return Err(invalid("Root must be indirect"));
                };
                if root.is_none() {
                    root = Some(reference);
                }
            }
            match trailer.remove(b"Prev".as_slice()) {
                None => return root.ok_or_else(|| invalid("missing document Root")),
                Some(value) => {
                    let previous = value.integer()?;
                    if previous >= offset {
                        return Err(invalid("cyclic or forward Prev chain"));
                    }
                    offset = previous;
                }
            }
        }
        Err(invalid("revision limit exceeded"))
    }

    fn object(&mut self, reference: Reference) -> Result<Object> {
        self.objects += 1;
        if self.objects > MAX_OBJECTS {
            return Err(invalid("object resolution limit exceeded"));
        }
        let entry = self
            .entries
            .get(&reference.0)
            .ok_or_else(|| invalid("unresolved object"))?;
        if !entry.in_use || entry.generation != reference.1 {
            return Err(invalid("stale or free object reference"));
        }
        let offset = entry.offset;
        self.seek(offset)?;
        if self.number()? != u64::from(reference.0) || self.number()? != u64::from(reference.1) {
            return Err(invalid("xref/object identity mismatch"));
        }
        self.expect(b"obj")?;
        let value = self.value(0)?;
        let ending = self.token()?;
        let stream = if ending == b"stream" {
            if !matches!(value, Value::Dictionary(_)) {
                return Err(invalid("stream without dictionary"));
            }
            match self.byte()? {
                b'\n' => {}
                b'\r' => {
                    if self.peek()? == Some(b'\n') {
                        self.byte()?;
                    }
                }
                _ => return Err(invalid("stream keyword requires newline")),
            }
            Some(self.position()?)
        } else if ending == b"endobj" {
            None
        } else {
            return Err(invalid("missing endobj"));
        };
        Ok(Object { value, stream })
    }

    fn resolve(&mut self, mut value: Value) -> Result<Value> {
        let mut seen = BTreeSet::new();
        loop {
            let Value::Reference(reference) = value else {
                return Ok(value);
            };
            if seen.len() == MAX_DEPTH {
                return Err(invalid("indirect reference depth limit exceeded"));
            }
            if !seen.insert(reference) {
                return Err(invalid("cyclic indirect reference"));
            }
            let object = self.object(reference)?;
            if object.stream.is_some() {
                return Err(invalid("unexpected stream reference"));
            }
            value = object.value;
        }
    }

    fn stream(&mut self, value: Value, kind: SegmentKind) -> Result<ByteRange> {
        let Value::Reference(reference) = value else {
            return Err(invalid("stream must be indirect"));
        };
        let object = self.object(reference)?;
        let offset = object.stream.ok_or_else(|| invalid("expected stream"))?;
        let mut dict = object.value.dictionary()?;
        let expected_type: &[u8] = if kind == SegmentKind::Xmp {
            b"Metadata"
        } else {
            b"EmbeddedFile"
        };
        if !required(&mut dict, b"Type")?.is_name(expected_type) {
            return Err(invalid("unexpected metadata stream type"));
        }
        if kind == SegmentKind::Xmp && !required(&mut dict, b"Subtype")?.is_name(b"XML") {
            return Err(invalid("XMP stream must have XML subtype"));
        }
        if dict.contains_key(b"F".as_slice()) {
            return Err(Error::UnsupportedFeature("PDF external streams"));
        }
        if dict
            .get(b"Filter".as_slice())
            .is_some_and(|v| !matches!(v, Value::Null))
            || dict
                .get(b"DecodeParms".as_slice())
                .is_some_and(|v| !matches!(v, Value::Null))
        {
            return Err(Error::UnsupportedFeature(
                "PDF compressed or filtered metadata",
            ));
        }
        let length_value = required(&mut dict, b"Length")?;
        let length = self.resolve(length_value)?.integer()?;
        let end = offset
            .checked_add(length)
            .filter(|end| *end < self.size)
            .ok_or_else(|| invalid("stream length outside file"))?;
        if length > crate::segment::MAX_SEGMENT_SIZE {
            return Err(invalid("metadata stream exceeds 256 MiB limit"));
        }
        self.seek(end)?;
        self.expect(b"endstream")?;
        self.expect(b"endobj")?;
        Ok(ByteRange::new(offset, length))
    }

    fn metadata(&mut self, root: Reference) -> Result<Vec<(ByteRange, SegmentKind)>> {
        let catalog_value = self.resolve(Value::Reference(root))?;
        let mut catalog = catalog_value.dictionary()?;
        if !required(&mut catalog, b"Type")?.is_name(b"Catalog") {
            return Err(invalid("Root is not a Catalog"));
        }
        let mut ranges = Vec::new();
        if let Some(value) = catalog.remove(b"Metadata".as_slice()) {
            if !matches!(value, Value::Null) {
                ranges.push((self.stream(value, SegmentKind::Xmp)?, SegmentKind::Xmp));
            }
        }
        if let Some(value) = catalog.remove(b"AF".as_slice()) {
            let values = match self.resolve(value)? {
                Value::Array(values) => values,
                Value::Null => Vec::new(),
                value @ Value::Dictionary(_) => vec![value],
                _ => return Err(invalid("invalid Catalog AF")),
            };
            for value in values {
                let mut spec = self.resolve(value)?.dictionary()?;
                let Some(relationship) = spec.remove(b"AFRelationship".as_slice()) else {
                    continue;
                };
                let relationship = self.resolve(relationship)?;
                if !matches!(relationship, Value::Name(_)) {
                    return Err(invalid("AFRelationship must be a name"));
                }
                if !relationship.is_name(b"C2PA_Manifest") {
                    continue;
                }
                if ranges.iter().any(|(_, kind)| *kind == SegmentKind::Jumbf) {
                    return Err(invalid("multiple document-level C2PA manifests"));
                }
                if !required(&mut spec, b"Subtype")?.is_name(b"application/c2pa") {
                    return Err(invalid("C2PA file specification has wrong subtype"));
                }
                let ef_value = required(&mut spec, b"EF")?;
                let mut ef = self.resolve(ef_value)?.dictionary()?;
                let stream = match (ef.remove(b"F".as_slice()), ef.remove(b"UF".as_slice())) {
                    (Some(Value::Reference(f)), Some(Value::Reference(uf))) if f == uf => {
                        Value::Reference(f)
                    }
                    (Some(_), Some(_)) => {
                        return Err(invalid("conflicting embedded file references"))
                    }
                    (Some(value), None) | (None, Some(value)) => value,
                    (None, None) => return Err(invalid("missing C2PA embedded file")),
                };
                ranges.push((self.stream(stream, SegmentKind::Jumbf)?, SegmentKind::Jumbf));
            }
        }
        Ok(ranges)
    }
}

pub(crate) struct PdfIO;

impl PdfIO {
    pub fn new() -> Self {
        Self
    }
    pub fn container_type() -> ContainerKind {
        ContainerKind::Pdf
    }
    pub fn supported_media_types() -> &'static [MediaType] {
        &[MediaType::Pdf]
    }
    pub fn extensions() -> &'static [&'static str] {
        &["pdf"]
    }
    pub fn mime_types() -> &'static [&'static str] {
        &["application/pdf"]
    }
    pub fn detect(header: &[u8]) -> Option<ContainerKind> {
        (header.len() >= 9
            && &header[..5] == b"%PDF-"
            && ((header[5] == b'1' && header[6] == b'.' && matches!(header[7], b'0'..=b'7'))
                || &header[5..8] == b"2.0")
            && matches!(header[8], b'\r' | b'\n'))
        .then_some(ContainerKind::Pdf)
    }
}

impl ContainerIO for PdfIO {
    fn container_type() -> ContainerKind {
        Self::container_type()
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
        let size = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        let mut header = [0; 9];
        source.read_exact(&mut header)?;
        if Self::detect(&header).is_none() {
            return Err(invalid("invalid header"));
        }
        let tail_size = size.min(1024);
        source.seek(SeekFrom::Start(size - tail_size))?;
        let mut tail = vec![0; tail_size as usize];
        source.read_exact(&mut tail)?;
        let tail = tail.trim_ascii_end();
        let tail = tail
            .strip_suffix(b"%%EOF")
            .ok_or_else(|| invalid("missing final EOF marker"))?;
        let start = tail
            .windows(9)
            .rposition(|window| window == b"startxref")
            .ok_or_else(|| invalid("missing startxref in final 1024 bytes"))?;
        let number = &tail[start + 9..];
        if !number.first().is_some_and(|b| whitespace(*b)) {
            return Err(invalid("invalid startxref"));
        }
        let offset = unsigned(number.trim_ascii())?;
        let mut parser = Parser {
            input: BufReader::new(source),
            size,
            budget: MAX_SYNTAX_BYTES,
            nodes: 0,
            objects: 0,
            entries: BTreeMap::new(),
        };
        let root = parser.xrefs(offset)?;
        let mut metadata = parser.metadata(root)?;
        metadata.sort_by_key(|(range, _)| range.offset);
        let mut structure = Structure::new(ContainerKind::Pdf, MediaType::Pdf);
        structure.total_size = size;
        structure.add_segment(Segment::new(0, 9, SegmentKind::Header, Some("PDF".into())));
        let mut end = 9;
        for (range, kind) in metadata {
            if range.offset < end {
                return Err(invalid("overlapping metadata streams"));
            }
            if range.offset > end {
                structure.add_segment(Segment::new(
                    end,
                    range.offset - end,
                    SegmentKind::Other,
                    None,
                ));
            }
            structure.add_segment(Segment::new(
                range.offset,
                range.size,
                kind,
                Some(format!("PDF/{}", kind.as_str())),
            ));
            end = range.end_offset();
        }
        if end < size {
            structure.add_segment(Segment::new(end, size - end, SegmentKind::Other, None));
        }
        Ok(structure)
    }

    fn write<R: Read + Seek, W: Read + Write + Seek>(
        &self,
        _: &Structure,
        _: &mut R,
        _: &mut W,
        _: &Updates,
    ) -> Result<()> {
        Err(Error::UnsupportedFeature(READ_ONLY))
    }

    fn calculate_updated_structure(&self, _: &Structure, _: &Updates) -> Result<Structure> {
        Err(Error::UnsupportedFeature(READ_ONLY))
    }

    fn read_xmp<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        structure
            .xmp_index()
            .map(|index| structure.read_range(source, structure.segments[index].location()))
            .transpose()
    }

    fn read_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        structure
            .c2pa_jumbf_index()
            .map(|index| structure.read_range(source, structure.segments[index].location()))
            .transpose()
    }

    #[cfg(feature = "exif")]
    fn read_embedded_thumbnail_info<R: Read + Seek>(
        &self,
        _: &Structure,
        _: &mut R,
    ) -> Result<Option<crate::thumbnail::EmbeddedThumbnailInfo>> {
        Ok(None)
    }

    #[cfg(feature = "exif")]
    fn read_exif_info<R: Read + Seek>(
        &self,
        _: &Structure,
        _: &mut R,
    ) -> Result<Option<crate::tiff::ExifInfo>> {
        Ok(None)
    }

    fn exclusion_range_for_segment(structure: &Structure, kind: SegmentKind) -> Option<(u64, u64)> {
        structure
            .segments
            .iter()
            .find(|segment| segment.is_type(kind))
            .map(|segment| {
                let range = segment.location();
                (range.offset, range.size)
            })
    }
}
