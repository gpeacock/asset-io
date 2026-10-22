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

//! Checked byte ranges.

use crate::error::{Error, Result};

/// A byte range `[offset, offset + len)` whose end is guaranteed not to overflow.
///
/// Fields are private; every constructor validates, so `end()` is always safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    offset: u64,
    len: u64,
}

impl ByteRange {
    /// Create a range, failing if `offset + len` overflows `u64`.
    pub fn new(offset: u64, len: u64) -> Result<Self> {
        if offset.checked_add(len).is_none() {
            return Err(Error::invalid_segment(
                offset,
                format!("range overflows u64 (len {len})"),
            ));
        }
        Ok(Self { offset, len })
    }

    /// Construct from values the caller has already proven cannot overflow.
    pub(crate) const fn raw(offset: u64, len: u64) -> Self {
        Self { offset, len }
    }

    /// Start offset.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Length in bytes.
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// True if the range has zero length.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exclusive end offset.
    pub const fn end(&self) -> u64 {
        self.offset + self.len
    }

    /// True if `other` lies entirely within `self`.
    pub const fn contains(&self, other: &ByteRange) -> bool {
        other.offset >= self.offset && other.end() <= self.end()
    }

    /// Sub-range at `rel_offset` from the start of `self`.
    pub fn sub(&self, rel_offset: u64, len: u64) -> Result<ByteRange> {
        let end = rel_offset.checked_add(len);
        match end {
            Some(e) if e <= self.len => Ok(ByteRange::raw(self.offset + rel_offset, len)),
            _ => Err(Error::invalid_segment(
                self.offset,
                format!(
                    "sub-range {rel_offset}+{len} exceeds range length {}",
                    self.len
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_overflow() {
        assert!(ByteRange::new(u64::MAX, 1).is_err());
        assert!(ByteRange::new(u64::MAX - 1, 1).is_ok());
    }

    #[test]
    fn sub_and_contains() {
        let r = ByteRange::new(10, 20).unwrap();
        let s = r.sub(5, 10).unwrap();
        assert_eq!((s.offset(), s.len(), s.end()), (15, 10, 25));
        assert!(r.contains(&s));
        assert!(r.sub(15, 6).is_err());
        assert!(r.sub(u64::MAX, 2).is_err());
    }
}
