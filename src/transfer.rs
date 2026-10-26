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

//! Transfer encodings: how a container wraps the bytes it stores for an item
//! (base64 text, a Flate stream), as opposed to the [`Encoding`](crate::Encoding)
//! of the content itself.
//!
//! The core always identifies a transfer encoding; decoding a compressed one needs
//! the `flate` or `brotli` feature.
//!
//! [`TransferEncoding::decode`] and [`TransferEncoding::encode`] convert between
//! what is stored (what [`Structure::read_item`](crate::Structure::read_item)
//! returns, and what a hash covers) and the content
//! ([`Structure::read_content`](crate::Structure::read_content)). The helpers here
//! are also what handlers use for the same wrappings elsewhere in a file.

use crate::{Error, Result};
use std::borrow::Cow;

/// Largest content a compressed item may decode to (64 MiB).
pub const MAX_DECODED_LEN: usize = 64 * 1024 * 1024;

/// Longest native label kept for an [`Unknown`](TransferEncoding::Unknown) encoding.
pub const MAX_LABEL_LEN: usize = 255;

/// How the bytes stored for an item wrap its content.
///
/// The core always identifies the wrapping, even when it cannot undo it: the
/// decoders are optional features (`flate` for [`Zlib`](Self::Zlib),
/// [`Deflate`](Self::Deflate) and [`Gzip`](Self::Gzip); `brotli` for
/// [`Brotli`](Self::Brotli)).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum TransferEncoding {
    /// Stored as is.
    #[default]
    Identity,
    /// Standard base64 (RFC 4648 alphabet, with padding), as SVG stores a manifest.
    /// Whitespace is ignored when decoding.
    Base64,
    /// A zlib stream (RFC 1950), as PDF `FlateDecode` stores a stream, optionally
    /// followed by PNG predictor rows of this many 8-bit single-component columns
    /// (PDF `Predictor` 10 to 15).
    Zlib {
        /// Columns per PNG predictor row, or `None` when there is no predictor.
        png_predictor_columns: Option<u32>,
    },
    /// A raw deflate stream (RFC 1951). Decoding also accepts a zlib-wrapped stream,
    /// recognised by a valid zlib header, since some writers label zlib data
    /// `deflate`.
    Deflate,
    /// A gzip stream (RFC 1952).
    Gzip,
    /// A Brotli stream (RFC 7932).
    Brotli,
    /// An encoding the core does not know, with the label the format gave it
    /// (printable ASCII, at most [`MAX_LABEL_LEN`] bytes).
    Unknown(Cow<'static, str>),
}

impl TransferEncoding {
    /// An [`Unknown`](Self::Unknown) encoding labelled `label`, or `None` if the label
    /// is empty, longer than [`MAX_LABEL_LEN`] or not printable ASCII.
    pub fn unknown(label: &str) -> Option<Self> {
        let ok = !label.is_empty()
            && label.len() <= MAX_LABEL_LEN
            && label.bytes().all(|c| (0x20..0x7f).contains(&c));
        ok.then(|| Self::Unknown(Cow::Owned(label.to_owned())))
    }

    /// `"identity"`, `"base64"`, `"zlib"`, `"deflate"`, `"gzip"`, `"brotli"` or
    /// `"unknown"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Base64 => "base64",
            Self::Zlib { .. } => "zlib",
            Self::Deflate => "deflate",
            Self::Gzip => "gzip",
            Self::Brotli => "brotli",
            Self::Unknown(_) => "unknown",
        }
    }

    /// The native label of an [`Unknown`](Self::Unknown) encoding.
    pub fn unknown_name(&self) -> Option<&str> {
        match self {
            Self::Unknown(name) => Some(name),
            _ => None,
        }
    }

    /// The content of `stored` bytes. Decompressed content is bounded by
    /// [`MAX_DECODED_LEN`]. An encoding whose decoder is not built in (see the type's
    /// documentation), and an [`Unknown`](Self::Unknown) one, is
    /// [`Error::UnsupportedFeature`].
    pub fn decode(&self, stored: Vec<u8>) -> Result<Vec<u8>> {
        let missing = match self {
            Self::Zlib { .. } if !cfg!(feature = "flate") => {
                Some("zlib decoding (enable the `flate` feature)")
            }
            Self::Deflate if !cfg!(feature = "flate") => {
                Some("deflate decoding (enable the `flate` feature)")
            }
            Self::Gzip if !cfg!(feature = "flate") => {
                Some("gzip decoding (enable the `flate` feature)")
            }
            Self::Brotli if !cfg!(feature = "brotli") => {
                Some("Brotli decoding (enable the `brotli` feature)")
            }
            _ => None,
        };
        if let Some(missing) = missing {
            return Err(Error::UnsupportedFeature(missing));
        }
        match self {
            Self::Identity => Ok(stored),
            Self::Base64 => base64_decode(&stored),
            Self::Zlib {
                png_predictor_columns,
            } => {
                let data = inflate(&stored, MAX_DECODED_LEN, None)?;
                match png_predictor_columns {
                    Some(columns) => undo_png_predictor(data, *columns),
                    None => Ok(data),
                }
            }
            Self::Deflate if is_zlib(&stored) => inflate(&stored, MAX_DECODED_LEN, None),
            Self::Deflate => inflate_raw(&stored, MAX_DECODED_LEN),
            Self::Gzip => gunzip(&stored, MAX_DECODED_LEN),
            Self::Brotli => unbrotli(&stored, MAX_DECODED_LEN),
            Self::Unknown(_) => Err(Error::UnsupportedFeature(
                "decoding an unknown transfer encoding",
            )),
        }
    }

    /// At most the first `n` bytes of the content, decoding only as much of
    /// `stored` as that needs. `stored` may be a prefix of what is stored. Fewer
    /// bytes are returned when the content is shorter. Errors as
    /// [`decode`](Self::decode) does.
    pub fn decode_prefix(&self, stored: &[u8], n: usize) -> Result<Vec<u8>> {
        match self {
            Self::Identity => Ok(stored[..stored.len().min(n)].to_vec()),
            Self::Base64 => {
                let chars: Vec<u8> = stored
                    .iter()
                    .copied()
                    .filter(|c| !c.is_ascii_whitespace())
                    .take(n.div_ceil(3) * 4)
                    .collect();
                let whole = chars.len() / 4 * 4;
                let mut out = base64_decode(&chars[..whole])?;
                out.truncate(n);
                Ok(out)
            }
            Self::Zlib {
                png_predictor_columns: Some(columns),
            } => {
                let columns = (*columns).max(1) as usize;
                let rows = n.div_ceil(columns);
                let mut data = stream_prefix(self, stored, rows * (columns + 1))?;
                data.truncate(data.len() / (columns + 1) * (columns + 1));
                let mut out = undo_png_predictor(data, columns as u32)?;
                out.truncate(n);
                Ok(out)
            }
            Self::Unknown(_) => Err(Error::UnsupportedFeature(
                "decoding an unknown transfer encoding",
            )),
            _ => stream_prefix(self, stored, n),
        }
    }

    /// The bytes to store for `content`. Only [`Identity`](Self::Identity) and
    /// [`Base64`](Self::Base64) can be written; anything else is
    /// [`Error::UnsupportedFeature`].
    pub fn encode<'a>(&self, content: &'a [u8]) -> Result<Cow<'a, [u8]>> {
        match self {
            Self::Identity => Ok(Cow::Borrowed(content)),
            Self::Base64 => Ok(Cow::Owned(base64_encode(content))),
            _ => Err(Error::UnsupportedFeature(
                "encoding a compressed transfer encoding",
            )),
        }
    }
}

impl std::fmt::Display for TransferEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown ({name})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// Whether `data` starts with a valid zlib header: deflate method, a window of at
/// most 32 KiB, and a header checksum that holds.
fn is_zlib(data: &[u8]) -> bool {
    match data {
        [cmf, flg, ..] => {
            cmf & 0x0f == 8 && cmf >> 4 <= 7 && (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 == 0
        }
        _ => false,
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]);
            } else {
                out.push(b'=');
            }
        }
    }
    out
}

/// Decode standard base64 with padding, ignoring whitespace (which an XML text
/// may have).
pub fn base64_decode(text: &[u8]) -> Result<Vec<u8>> {
    let bad = |why: &str| Error::InvalidFormat(format!("not base64: {why}"));
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut n, mut bits, mut pad) = (0u32, 0, 0);
    for &c in text.iter().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            _ => return Err(bad("an invalid character")),
        };
        if pad > 0 {
            return Err(bad("data after padding"));
        }
        n = n << 6 | v as u32;
        bits += 6;
        if bits == 24 {
            out.extend_from_slice(&n.to_be_bytes()[1..]);
            (n, bits) = (0, 0);
        }
    }
    match (bits, pad) {
        (0, 0) => {}
        (12, 2) => out.push((n >> 4) as u8),
        (18, 1) => out.extend_from_slice(&[(n >> 10) as u8, (n >> 2) as u8]),
        _ => return Err(bad("the length or padding is wrong")),
    }
    Ok(out)
}

fn flate_error(message: &str) -> Error {
    Error::InvalidFormat(format!("Flate: {message}"))
}

/// Inflate zlib data, which must be all of `input`, to at most `limit` bytes.
///
/// `sync_flush_size`: some producers end a stream with a sync flush instead of a
/// finished zlib stream (PDF cross-reference streams). That is accepted, only for
/// data that ends in the flush marker and decodes to exactly this size.
///
/// Needs the `flate` feature; without it this is [`Error::UnsupportedFeature`].
pub fn inflate(input: &[u8], limit: usize, sync_flush_size: Option<usize>) -> Result<Vec<u8>> {
    inflate_with(input, limit, sync_flush_size, true)
}

/// Inflate a raw deflate stream (no zlib header), which must be all of `input`.
fn inflate_raw(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    inflate_with(input, limit, None, false)
}

#[cfg(feature = "flate")]
fn inflate_with(
    input: &[u8],
    limit: usize,
    sync_flush_size: Option<usize>,
    zlib: bool,
) -> Result<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut decoder = Decompress::new(zlib);
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut used = 0usize;
    loop {
        let (before_in, before_out) = (decoder.total_in(), decoder.total_out());
        let status = decoder
            .decompress(&input[used..], &mut buffer, FlushDecompress::None)
            .map_err(|_| flate_error("invalid Flate stream or checksum"))?;
        let consumed = (decoder.total_in() - before_in) as usize;
        let produced = (decoder.total_out() - before_out) as usize;
        used += consumed;
        if output.len().checked_add(produced).is_none_or(|n| n > limit) {
            return Err(flate_error("decoded stream size limit exceeded"));
        }
        output.extend_from_slice(&buffer[..produced]);
        if status == Status::StreamEnd {
            if used != input.len() {
                return Err(flate_error("trailing data in Flate stream"));
            }
            return Ok(output);
        }
        if consumed == 0 && produced == 0 {
            if used == input.len()
                && input.ends_with(&[0, 0, 0xff, 0xff])
                && sync_flush_size == Some(output.len())
            {
                return Ok(output);
            }
            return Err(flate_error(if used == input.len() {
                "incomplete Flate stream"
            } else {
                "Flate decoder made no progress"
            }));
        }
    }
}

#[cfg(not(feature = "flate"))]
fn inflate_with(_: &[u8], _: usize, _: Option<usize>, _: bool) -> Result<Vec<u8>> {
    Err(Error::UnsupportedFeature(
        "Flate decoding (enable the `flate` feature)",
    ))
}

/// The first `n` bytes a streaming decoder gives for `stored`; a stream that ends
/// early (or a truncated `stored`) gives what was decoded before the end.
#[cfg(any(feature = "flate", feature = "brotli"))]
fn stream_prefix(encoding: &TransferEncoding, stored: &[u8], n: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let reader: Box<dyn Read + '_> = match encoding {
        #[cfg(feature = "flate")]
        TransferEncoding::Zlib { .. } => Box::new(flate2::read::ZlibDecoder::new(stored)),
        #[cfg(feature = "flate")]
        TransferEncoding::Deflate if is_zlib(stored) => {
            Box::new(flate2::read::ZlibDecoder::new(stored))
        }
        #[cfg(feature = "flate")]
        TransferEncoding::Deflate => Box::new(flate2::read::DeflateDecoder::new(stored)),
        #[cfg(feature = "flate")]
        TransferEncoding::Gzip => Box::new(flate2::read::GzDecoder::new(stored)),
        #[cfg(feature = "brotli")]
        TransferEncoding::Brotli => Box::new(brotli_decompressor::Decompressor::new(stored, 4096)),
        other => return missing_decoder(other),
    };
    let mut out = vec![0u8; n];
    let mut filled = 0;
    let mut reader = reader;
    while filled < n {
        match reader.read(&mut out[filled..]) {
            Ok(0) => break,
            Ok(k) => filled += k,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && filled > 0 => break,
            Err(e) => return Err(Error::InvalidFormat(format!("{}: {e}", encoding.as_str()))),
        }
    }
    out.truncate(filled);
    Ok(out)
}

#[cfg(not(any(feature = "flate", feature = "brotli")))]
fn stream_prefix(encoding: &TransferEncoding, _: &[u8], _: usize) -> Result<Vec<u8>> {
    missing_decoder(encoding)
}

fn missing_decoder(encoding: &TransferEncoding) -> Result<Vec<u8>> {
    // `decode` names the feature to enable; reuse its message.
    match encoding.decode(Vec::new()) {
        Err(e @ Error::UnsupportedFeature(_)) => Err(e),
        _ => Err(Error::UnsupportedFeature("decoder not built in")),
    }
}

/// Read all of `reader`, failing above `limit` bytes.
#[cfg(any(feature = "flate", feature = "brotli"))]
fn read_bounded(reader: impl std::io::Read, limit: usize, what: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| Error::InvalidFormat(format!("{what}: {e}")))?;
    if out.len() > limit {
        return Err(Error::InvalidFormat(format!(
            "{what}: decoded size limit exceeded"
        )));
    }
    Ok(out)
}

#[cfg(feature = "flate")]
fn gunzip(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    read_bounded(flate2::read::GzDecoder::new(input), limit, "gzip")
}

#[cfg(not(feature = "flate"))]
fn gunzip(_: &[u8], _: usize) -> Result<Vec<u8>> {
    Err(Error::UnsupportedFeature(
        "gzip decoding (enable the `flate` feature)",
    ))
}

#[cfg(feature = "brotli")]
fn unbrotli(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    read_bounded(
        brotli_decompressor::Decompressor::new(input, 4096),
        limit,
        "Brotli",
    )
}

#[cfg(not(feature = "brotli"))]
fn unbrotli(_: &[u8], _: usize) -> Result<Vec<u8>> {
    Err(Error::UnsupportedFeature(
        "Brotli decoding (enable the `brotli` feature)",
    ))
}

/// Undo PNG predictors (filter types 0 to 4, one per row) over rows of `columns`
/// 8-bit single-component samples, each row led by its filter byte.
pub fn undo_png_predictor(mut data: Vec<u8>, columns: u32) -> Result<Vec<u8>> {
    let columns = columns as usize;
    if columns == 0 {
        return Err(flate_error("PNG predictor rows have no columns"));
    }
    let stride = columns + 1;
    if !data.chunks_exact(stride).remainder().is_empty() {
        return Err(flate_error("incomplete PNG predictor row"));
    }
    let rows = data.len() / stride;
    for row in 0..rows {
        let source = row * stride;
        let target = row * columns;
        let filter = data[source];
        if filter > 4 {
            return Err(flate_error("invalid PNG predictor filter"));
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
                _ => unreachable!("filter was checked above"),
            };
            data[target + column] = data[target + column].wrapping_add(prediction);
        }
    }
    data.truncate(rows * columns);
    Ok(data)
}

/// Compressors for tests elsewhere in the crate, which keep `flate2` here.
#[cfg(all(test, feature = "flate"))]
pub(crate) mod compress {
    use flate2::write::{DeflateEncoder, ZlibEncoder};
    use flate2::Compression;
    use std::io::Write;

    pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    pub(crate) fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }
}

#[cfg(test)]
/// A Brotli stream holding `data` (at most 65536 bytes) in one uncompressed
/// meta-block: WBITS 16, a header of 21 bits padded to 3 bytes, the data, then
/// an empty last meta-block.
pub(crate) fn brotli_stored(data: &[u8]) -> Vec<u8> {
    let mlen = (data.len() - 1) as u32;
    // bit 0 WBITS=0, bit 1 ISLAST=0, bits 2-3 MNIBBLES=0 (4), bits 4-19 MLEN-1,
    // bit 20 ISUNCOMPRESSED=1.
    let bits = (mlen << 4) | (1 << 20);
    let mut out = bits.to_le_bytes()[..3].to_vec();
    out.extend_from_slice(data);
    out.push(0x03); // ISLAST=1, ISLASTEMPTY=1
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_every_length() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n as u8).map(|b| b.wrapping_mul(37)).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
            let t = TransferEncoding::Base64;
            assert_eq!(
                t.decode(t.encode(&data).unwrap().into_owned()).unwrap(),
                data
            );
        }
        assert_eq!(base64_encode(b"Man"), b"TWFu");
        assert_eq!(base64_encode(b"Ma"), b"TWE=");
        assert_eq!(base64_encode(b"M"), b"TQ==");
    }

    #[test]
    fn base64_rejects_what_is_not_base64() {
        for bad in [&b"TWE"[..], b"TW=u", b"T!Fu", b"TQ==TQ==", b"TQ="] {
            assert!(
                base64_decode(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(base64_decode(b" TW\nFu ").unwrap(), b"Man");
    }

    #[test]
    fn identity_is_unchanged_and_compressed_encodings_do_not_encode() {
        let t = TransferEncoding::default();
        assert_eq!(t, TransferEncoding::Identity);
        assert_eq!(t.decode(b"abc".to_vec()).unwrap(), b"abc");
        assert!(matches!(t.encode(b"abc").unwrap(), Cow::Borrowed(b"abc")));
        for t in [
            TransferEncoding::Zlib {
                png_predictor_columns: None,
            },
            TransferEncoding::Deflate,
            TransferEncoding::Gzip,
            TransferEncoding::Brotli,
            TransferEncoding::unknown("x-lzma").unwrap(),
        ] {
            assert!(matches!(
                t.encode(b"abc"),
                Err(Error::UnsupportedFeature(_))
            ));
        }
    }

    #[test]
    fn names_and_unknown_labels() {
        let names: Vec<_> = [
            TransferEncoding::Identity,
            TransferEncoding::Base64,
            TransferEncoding::Zlib {
                png_predictor_columns: Some(3),
            },
            TransferEncoding::Deflate,
            TransferEncoding::Gzip,
            TransferEncoding::Brotli,
            TransferEncoding::unknown("x-lzma").unwrap(),
        ]
        .iter()
        .map(|t| t.as_str())
        .collect();
        assert_eq!(
            names,
            ["identity", "base64", "zlib", "deflate", "gzip", "brotli", "unknown"]
        );
        let u = TransferEncoding::unknown("x-lzma").unwrap();
        assert_eq!(u.unknown_name(), Some("x-lzma"));
        assert_eq!(u.to_string(), "unknown (x-lzma)");
        assert!(matches!(
            u.decode(vec![1]),
            Err(Error::UnsupportedFeature(_))
        ));
        assert!(TransferEncoding::unknown("").is_none());
        assert!(TransferEncoding::unknown("a\u{1}b").is_none());
        assert!(TransferEncoding::unknown(&"a".repeat(256)).is_none());
        assert!(TransferEncoding::unknown(&"a".repeat(255)).is_some());
    }

    #[test]
    fn zlib_headers_are_recognised() {
        assert!(is_zlib(&[0x78, 0x9c]));
        assert!(is_zlib(&[0x78, 0x01]));
        assert!(!is_zlib(&[0x78, 0x9d]), "bad check bits");
        assert!(!is_zlib(&[0x88, 0x1d]), "window too large");
        assert!(!is_zlib(&[0x79, 0x9c]), "not deflate");
        assert!(!is_zlib(&[0x78]));
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn brotli_decodes_and_is_bounded() {
        let data = b"<x:xmpmeta/>".repeat(10);
        let stored = brotli_stored(&data);
        assert_eq!(
            TransferEncoding::Brotli.decode(stored.clone()).unwrap(),
            data
        );
        assert!(unbrotli(&stored, data.len() - 1).is_err());
        assert!(TransferEncoding::Brotli.decode(vec![0xff; 8]).is_err());
    }

    #[cfg(not(feature = "brotli"))]
    #[test]
    fn brotli_needs_the_feature() {
        assert!(matches!(
            TransferEncoding::Brotli.decode(brotli_stored(b"abc")),
            Err(Error::UnsupportedFeature(m)) if m.contains("Brotli")
        ));
    }

    #[test]
    fn a_png_predictor_is_undone() {
        // Two rows of three samples: "Sub" then "Up".
        let encoded = vec![1, 10, 1, 1, 2, 5, 5, 5];
        assert_eq!(
            undo_png_predictor(encoded, 3).unwrap(),
            [10, 11, 12, 15, 16, 17]
        );
        assert!(undo_png_predictor(vec![0; 5], 3).is_err(), "partial row");
        assert!(
            undo_png_predictor(vec![9, 0, 0, 0], 3).is_err(),
            "bad filter"
        );
        assert!(undo_png_predictor(vec![0], 0).is_err(), "no columns");
    }

    #[cfg(not(feature = "flate"))]
    #[test]
    fn flate_needs_the_feature() {
        for (t, name) in [
            (
                TransferEncoding::Zlib {
                    png_predictor_columns: None,
                },
                "zlib",
            ),
            (TransferEncoding::Deflate, "deflate"),
            (TransferEncoding::Gzip, "gzip"),
        ] {
            assert!(matches!(
                t.decode(vec![0x78, 0x9c]),
                Err(Error::UnsupportedFeature(m)) if m.contains(name)
            ));
        }
    }

    #[cfg(feature = "flate")]
    mod flate {
        use super::*;
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        fn deflate(data: &[u8]) -> Vec<u8> {
            let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }

        #[test]
        fn inflates_and_enforces_the_limit_and_the_end() {
            let data = vec![7u8; 10_000];
            let z = deflate(&data);
            assert_eq!(inflate(&z, 10_000, None).unwrap(), data);
            assert!(inflate(&z, 9_999, None).is_err(), "over the limit");
            let mut trailing = z.clone();
            trailing.push(0);
            assert!(inflate(&trailing, 10_000, None).is_err());
            assert!(
                inflate(&z[..z.len() - 2], 10_000, None).is_err(),
                "cut short"
            );
            let mut bad = z.clone();
            let last = bad.len() - 1;
            bad[last] ^= 1;
            assert!(inflate(&bad, 10_000, None).is_err(), "checksum");
        }

        #[test]
        fn decodes_with_and_without_a_predictor() {
            let plain = TransferEncoding::Zlib {
                png_predictor_columns: None,
            };
            assert_eq!(plain.decode(deflate(b"hello")).unwrap(), b"hello");
            let predicted = TransferEncoding::Zlib {
                png_predictor_columns: Some(3),
            };
            let rows = deflate(&[1, 10, 1, 1, 2, 5, 5, 5]);
            assert_eq!(predicted.decode(rows).unwrap(), [10, 11, 12, 15, 16, 17]);
            assert!(plain.decode(b"not zlib".to_vec()).is_err());
        }

        #[test]
        fn deflate_accepts_raw_and_zlib_wrapped_streams_and_gzip_decodes() {
            use flate2::write::{DeflateEncoder, GzEncoder};
            let data = b"<x:xmpmeta>compressed</x:xmpmeta>".repeat(20);
            let mut raw = DeflateEncoder::new(Vec::new(), Compression::default());
            raw.write_all(&data).unwrap();
            let raw = raw.finish().unwrap();
            assert!(!is_zlib(&raw));
            for stored in [raw, deflate(&data)] {
                assert_eq!(TransferEncoding::Deflate.decode(stored).unwrap(), data);
            }
            let mut gz = GzEncoder::new(Vec::new(), Compression::default());
            gz.write_all(&data).unwrap();
            let gz = gz.finish().unwrap();
            assert_eq!(TransferEncoding::Gzip.decode(gz.clone()).unwrap(), data);
            assert!(gunzip(&gz, data.len() - 1).is_err(), "over the limit");
            assert!(TransferEncoding::Gzip.decode(b"not gzip".to_vec()).is_err());
        }

        #[test]
        fn a_prefix_decodes_only_what_it_needs() {
            let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 251) as u8).collect();
            let z = deflate(&data);
            let plain = TransferEncoding::Zlib {
                png_predictor_columns: None,
            };
            // A cut-short stream still gives its head.
            assert_eq!(
                plain.decode_prefix(&z[..z.len() / 2], 32).unwrap(),
                data[..32]
            );
            assert_eq!(
                TransferEncoding::Deflate.decode_prefix(&z, 5).unwrap(),
                data[..5]
            );
            let predicted = TransferEncoding::Zlib {
                png_predictor_columns: Some(3),
            };
            let rows = deflate(&[1, 10, 1, 1, 2, 5, 5, 5]);
            assert_eq!(predicted.decode_prefix(&rows, 4).unwrap(), [10, 11, 12, 15]);
            assert_eq!(plain.decode_prefix(&deflate(b"ab"), 32).unwrap(), b"ab");
            assert!(plain.decode_prefix(b"not zlib", 32).is_err());
        }
    }

    #[test]
    fn a_base64_prefix_decodes_only_its_head() {
        let data: Vec<u8> = (0..100u8).collect();
        let mut text = base64_encode(&data);
        text.insert(10, b'\n');
        let tail = text.len() - 3;
        text[tail] = b'!'; // damage beyond the head is not read
        assert_eq!(
            TransferEncoding::Base64.decode_prefix(&text, 32).unwrap(),
            data[..32]
        );
        assert_eq!(
            TransferEncoding::Identity.decode_prefix(&data, 4).unwrap(),
            data[..4]
        );
        assert!(TransferEncoding::Base64.decode_prefix(b"!!!!", 3).is_err());
        assert!(matches!(
            TransferEncoding::unknown("x-custom")
                .unwrap()
                .decode_prefix(b"abc", 3),
            Err(Error::UnsupportedFeature(_))
        ));
    }
}
