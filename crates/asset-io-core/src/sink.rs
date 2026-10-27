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

//! Streaming observation: a [`Sink`] sees every byte of an asset, in order, as it
//! is written ([`Structure::write`]) or read ([`Structure::read`]).

use crate::error::Result;
use crate::segment::Role;
use crate::structure::{SegmentId, Structure};

/// A run of bytes, borrowed from the buffer being read or written. No copy is made.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Chunk<'a> {
    /// Segment these bytes belong to, in the structure given to [`Sink::begin`].
    pub segment: SegmentId,
    /// Header, payload or trailer.
    pub role: Role,
    /// Offset of `data[0]` in the asset.
    pub offset: u64,
    /// The bytes.
    pub data: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// A chunk of `data` at `offset` in the asset, belonging to `role` of `segment`.
    pub const fn new(segment: SegmentId, role: Role, offset: u64, data: &'a [u8]) -> Self {
        Self {
            segment,
            role,
            offset,
            data,
        }
    }
}

/// Receives bytes during a read or a write.
///
/// Every byte the sink wants is delivered exactly once, in order, tagged with
/// its segment and role. The sink decides what the bytes mean; asset-io attaches
/// no hashing semantics to them. Return [`Error::UserCanceled`](crate::Error::UserCanceled) from `chunk` to
/// stop the operation.
pub trait Sink {
    /// Called once before any bytes, with the complete structure of the asset being
    /// read or of the output being written.
    fn begin(&mut self, _structure: &Structure) -> Result<()> {
        Ok(())
    }

    /// Return `false` to skip delivery of a run. A read does not fetch skipped
    /// runs; a write still writes them.
    fn wants(&self, _segment: &crate::segment::Segment, _role: Role) -> bool {
        true
    }

    /// Receive a run of output bytes.
    fn chunk(&mut self, chunk: Chunk<'_>) -> Result<()>;
}

/// A sink that ignores everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl Sink for NullSink {
    fn wants(&self, _: &crate::segment::Segment, _: Role) -> bool {
        false
    }
    fn chunk(&mut self, _: Chunk<'_>) -> Result<()> {
        Ok(())
    }
}

impl<F> Sink for F
where
    F: for<'a> FnMut(Chunk<'a>) -> Result<()>,
{
    fn chunk(&mut self, chunk: Chunk<'_>) -> Result<()> {
        self(chunk)
    }
}
