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

//! Stream filters: which FlateDecode parameters a stream dictionary gives. The
//! decoding itself is the core's (see [`TransferEncoding`]).

use super::lexer::{invalid, Dictionary, Value};
use asset_io_core::transfer::undo_png_predictor;
use asset_io_core::{Error, Result, TransferEncoding};

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

    /// The transfer encoding of an item stored this way.
    pub(super) fn transfer(self) -> TransferEncoding {
        TransferEncoding::Zlib {
            png_predictor_columns: (self.predictor != 1).then_some(self.columns),
        }
    }

    /// Undo the PNG predictor of decoded data, if there is one.
    pub(super) fn undo_predictor(self, data: Vec<u8>) -> Result<Vec<u8>> {
        if self.predictor == 1 {
            return Ok(data);
        }
        undo_png_predictor(data, self.columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_map_to_transfer_encodings_and_others_are_refused() {
        let d = Decode {
            predictor: 12,
            columns: 3,
        };
        assert_eq!(
            d.transfer(),
            TransferEncoding::Zlib {
                png_predictor_columns: Some(3)
            }
        );
        assert_eq!(d.undo_predictor(vec![1, 10, 1, 1]).unwrap(), [10, 11, 12]);
        let plain = Decode {
            predictor: 1,
            columns: 1,
        };
        assert_eq!(
            plain.transfer(),
            TransferEncoding::Zlib {
                png_predictor_columns: None
            }
        );

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
