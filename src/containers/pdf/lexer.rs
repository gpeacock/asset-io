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

//! PDF syntax: values and a lexer over a window of the file.
//!
//! The lexer is synchronous and works on bytes already in memory. When it runs out
//! of them before the end of the file it stops with [`Stop::More`], and the caller
//! reads a larger window and tries again. That keeps the syntax code free of I/O
//! (and of `async` recursion), and the limits below apply to what is consumed, not to
//! how many attempts it took.

use crate::error::Error;
use std::collections::BTreeMap;

pub(super) const MAX_DEPTH: usize = 32;
pub(super) const MAX_NODES: usize = 100_000;
pub(super) const MAX_TOKEN: usize = 4096;
pub(super) const MAX_SYNTAX_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn invalid(message: &str) -> Error {
    Error::InvalidFormat(format!("PDF: {message}"))
}

pub(super) type Dictionary = BTreeMap<Vec<u8>, Value>;
pub(super) type Reference = (u32, u16);

/// Why lexing stopped.
#[derive(Debug)]
pub(super) enum Stop {
    /// The window ended before the file did: read more and try again.
    More,
    Fail(Error),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Self {
        Stop::Fail(e)
    }
}

pub(super) type Lex<T> = std::result::Result<T, Stop>;

fn bad(message: &str) -> Stop {
    Stop::Fail(invalid(message))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Integer(u64),
    Reference(Reference),
    Name(Vec<u8>),
    Dictionary(Dictionary),
    Array(Vec<Value>),
    Null,
    Bytes(Vec<u8>),
    Scalar(Vec<u8>),
}

impl Value {
    pub(super) fn dictionary(self) -> Result<Dictionary, Error> {
        match self {
            Self::Dictionary(dict) => Ok(dict),
            _ => Err(invalid("expected dictionary")),
        }
    }

    pub(super) fn integer(self) -> Result<u64, Error> {
        match self {
            Self::Integer(value) => Ok(value),
            _ => Err(invalid("expected nonnegative integer")),
        }
    }

    pub(super) fn is_name(&self, name: &[u8]) -> bool {
        matches!(self, Self::Name(value) if value == name)
    }
}

pub(super) fn required(dict: &mut Dictionary, key: &[u8]) -> Result<Value, Error> {
    dict.remove(key)
        .ok_or_else(|| invalid("missing required dictionary key"))
}

pub(super) fn whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

pub(super) fn delimiter(byte: u8) -> bool {
    whitespace(byte) || b"()<>[]{}/%".contains(&byte)
}

pub(super) fn unsigned(bytes: &[u8]) -> Result<u64, Error> {
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

fn hex(byte: u8) -> Result<u8, Error> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(invalid("invalid hexadecimal digit")),
    }
}

/// A lexer over `data`, which starts at offset `base` of the file.
pub(super) struct Lexer<'a> {
    data: &'a [u8],
    /// Where the next byte is, in `data`.
    pub(super) at: usize,
    base: u64,
    /// `data` reaches the end of the file, so running out is the end of the file.
    eof: bool,
    /// Syntax bytes this lexer may still consume.
    pub(super) budget: u64,
    /// Syntax nodes created so far, in the whole parse.
    pub(super) nodes: usize,
}

impl<'a> Lexer<'a> {
    pub(super) fn new(data: &'a [u8], base: u64, eof: bool, budget: u64, nodes: usize) -> Self {
        Self {
            data,
            at: 0,
            base,
            eof,
            budget,
            nodes,
        }
    }

    /// The file offset of the next byte.
    pub(super) fn offset(&self) -> u64 {
        self.base + self.at as u64
    }

    pub(super) fn peek(&self) -> Lex<Option<u8>> {
        match self.data.get(self.at) {
            Some(&b) => Ok(Some(b)),
            None if self.eof => Ok(None),
            None => Err(Stop::More),
        }
    }

    pub(super) fn byte(&mut self) -> Lex<u8> {
        self.budget = self
            .budget
            .checked_sub(1)
            .ok_or_else(|| bad("syntax work limit exceeded"))?;
        let byte = self.peek()?.ok_or_else(|| bad("unexpected end of file"))?;
        self.at += 1;
        Ok(byte)
    }

    pub(super) fn skip_space(&mut self) -> Lex<()> {
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

    pub(super) fn token(&mut self) -> Lex<Vec<u8>> {
        self.skip_space()?;
        let mut token = Vec::new();
        while let Some(byte) = self.peek()? {
            if delimiter(byte) {
                break;
            }
            if token.len() == MAX_TOKEN {
                return Err(bad("token limit exceeded"));
            }
            token.push(self.byte()?);
        }
        if token.is_empty() {
            return Err(bad("expected token"));
        }
        Ok(token)
    }

    pub(super) fn expect(&mut self, expected: &[u8]) -> Lex<()> {
        if self.token()? != expected {
            return Err(bad("unexpected keyword"));
        }
        Ok(())
    }

    pub(super) fn number(&mut self) -> Lex<u64> {
        Ok(unsigned(&self.token()?)?)
    }

    pub(super) fn value(&mut self, depth: usize) -> Lex<Value> {
        if depth >= MAX_DEPTH || self.nodes >= MAX_NODES {
            return Err(bad("object complexity limit exceeded"));
        }
        self.nodes += 1;
        self.skip_space()?;
        match self.peek()?.ok_or_else(|| bad("missing value"))? {
            b'/' => {
                self.byte()?;
                let mut name = Vec::new();
                while let Some(byte) = self.peek()? {
                    if delimiter(byte) {
                        break;
                    }
                    if name.len() == MAX_TOKEN {
                        return Err(bad("name limit exceeded"));
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
                    let mut bytes = Vec::new();
                    let mut high = None;
                    loop {
                        let byte = self.byte()?;
                        if byte == b'>' {
                            break;
                        }
                        if !whitespace(byte) {
                            let digit = hex(byte)?;
                            if let Some(first) = high.take() {
                                bytes.push(first * 16 + digit);
                            } else {
                                high = Some(digit);
                            }
                        }
                    }
                    if let Some(first) = high {
                        bytes.push(first * 16);
                    }
                    return Ok(Value::Bytes(bytes));
                }
                self.byte()?;
                let mut dict = Dictionary::new();
                loop {
                    self.skip_space()?;
                    if self.peek()? == Some(b'>') {
                        self.byte()?;
                        if self.byte()? != b'>' {
                            return Err(bad("unterminated dictionary"));
                        }
                        break;
                    }
                    let Value::Name(key) = self.value(depth + 1)? else {
                        return Err(bad("dictionary key is not a name"));
                    };
                    let value = self.value(depth + 1)?;
                    if dict.insert(key, value).is_some() {
                        return Err(bad("duplicate dictionary key"));
                    }
                }
                Ok(Value::Dictionary(dict))
            }
            b'(' => {
                self.byte()?;
                let mut nesting = 1;
                let mut bytes = Vec::new();
                while nesting != 0 {
                    let byte = self.byte()?;
                    match byte {
                        b'\\' => {
                            let escaped = self.byte()?;
                            match escaped {
                                b'n' => bytes.push(b'\n'),
                                b'r' => bytes.push(b'\r'),
                                b't' => bytes.push(b'\t'),
                                b'b' => bytes.push(8),
                                b'f' => bytes.push(12),
                                b'\n' => {}
                                b'\r' => {
                                    if self.peek()? == Some(b'\n') {
                                        self.byte()?;
                                    }
                                }
                                b'0'..=b'7' => {
                                    let mut value = u16::from(escaped - b'0');
                                    for _ in 0..2 {
                                        if let Some(next @ b'0'..=b'7') = self.peek()? {
                                            self.byte()?;
                                            value = value * 8 + u16::from(next - b'0');
                                        } else {
                                            break;
                                        }
                                    }
                                    bytes.push(value as u8);
                                }
                                other => bytes.push(other),
                            }
                        }
                        b'(' => {
                            nesting += 1;
                            if nesting > MAX_DEPTH {
                                return Err(bad("string nesting limit exceeded"));
                            }
                            bytes.push(byte);
                        }
                        b')' => {
                            nesting -= 1;
                            if nesting != 0 {
                                bytes.push(byte);
                            }
                        }
                        b'\r' => {
                            if self.peek()? == Some(b'\n') {
                                self.byte()?;
                            }
                            bytes.push(b'\n');
                        }
                        _ => bytes.push(byte),
                    }
                }
                Ok(Value::Bytes(bytes))
            }
            _ => {
                let token = self.token()?;
                if token == b"null" {
                    return Ok(Value::Null);
                }
                if token == b"true" || token == b"false" {
                    return Ok(Value::Scalar(token));
                }
                let digits = token.strip_prefix(b"+").unwrap_or(&token);
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                    let number = unsigned(&token)?;
                    // A reference is `number generation R`; remember where the
                    // integer ended in case it is not one.
                    let end = self.at;
                    self.skip_space()?;
                    if matches!(self.peek()?, Some(b'0'..=b'9')) {
                        let generation = self.token()?;
                        self.skip_space()?;
                        if generation.iter().all(u8::is_ascii_digit) && self.peek()? == Some(b'R') {
                            self.expect(b"R")?;
                            return Ok(Value::Reference((
                                u32::try_from(number).map_err(|_| bad("object number overflow"))?,
                                u16::try_from(unsigned(&generation)?)
                                    .map_err(|_| bad("generation overflow"))?,
                            )));
                        }
                    }
                    self.at = end;
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
                    return Ok(Value::Scalar(token));
                }
                Err(bad("invalid value"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex<T>(data: &[u8], eof: bool, f: impl Fn(&mut Lexer<'_>) -> Lex<T>) -> Lex<T> {
        f(&mut Lexer::new(data, 0, eof, MAX_SYNTAX_BYTES, 0))
    }

    #[test]
    fn values_parse_as_they_did_before() {
        let v = lex(
            b"<< /A [1 2 0 R (a\\)b) <4142> /N#41me 1.5 null] /B << >> >> ",
            true,
            |l| l.value(0),
        )
        .unwrap();
        let Value::Dictionary(d) = v else { panic!() };
        let Value::Array(a) = &d[b"A".as_slice()] else {
            panic!()
        };
        assert_eq!(a[0], Value::Integer(1));
        assert_eq!(a[1], Value::Reference((2, 0)));
        assert_eq!(a[2], Value::Bytes(b"a)b".to_vec()));
        assert_eq!(a[3], Value::Bytes(b"AB".to_vec()));
        assert_eq!(a[4], Value::Name(b"NAme".to_vec()));
        assert_eq!(a[5], Value::Scalar(b"1.5".to_vec()));
        assert_eq!(a[6], Value::Null);
        assert_eq!(d[b"B".as_slice()], Value::Dictionary(Dictionary::new()));
    }

    #[test]
    fn a_window_that_ends_before_the_file_asks_for_more() {
        let text = b"<< /A 12 /B [1 2 3] >>";
        for cut in 0..text.len() {
            let r = lex(&text[..cut], false, |l| l.value(0));
            assert!(matches!(r, Err(Stop::More)), "cut at {cut}: {r:?}");
        }
        // The whole value is there: it needs nothing after it.
        assert!(lex(text, false, |l| l.value(0)).is_ok());
        assert!(lex(text, true, |l| l.value(0)).is_ok());
        // The same bytes at the end of the file are an error, not a request for more.
        assert!(matches!(
            lex(&text[..text.len() - 3], true, |l| l.value(0)),
            Err(Stop::Fail(_))
        ));
    }

    #[test]
    fn an_integer_not_followed_by_a_reference_is_an_integer() {
        let mut l = Lexer::new(b"12 345 ", 0, true, MAX_SYNTAX_BYTES, 0);
        assert_eq!(l.value(0).unwrap(), Value::Integer(12));
        assert_eq!(l.value(0).unwrap(), Value::Integer(345));
    }

    #[test]
    fn limits_apply_to_what_is_consumed() {
        let deep = vec![b'['; MAX_DEPTH + 1];
        assert!(matches!(
            lex(&deep, true, |l| l.value(0)),
            Err(Stop::Fail(_))
        ));
        let mut l = Lexer::new(b"/Name", 0, true, 3, 0);
        assert!(matches!(l.value(0), Err(Stop::Fail(_))));
        let dup = b"<< /A 1 /A 2 >>";
        assert!(matches!(lex(dup, true, |l| l.value(0)), Err(Stop::Fail(_))));
    }
}
