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

//! Hierarchical view of a container.

use crate::range::ByteRange;
use crate::segment::Name;

/// One element met while [walking](crate::Structure::walk) a container's tree.
///
/// Flat containers report one node per segment at depth 1. Containers that nest
/// elements (ISO BMFF boxes) also report the nested ones, which are not segments:
/// segments tile the file once, nodes describe its hierarchy.
#[derive(Debug, Clone, Copy)]
pub struct Node<'a> {
    /// Slash-separated names from the root, for example `/moov/trak/mdia`. Valid
    /// only during the callback.
    pub path: &'a str,
    /// The element's own name.
    pub name: Name,
    /// Whole element, header through end.
    pub extent: ByteRange,
    /// Bytes of framing before the content: size, type, extended size and
    /// extended type. A FullBox's version and flags are reported separately.
    pub header_len: u32,
    /// 1 for top-level elements.
    pub depth: u32,
    /// FullBox version, if the element has one.
    pub version: Option<u8>,
    /// FullBox flags, if the element has them.
    pub flags: Option<u32>,
    /// Extended type of a `uuid` box.
    pub user_type: Option<[u8; 16]>,
}
