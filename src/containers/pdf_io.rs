//! Bounded, read-only PDF metadata access with lazy payload decoding.

use super::{ContainerIO, ContainerKind};
use crate::{ByteRange, Error, MediaType, Result, Segment, SegmentKind, Structure, Updates};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write};

const MAX_ENTRIES: usize = 1_000_000;
const MAX_REVISIONS: usize = 128;
const MAX_OBJECTS: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 100_000;
const MAX_TOKEN: usize = 4096;
const MAX_SYNTAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_STRUCTURAL_STREAM: usize = 8 * 1024 * 1024;
const MAX_DECODE_WORK: usize = 32 * 1024 * 1024;
const MAX_XREF_STORAGE: usize = 16 * 1024 * 1024;
const MAX_DECODED_METADATA: usize = 64 * 1024 * 1024;
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

#[derive(Clone, Copy)]
enum Entry {
    Free,
    Normal { offset: u64, generation: u16 },
    Compressed { container: u32, index: u32 },
}

enum Xrefs {
    Classic(BTreeMap<u32, Entry>),
    Stream {
        ranges: Vec<(u32, u32)>,
        widths: [usize; 3],
        data: Vec<u8>,
    },
}

impl Xrefs {
    fn entry(&self, id: u32) -> Result<Option<Entry>> {
        match self {
            Self::Classic(entries) => Ok(entries.get(&id).copied()),
            Self::Stream {
                ranges,
                widths,
                data,
            } => {
                let mut index = 0usize;
                for &(start, count) in ranges {
                    if id >= start && id - start < count {
                        index += (id - start) as usize;
                        let row = &data[index * widths.iter().sum::<usize>()..];
                        let mut position = 0;
                        let mut fields = [0u64; 3];
                        for (i, &width) in widths.iter().enumerate() {
                            fields[i] = if i == 0 && width == 0 { 1 } else { 0 };
                            for &byte in &row[position..position + width] {
                                fields[i] = (fields[i] << 8) | u64::from(byte);
                            }
                            position += width;
                        }
                        return Ok(Some(match fields[0] {
                            0 => Entry::Free,
                            1 => Entry::Normal {
                                offset: fields[1],
                                generation: u16::try_from(fields[2])
                                    .map_err(|_| invalid("generation overflow"))?,
                            },
                            2 => Entry::Compressed {
                                container: u32::try_from(fields[1])
                                    .map_err(|_| invalid("object stream number overflow"))?,
                                index: u32::try_from(fields[2])
                                    .map_err(|_| invalid("object stream index overflow"))?,
                            },
                            _ => return Err(invalid("unknown cross-reference entry type")),
                        }));
                    }
                    index += count as usize;
                }
                Ok(None)
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Decode {
    predictor: u8,
    columns: u32,
}

struct ObjectStream {
    id: u32,
    data: Vec<u8>,
    objects: Vec<(u32, usize)>,
    first: usize,
}

impl Decode {
    fn from_dictionary(dict: &mut Dictionary) -> Result<Option<Self>> {
        if dict.contains_key(b"F".as_slice()) {
            return Err(Error::UnsupportedFeature("PDF external streams"));
        }
        let filter = match dict.remove(b"Filter".as_slice()) {
            Some(Value::Array(mut filters)) if filters.len() == 1 => filters.remove(0),
            Some(value) => value,
            None => Value::Null,
        };
        let parameters = match dict.remove(b"DecodeParms".as_slice()) {
            Some(Value::Array(mut parameters)) if parameters.len() == 1 => parameters.remove(0),
            Some(value) => value,
            None => Value::Null,
        };
        if matches!(filter, Value::Null) {
            if !matches!(parameters, Value::Null) {
                return Err(invalid("DecodeParms without a filter"));
            }
            return Ok(None);
        }
        if !filter.is_name(b"FlateDecode") && !filter.is_name(b"Fl") {
            return Err(Error::UnsupportedFeature(
                "PDF stream filter (only FlateDecode is supported)",
            ));
        }
        let mut parameters = match parameters {
            Value::Null => Dictionary::new(),
            value => value.dictionary()?,
        };
        let predictor = parameters
            .remove(b"Predictor".as_slice())
            .map(Value::integer)
            .transpose()?
            .unwrap_or(1);
        let columns = parameters
            .remove(b"Columns".as_slice())
            .map(Value::integer)
            .transpose()?
            .unwrap_or(1);
        let colors = parameters
            .remove(b"Colors".as_slice())
            .map(Value::integer)
            .transpose()?
            .unwrap_or(1);
        let bits = parameters
            .remove(b"BitsPerComponent".as_slice())
            .map(Value::integer)
            .transpose()?
            .unwrap_or(8);
        if predictor != 1 && (!(10..=15).contains(&predictor) || colors != 1 || bits != 8) {
            return Err(Error::UnsupportedFeature(
                "PDF predictor (requires 8-bit single-component PNG rows)",
            ));
        }
        if columns == 0 || columns > 65536 {
            return Err(invalid("predictor row size limit exceeded"));
        }
        Ok(Some(Self {
            predictor: predictor as u8,
            columns: columns as u32,
        }))
    }

    fn undo_predictor(self, mut data: Vec<u8>) -> Result<Vec<u8>> {
        if self.predictor == 1 {
            return Ok(data);
        }
        let columns = self.columns as usize;
        let stride = columns + 1;
        if !data.chunks_exact(stride).remainder().is_empty() {
            return Err(invalid("incomplete PNG predictor row"));
        }
        let rows = data.len() / stride;
        for row in 0..rows {
            let source = row * stride;
            let target = row * columns;
            let filter = data[source];
            if filter > 4 {
                return Err(invalid("invalid PNG predictor filter"));
            }
            data.copy_within(source + 1..source + stride, target);
            for column in 0..columns {
                let left = if column == 0 {
                    0
                } else {
                    data[target + column - 1]
                };
                let above = if row == 0 {
                    0
                } else {
                    data[target - columns + column]
                };
                let upper_left = if row == 0 || column == 0 {
                    0
                } else {
                    data[target - columns + column - 1]
                };
                let prediction = match filter {
                    0 => 0,
                    1 => left,
                    2 => above,
                    3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
                    4 => {
                        let p = i32::from(left) + i32::from(above) - i32::from(upper_left);
                        let distances = [
                            (p - i32::from(left)).abs(),
                            (p - i32::from(above)).abs(),
                            (p - i32::from(upper_left)).abs(),
                        ];
                        if distances[0] <= distances[1] && distances[0] <= distances[2] {
                            left
                        } else if distances[1] <= distances[2] {
                            above
                        } else {
                            upper_left
                        }
                    }
                    _ => unreachable!(),
                };
                data[target + column] = data[target + column].wrapping_add(prediction);
            }
        }
        data.truncate(rows * columns);
        Ok(data)
    }
}

fn inflate<R: Read>(
    source: R,
    length: u64,
    limit: usize,
    sync_flush_size: Option<usize>,
) -> Result<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut input = BufReader::new(source.take(length));
    let mut decoder = Decompress::new(true);
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut suffix = 0u32;
    loop {
        let bytes = input.fill_buf()?;
        let at_eof = bytes.is_empty();
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(bytes, &mut buffer, FlushDecompress::None)
            .map_err(|_| invalid("invalid Flate stream or checksum"))?;
        let consumed = (decoder.total_in() - before_in) as usize;
        let produced = (decoder.total_out() - before_out) as usize;
        for &byte in &bytes[consumed.saturating_sub(4)..consumed] {
            suffix = (suffix << 8) | u32::from(byte);
        }
        input.consume(consumed);
        if output
            .len()
            .checked_add(produced)
            .is_none_or(|size| size > limit)
        {
            return Err(invalid("decoded stream size limit exceeded"));
        }
        let needed = output.len() + produced;
        if needed > output.capacity() {
            let capacity = output
                .capacity()
                .saturating_mul(2)
                .max(8192)
                .min(limit)
                .max(needed);
            output
                .try_reserve_exact(capacity - output.len())
                .map_err(|_| invalid("unable to allocate decoded stream buffer"))?;
        }
        output.extend_from_slice(&buffer[..produced]);
        if status == Status::StreamEnd {
            if decoder.total_in() != length {
                return Err(invalid("trailing data in Flate stream"));
            }
            return Ok(output);
        }
        if at_eof && consumed == 0 && produced == 0 {
            // Some PDF producers terminate xref data with Z_SYNC_FLUSH, not Z_FINISH.
            // Permit only an exact-sized xref table ending in the flush marker.
            if decoder.total_in() == length
                && suffix == 0x0000ffff
                && sync_flush_size == Some(output.len())
            {
                log::warn!(
                    "PDF xref stream uses legacy sync-flush termination without a zlib trailer"
                );
                return Ok(output);
            }
            return Err(invalid("incomplete Flate stream"));
        }
        if consumed == 0 && produced == 0 {
            return Err(invalid("Flate decoder made no progress"));
        }
    }
}

struct Parser<R> {
    input: BufReader<R>,
    size: u64,
    budget: u64,
    nodes: usize,
    objects: usize,
    xrefs: Vec<Xrefs>,
    decoding_left: usize,
    resolving: BTreeSet<Reference>,
    object_stream: Option<ObjectStream>,
}

impl<R: Read + Seek> Parser<R> {
    fn new(input: R, size: u64) -> Self {
        Self {
            input: BufReader::new(input),
            size,
            budget: MAX_SYNTAX_BYTES,
            nodes: 0,
            objects: 0,
            xrefs: Vec::new(),
            decoding_left: MAX_DECODE_WORK,
            resolving: BTreeSet::new(),
            object_stream: None,
        }
    }

    fn position(&mut self) -> Result<u64> {
        Ok(self.input.stream_position()?)
    }

    fn seek(&mut self, offset: u64) -> Result<()> {
        if offset > self.size {
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
        let mut classic_count = 0usize;
        let mut storage = 0usize;
        for _ in 0..MAX_REVISIONS {
            self.seek(offset)?;
            let (mut trailer, section, highest) = if self.token()? == b"xref" {
                let mut entries = BTreeMap::new();
                loop {
                    self.skip_space()?;
                    if self.peek()? == Some(b't') {
                        break;
                    }
                    let first = self.number()?;
                    let count = self.number()?;
                    let count =
                        usize::try_from(count).map_err(|_| invalid("xref count overflow"))?;
                    entry_count = entry_count
                        .checked_add(count)
                        .filter(|n| *n <= MAX_ENTRIES)
                        .ok_or_else(|| invalid("cross-reference entry limit exceeded"))?;
                    // Classic tables have much higher per-entry overhead than compact streams.
                    classic_count = classic_count
                        .checked_add(count)
                        .filter(|n| *n <= 100_000)
                        .ok_or_else(|| invalid("classic cross-reference entry limit exceeded"))?;
                    let end = first
                        .checked_add(count as u64)
                        .filter(|end| *end <= u64::from(u32::MAX))
                        .ok_or_else(|| invalid("xref object number overflow"))?;
                    for id in first..end {
                        let object_offset = self.number()?;
                        let generation = u16::try_from(self.number()?)
                            .map_err(|_| invalid("generation overflow"))?;
                        let entry = match self.token()?.as_slice() {
                            b"n" if object_offset < self.size => Entry::Normal {
                                offset: object_offset,
                                generation,
                            },
                            b"f" => Entry::Free,
                            _ => return Err(invalid("invalid xref entry or offset")),
                        };
                        if entries.insert(id as u32, entry).is_some() {
                            return Err(invalid("overlapping xref subsections"));
                        }
                    }
                }
                self.expect(b"trailer")?;
                let highest = entries.last_key_value().map(|(&id, _)| id);
                (
                    self.value(0)?.dictionary()?,
                    Xrefs::Classic(entries),
                    highest,
                )
            } else {
                let object = self.raw_object(offset, None)?;
                let mut dict = object.value.dictionary()?;
                if !required(&mut dict, b"Type")?.is_name(b"XRef") {
                    return Err(invalid(
                        "startxref does not reference a cross-reference table",
                    ));
                }
                let size = required(&mut dict, b"Size")?.integer()?;
                if size > u64::from(u32::MAX) {
                    return Err(invalid("invalid trailer Size"));
                }
                let Value::Array(w) = required(&mut dict, b"W")? else {
                    return Err(invalid("invalid xref widths"));
                };
                if w.len() != 3 {
                    return Err(invalid("xref requires three field widths"));
                }
                let mut widths = [0usize; 3];
                for (target, value) in widths.iter_mut().zip(w) {
                    let width = value.integer()?;
                    if width > 8 {
                        return Err(invalid("xref field width exceeds 8 bytes"));
                    }
                    *target = width as usize;
                }
                let row_size: usize = widths.iter().sum();
                if row_size == 0 {
                    return Err(invalid("empty xref rows"));
                }
                let indices = match dict.remove(b"Index".as_slice()) {
                    Some(Value::Array(indices)) => indices,
                    None => vec![Value::Integer(0), Value::Integer(size)],
                    _ => return Err(invalid("invalid xref Index")),
                };
                if indices.len() % 2 != 0 {
                    return Err(invalid("odd xref Index length"));
                }
                let mut ranges = Vec::new();
                let mut end = 0;
                let mut rows = 0usize;
                let mut indices = indices.into_iter();
                while let Some(first) = indices.next() {
                    let first = first.integer()?;
                    let count = indices
                        .next()
                        .ok_or_else(|| invalid("missing xref count"))?
                        .integer()?;
                    let next = first
                        .checked_add(count)
                        .filter(|n| *n <= size)
                        .ok_or_else(|| invalid("xref Index outside Size"))?;
                    if first < end {
                        return Err(invalid("unordered or overlapping xref Index"));
                    }
                    end = next;
                    rows = rows
                        .checked_add(
                            usize::try_from(count).map_err(|_| invalid("xref count overflow"))?,
                        )
                        .filter(|n| *n <= MAX_ENTRIES)
                        .ok_or_else(|| invalid("cross-reference entry limit exceeded"))?;
                    ranges.push((first as u32, count as u32));
                }
                entry_count = entry_count
                    .checked_add(rows)
                    .filter(|n| *n <= MAX_ENTRIES)
                    .ok_or_else(|| invalid("cross-reference entry limit exceeded"))?;
                let decoded_size = rows
                    .checked_mul(row_size)
                    .filter(|n| *n <= MAX_STRUCTURAL_STREAM)
                    .ok_or_else(|| invalid("xref decoded size limit exceeded"))?;
                let decode = Decode::from_dictionary(&mut dict)?;
                let expected = if let Some(config) = decode.filter(|c| c.predictor != 1) {
                    if decoded_size % config.columns as usize != 0 {
                        return Err(invalid("xref predictor row mismatch"));
                    }
                    decoded_size
                        .checked_add(decoded_size / config.columns as usize)
                        .ok_or_else(|| invalid("xref predictor size overflow"))?
                } else {
                    decoded_size
                };
                let data =
                    self.structural_stream(object.stream, &mut dict, decode, Some(expected))?;
                if data.len() != decoded_size {
                    return Err(invalid("xref decoded length mismatch"));
                }
                storage = storage
                    .checked_add(data.len())
                    .filter(|n| *n <= MAX_XREF_STORAGE)
                    .ok_or_else(|| invalid("xref storage limit exceeded"))?;
                dict.insert(b"Size".to_vec(), Value::Integer(size));
                let highest = ranges
                    .last()
                    .and_then(|&(first, count)| count.checked_sub(1).map(|n| first + n));
                (
                    dict,
                    Xrefs::Stream {
                        ranges,
                        widths,
                        data,
                    },
                    highest,
                )
            };
            let size = required(&mut trailer, b"Size")?.integer()?;
            if size > u64::from(u32::MAX) || highest.is_some_and(|id| u64::from(id) >= size) {
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
            self.xrefs.push(section);
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
        if self.resolving.len() >= MAX_DEPTH || !self.resolving.insert(reference) {
            return Err(invalid("cyclic or excessively nested object resolution"));
        }
        let result = (|| match self.entry(reference.0)? {
            Entry::Normal { offset, generation } if generation == reference.1 => {
                self.raw_object(offset, Some(reference))
            }
            Entry::Compressed { container, index } if reference.1 == 0 => {
                self.compressed_object(reference.0, container, index)
            }
            _ => Err(invalid("stale or free object reference")),
        })();
        self.resolving.remove(&reference);
        result
    }

    fn entry(&self, id: u32) -> Result<Entry> {
        for section in &self.xrefs {
            if let Some(entry) = section.entry(id)? {
                return Ok(entry);
            }
        }
        Err(invalid("unresolved object"))
    }

    fn raw_object(&mut self, offset: u64, reference: Option<Reference>) -> Result<Object> {
        self.seek(offset)?;
        let number = self.number()?;
        let generation = self.number()?;
        if reference.is_some_and(|r| number != u64::from(r.0) || generation != u64::from(r.1)) {
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

    fn stream_range(
        &mut self,
        offset: Option<u64>,
        dict: &mut Dictionary,
        direct_length: bool,
    ) -> Result<ByteRange> {
        let offset = offset.ok_or_else(|| invalid("expected stream"))?;
        let value = required(dict, b"Length")?;
        let length = if direct_length {
            value.integer()?
        } else {
            self.resolve(value)?.integer()?
        };
        let end = offset
            .checked_add(length)
            .filter(|end| *end < self.size)
            .ok_or_else(|| invalid("stream length outside file"))?;
        self.seek(end)?;
        self.expect(b"endstream")?;
        self.expect(b"endobj")?;
        Ok(ByteRange::new(offset, length))
    }

    fn structural_stream(
        &mut self,
        offset: Option<u64>,
        dict: &mut Dictionary,
        decode: Option<Decode>,
        expected: Option<usize>,
    ) -> Result<Vec<u8>> {
        let range = self.stream_range(offset, dict, expected.is_some())?;
        let encoded = usize::try_from(range.size).map_err(|_| invalid("stream length overflow"))?;
        if encoded > MAX_STRUCTURAL_STREAM || encoded > self.decoding_left {
            return Err(invalid(
                "encoded structural stream size/work limit exceeded",
            ));
        }
        self.decoding_left -= encoded;
        let limit = MAX_STRUCTURAL_STREAM.min(self.decoding_left);
        let limit = expected.map_or(limit, |size| size.min(limit));
        self.seek(range.offset)?;
        let data = if let Some(config) = decode {
            let data = inflate(&mut self.input, range.size, limit, expected)?;
            self.decoding_left -= data.len();
            config.undo_predictor(data)?
        } else {
            if encoded > limit {
                return Err(invalid("decoded structural stream work limit exceeded"));
            }
            let mut data = vec![0; encoded];
            self.input.read_exact(&mut data)?;
            self.decoding_left -= data.len();
            data
        };
        Ok(data)
    }

    fn compressed_object(&mut self, id: u32, container: u32, index: u32) -> Result<Object> {
        if self
            .object_stream
            .as_ref()
            .is_none_or(|stream| stream.id != container)
        {
            if !matches!(self.entry(container)?, Entry::Normal { .. }) {
                return Err(invalid("object stream container is not an ordinary object"));
            }
            let object = self.object((container, 0))?;
            let mut dict = object.value.dictionary()?;
            if !required(&mut dict, b"Type")?.is_name(b"ObjStm") {
                return Err(invalid("compressed object container is not ObjStm"));
            }
            let count = required(&mut dict, b"N")?.integer()?;
            let first = required(&mut dict, b"First")?.integer()?;
            if count > 100_000 {
                return Err(invalid("object stream entry limit exceeded"));
            }
            let decode = Decode::from_dictionary(&mut dict)?;
            let data = self.structural_stream(object.stream, &mut dict, decode, None)?;
            let first = usize::try_from(first)
                .ok()
                .filter(|n| *n <= data.len())
                .ok_or_else(|| invalid("object stream First outside payload"))?;
            let mut parser = Parser::new(Cursor::new(&data[..first]), first as u64);
            parser.budget = self.budget;
            let mut objects = Vec::new();
            let mut ids = BTreeSet::new();
            for _ in 0..count {
                let object_id = u32::try_from(parser.number()?)
                    .map_err(|_| invalid("object number overflow"))?;
                let offset = usize::try_from(parser.number()?)
                    .map_err(|_| invalid("object offset overflow"))?;
                if offset >= data.len() - first
                    || objects
                        .last()
                        .is_some_and(|&(_, previous)| offset <= previous)
                    || !ids.insert(object_id)
                {
                    return Err(invalid(
                        "invalid object stream offsets or duplicate objects",
                    ));
                }
                objects.push((object_id, offset));
            }
            parser.skip_space()?;
            if parser.position()? != first as u64 {
                return Err(invalid("object stream header length mismatch"));
            }
            self.budget = parser.budget;
            self.object_stream = Some(ObjectStream {
                id: container,
                data,
                objects,
                first,
            });
        }
        let stream = self
            .object_stream
            .as_ref()
            .ok_or_else(|| invalid("missing object stream"))?;
        let &(object_id, start) = stream
            .objects
            .get(index as usize)
            .ok_or_else(|| invalid("compressed object index outside stream"))?;
        if id != object_id {
            return Err(invalid("compressed object identity mismatch"));
        }
        let end = stream
            .objects
            .get(index as usize + 1)
            .map(|&(_, offset)| offset)
            .unwrap_or(stream.data.len() - stream.first);
        let slice = &stream.data[stream.first + start..stream.first + end];
        let mut parser = Parser::new(Cursor::new(slice), slice.len() as u64);
        parser.budget = self.budget;
        parser.nodes = self.nodes;
        let value = parser.value(0)?;
        parser.skip_space()?;
        if parser.position()? != slice.len() as u64 {
            return Err(invalid("trailing syntax in compressed object"));
        }
        self.budget = parser.budget;
        self.nodes = parser.nodes;
        Ok(Object {
            value,
            stream: None,
        })
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

    fn stream(&mut self, value: Value, kind: SegmentKind) -> Result<(ByteRange, Option<Decode>)> {
        let Value::Reference(reference) = value else {
            return Err(invalid("stream must be indirect"));
        };
        let object = self.object(reference)?;
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
        let decode = Decode::from_dictionary(&mut dict)?;
        let range = self.stream_range(object.stream, &mut dict, false)?;
        if range.size > crate::segment::MAX_SEGMENT_SIZE {
            return Err(invalid("metadata stream exceeds 256 MiB limit"));
        }
        Ok((range, decode))
    }

    fn metadata(
        &mut self,
        root: Reference,
    ) -> Result<Vec<(ByteRange, SegmentKind, Option<Decode>)>> {
        let catalog_value = self.resolve(Value::Reference(root))?;
        let mut catalog = catalog_value.dictionary()?;
        if !required(&mut catalog, b"Type")?.is_name(b"Catalog") {
            return Err(invalid("Root is not a Catalog"));
        }
        let mut ranges = Vec::new();
        if let Some(value) = catalog.remove(b"Metadata".as_slice()) {
            if !matches!(value, Value::Null) {
                let (range, decode) = self.stream(value, SegmentKind::Xmp)?;
                ranges.push((range, SegmentKind::Xmp, decode));
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
                if ranges
                    .iter()
                    .any(|(_, kind, _)| *kind == SegmentKind::Jumbf)
                {
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
                let (range, decode) = self.stream(stream, SegmentKind::Jumbf)?;
                ranges.push((range, SegmentKind::Jumbf, decode));
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

    fn read_metadata<R: Read + Seek>(
        structure: &Structure,
        source: &mut R,
        index: Option<usize>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(index) = index else {
            return Ok(None);
        };
        let segment = &structure.segments[index];
        let range = segment.location();
        match segment.metadata {
            Some(crate::SegmentMetadata::PdfFlate { predictor, columns }) => {
                if range.size > crate::segment::MAX_SEGMENT_SIZE {
                    return Err(invalid("encoded metadata size limit exceeded"));
                }
                source.seek(SeekFrom::Start(range.offset))?;
                let data = inflate(source, range.size, MAX_DECODED_METADATA, None)?;
                Ok(Some(Decode { predictor, columns }.undo_predictor(data)?))
            }
            None => structure.read_range(source, range).map(Some),
            Some(_) => Err(invalid("unexpected metadata on PDF stream segment")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_foreign_segment_metadata_without_reading_payload() {
        let mut structure = Structure::new(ContainerKind::Pdf, MediaType::Pdf);
        structure.add_segment(Segment::new(0, 1, SegmentKind::Xmp, None).with_metadata(
            crate::SegmentMetadata::JpegExtendedXmp {
                guid: String::new(),
                chunk_offsets: Vec::new(),
                total_size: 0,
            },
        ));
        let mut source = Cursor::new(b"x");
        let error =
            PdfIO::read_metadata(&structure, &mut source, structure.xmp_index()).unwrap_err();
        assert!(error.to_string().contains("unexpected metadata"));
        assert_eq!(source.position(), 0);
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn inflate_enforces_exact_output_limit() {
        let data = deflate(&[b'x'; 1025]);
        assert!(inflate(data.as_slice(), data.len() as u64, 1024, None)
            .unwrap_err()
            .to_string()
            .contains("size limit"));
        let data = deflate(&[b'x'; 1024]);
        assert_eq!(
            inflate(data.as_slice(), data.len() as u64, 1024, None).unwrap(),
            vec![b'x'; 1024]
        );
    }

    #[test]
    fn rejects_incomplete_corrupt_and_trailing_flate_data() {
        let data = deflate(b"test");
        for end in 0..data.len() {
            assert!(inflate(&data[..end], end as u64, 1024, None).is_err());
        }
        let mut corrupt = data.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(inflate(corrupt.as_slice(), corrupt.len() as u64, 1024, None).is_err());
        let mut trailing = data;
        trailing.push(0);
        assert!(inflate(trailing.as_slice(), trailing.len() as u64, 1024, None).is_err());
    }

    #[test]
    fn sync_flush_tolerance_requires_exact_xref_size_and_marker() {
        let mut encoder = flate2::Compress::new(flate2::Compression::default(), true);
        let mut data = [0u8; 128];
        encoder
            .compress(b"1234567", &mut data, flate2::FlushCompress::Sync)
            .unwrap();
        let length = encoder.total_out() as usize;
        let data = &data[..length];
        assert_eq!(
            inflate(data, length as u64, 1024, Some(7)).unwrap(),
            b"1234567"
        );
        assert!(inflate(data, length as u64, 1024, Some(8)).is_err());
        assert!(inflate(data, length as u64, 1024, None).is_err());
        assert!(inflate(&data[..length - 1], (length - 1) as u64, 1024, Some(7)).is_err());
    }

    #[test]
    fn all_png_predictor_modes_reconstruct_rows_in_place() {
        let rows = [[5u8, 10, 20], [8, 17, 31]];
        for filter in 0..=4 {
            let mut encoded = Vec::new();
            for (row, values) in rows.iter().enumerate() {
                encoded.push(filter);
                for (column, &value) in values.iter().enumerate() {
                    let a = if column == 0 { 0 } else { values[column - 1] };
                    let b = if row == 0 { 0 } else { rows[row - 1][column] };
                    let c = if row == 0 || column == 0 {
                        0
                    } else {
                        rows[row - 1][column - 1]
                    };
                    let predicted = match filter {
                        0 => 0,
                        1 => a,
                        2 => b,
                        3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                        4 => {
                            let p = i32::from(a) + i32::from(b) - i32::from(c);
                            let pa = (p - i32::from(a)).abs();
                            let pb = (p - i32::from(b)).abs();
                            let pc = (p - i32::from(c)).abs();
                            if pa <= pb && pa <= pc {
                                a
                            } else if pb <= pc {
                                b
                            } else {
                                c
                            }
                        }
                        _ => unreachable!(),
                    };
                    encoded.push(value.wrapping_sub(predicted));
                }
            }
            assert_eq!(
                Decode {
                    predictor: 15,
                    columns: 3
                }
                .undo_predictor(encoded)
                .unwrap(),
                rows.concat()
            );
        }
        assert!(Decode {
            predictor: 12,
            columns: 3
        }
        .undo_predictor(vec![2, 1])
        .is_err());
        assert!(Decode {
            predictor: 12,
            columns: 1
        }
        .undo_predictor(vec![5, 1])
        .is_err());
    }

    #[test]
    fn structural_decode_has_cumulative_not_just_per_stream_limits() {
        let data = deflate(&[b'x'; 1024]);
        let mut source = data.clone();
        source.extend_from_slice(b"\nendstream\nendobj\n");
        let mut parser = Parser::new(Cursor::new(&source), source.len() as u64);
        parser.decoding_left = (data.len() + 1024) * 2;
        for _ in 0..2 {
            let mut dict =
                Dictionary::from([(b"Length".to_vec(), Value::Integer(data.len() as u64))]);
            let decoded = parser
                .structural_stream(
                    Some(0),
                    &mut dict,
                    Some(Decode {
                        predictor: 1,
                        columns: 1,
                    }),
                    None,
                )
                .unwrap();
            assert_eq!(decoded.len(), 1024);
        }
        assert_eq!(parser.decoding_left, 0);
        let mut dict = Dictionary::from([(b"Length".to_vec(), Value::Integer(data.len() as u64))]);
        assert!(parser
            .structural_stream(
                Some(0),
                &mut dict,
                Some(Decode {
                    predictor: 1,
                    columns: 1
                }),
                None
            )
            .is_err());
    }

    #[test]
    fn structural_input_limit_is_checked_before_allocating_or_reading_payload() {
        let length = MAX_STRUCTURAL_STREAM + 1;
        let mut data = vec![0u8; length];
        data.extend_from_slice(b"\nendstream\nendobj\n");
        let mut parser = Parser::new(Cursor::new(&data), data.len() as u64);
        let mut dict = Dictionary::from([(b"Length".to_vec(), Value::Integer(length as u64))]);
        assert!(parser
            .structural_stream(Some(0), &mut dict, None, None)
            .unwrap_err()
            .to_string()
            .contains("encoded structural stream"));
        assert_eq!(parser.decoding_left, MAX_DECODE_WORK);
    }

    #[test]
    fn xref_decoding_stops_at_declared_table_size_not_general_stream_limit() {
        let data = deflate(&[b'x'; 1024]);
        let mut source = data.clone();
        source.extend_from_slice(b"\nendstream\nendobj\n");
        let mut parser = Parser::new(Cursor::new(&source), source.len() as u64);
        let mut dict = Dictionary::from([(b"Length".to_vec(), Value::Integer(data.len() as u64))]);
        let result = parser.structural_stream(
            Some(0),
            &mut dict,
            Some(Decode {
                predictor: 1,
                columns: 1,
            }),
            Some(7),
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("decoded stream size limit"));
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
        let mut parser = Parser::new(source, size);
        let root = parser.xrefs(offset)?;
        let mut metadata = parser.metadata(root)?;
        metadata.sort_by_key(|(range, _, _)| range.offset);
        let mut structure = Structure::new(ContainerKind::Pdf, MediaType::Pdf);
        structure.total_size = size;
        structure.add_segment(Segment::new(0, 9, SegmentKind::Header, Some("PDF".into())));
        let mut end = 9;
        for (range, kind, decode) in metadata {
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
            let mut segment = Segment::new(
                range.offset,
                range.size,
                kind,
                Some(format!("PDF/{}", kind.as_str())),
            );
            if let Some(config) = decode {
                segment.metadata = Some(crate::SegmentMetadata::PdfFlate {
                    predictor: config.predictor,
                    columns: config.columns,
                });
            }
            structure.add_segment(segment);
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
        Self::read_metadata(structure, source, structure.xmp_index())
    }

    fn read_jumbf<R: Read + Seek>(
        &self,
        structure: &Structure,
        source: &mut R,
    ) -> Result<Option<Vec<u8>>> {
        Self::read_metadata(structure, source, structure.c2pa_jumbf_index())
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
