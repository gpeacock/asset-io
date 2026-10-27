// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

//! Movie-fragment geometry, independent of track and sample interpretation.

use super::ID;
use crate::{ByteRange, Error, Name, Result, Structure};
use std::ops::Range;

/// One movie fragment: a `moof` and the top-level boxes following it, up to the
/// next `moof` or the end of the stream. This includes auxiliary metadata and,
/// for the final fragment, any random-access index or trailing bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    range: ByteRange,
    moof: ByteRange,
    segments: Range<usize>,
}

impl Fragment {
    /// Absolute range in this stream, not in an initialization or companion file.
    pub fn range(&self) -> ByteRange {
        self.range
    }

    /// The complete movie-fragment header box.
    pub fn moof(&self) -> ByteRange {
        self.moof
    }

    /// Indices of the top-level segments covered by this fragment.
    pub fn segments(&self) -> Range<usize> {
        self.segments.clone()
    }
}

/// The prefix and movie fragments of one BMFF stream.
///
/// Separate DASH initialization and media streams are parsed independently.
/// Offsets are always local to the stream; this type does not join streams,
/// resolve URLs, or interpret samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentLayout {
    prefix: Option<ByteRange>,
    fragments: Vec<Fragment>,
}

impl FragmentLayout {
    /// Bytes before the first `moof`, or the whole stream when there is no `moof`.
    ///
    /// In a single-file fragmented movie this includes initialization boxes. In
    /// a DASH media segment it can instead contain `styp`, `sidx`, or metadata;
    /// the presence of a prefix does not imply an initialization segment.
    pub fn prefix(&self) -> Option<ByteRange> {
        self.prefix
    }

    /// Movie fragments in file order. An initialization-only stream has none.
    pub fn fragments(&self) -> &[Fragment] {
        &self.fragments
    }
}

/// Locate movie fragments using only a structure's top-level box geometry.
///
/// Works on parsed, planned, and written structures without additional I/O.
/// Boundaries follow `moof` boxes, not media-segment files: a DASH media segment
/// can hold several fragments. Hashing an external segment may require its whole
/// stream, including the prefix, rather than just these fragment ranges.
pub fn fragment_layout(structure: &Structure) -> Result<FragmentLayout> {
    if structure.container_id() != ID {
        return Err(Error::UnsupportedFormat);
    }
    let segments = structure.segments();
    let mut fragments: Vec<Fragment> = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        if segment.name() != Name::FourCC(*b"moof") {
            continue;
        }
        let start = segment.range().offset();
        if let Some(previous) = fragments.last_mut() {
            previous.range =
                ByteRange::new(previous.range.offset(), start - previous.range.offset())?;
            previous.segments.end = i;
        }
        fragments.push(Fragment {
            range: ByteRange::new(start, structure.total_size() - start)?,
            moof: segment.range(),
            segments: i..segments.len(),
        });
    }
    let prefix_end = fragments
        .first()
        .map_or(structure.total_size(), |f| f.range.offset());
    Ok(FragmentLayout {
        prefix: if prefix_end == 0 {
            None
        } else {
            Some(ByteRange::new(0, prefix_end)?)
        },
        fragments,
    })
}
