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

//! Stream filters: FlateDecode with the PNG predictors, the only encoding supported.

use super::lexer::{invalid, Dictionary, Value};
use crate::error::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};

/// How a stream is encoded: Flate, then optionally a PNG predictor over rows of
/// `columns` single-byte samples. A stream with no filter has no `Decode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Decode {
    pub(super) predictor: u8,
    pub(super) columns: u32,
}

impl Decode {
    /// What the stream dictionary says about the encoding, consuming those keys.
    pub(super) fn from_dictionary(dict: &mut Dictionary) -> Result<Option<Self>> {
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
        let mut take = |key: &[u8], default: u64| -> Result<u64> {
            Ok(parameters
                .remove(key)
                .map(Value::integer)
                .transpose()?
                .unwrap_or(default))
        };
        let predictor = take(b"Predictor", 1)?;
        let columns = take(b"Columns", 1)?;
        let colors = take(b"Colors", 1)?;
        let bits = take(b"BitsPerComponent", 8)?;
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

    /// Undo the PNG predictor of decoded data, if there is one.
    pub(super) fn undo_predictor(self, mut data: Vec<u8>) -> Result<Vec<u8>> {
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
                    _ => unreachable!("filter was checked above"),
                };
                data[target + column] = data[target + column].wrapping_add(prediction);
            }
        }
        data.truncate(rows * columns);
        Ok(data)
    }
}

/// Inflate zlib data, which must be all of `input`, to at most `limit` bytes.
///
/// `sync_flush_size`: some producers end a cross-reference stream with a sync flush
/// instead of a finished zlib stream. That is accepted, only for data that ends in the
/// flush marker and decodes to exactly this size.
pub(super) fn inflate(
    input: &[u8],
    limit: usize,
    sync_flush_size: Option<usize>,
) -> Result<Vec<u8>> {
    let mut decoder = Decompress::new(true);
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut used = 0usize;
    loop {
        let (before_in, before_out) = (decoder.total_in(), decoder.total_out());
        let status = decoder
            .decompress(&input[used..], &mut buffer, FlushDecompress::None)
            .map_err(|_| invalid("invalid Flate stream or checksum"))?;
        let consumed = (decoder.total_in() - before_in) as usize;
        let produced = (decoder.total_out() - before_out) as usize;
        used += consumed;
        if output.len().checked_add(produced).is_none_or(|n| n > limit) {
            return Err(invalid("decoded stream size limit exceeded"));
        }
        output.extend_from_slice(&buffer[..produced]);
        if status == Status::StreamEnd {
            if used != input.len() {
                return Err(invalid("trailing data in Flate stream"));
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
            return Err(invalid(if used == input.len() {
                "incomplete Flate stream"
            } else {
                "Flate decoder made no progress"
            }));
        }
    }
}

#[cfg(test)]
mod tests {
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
    fn a_png_predictor_is_undone_and_others_are_refused() {
        // Two rows of three samples: "Sub" then "Up".
        let encoded = vec![1, 10, 1, 1, 2, 5, 5, 5];
        let d = Decode {
            predictor: 12,
            columns: 3,
        };
        assert_eq!(d.undo_predictor(encoded).unwrap(), [10, 11, 12, 15, 16, 17]);
        assert!(d.undo_predictor(vec![0; 5]).is_err(), "partial row");
        assert!(d.undo_predictor(vec![9, 0, 0, 0]).is_err(), "bad filter");

        let mut dict = Dictionary::new();
        dict.insert(b"Filter".to_vec(), Value::Name(b"LZWDecode".to_vec()));
        assert!(matches!(
            Decode::from_dictionary(&mut dict),
            Err(Error::UnsupportedFeature(_))
        ));
        let mut dict = Dictionary::new();
        assert_eq!(Decode::from_dictionary(&mut dict).unwrap(), None);
    }
}
