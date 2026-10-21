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

//! Built-in container handlers, one module per format, each behind its feature.
//!
//! Each handler module ([`jpeg`], [`png`], [`bmff`], [`riff`]) exposes `handler()`
//! for registration plus direct `parse` and `write` functions. Most callers go
//! through [`HandlerRegistry`](crate::HandlerRegistry) instead.
//!
//! The other modules are not handlers; they hold code that more than one handler
//! uses, or that a handler splits out:
//!
//! - [`jumbf`]: JUMBF box parsing, shared by every handler that carries a manifest
//!   store.
//! - `heif_items` (private): reads HEIF items (EXIF data, thumbnails) for the BMFF
//!   handler.

#[cfg(feature = "bmff")]
pub mod bmff;
#[cfg(feature = "bmff")]
mod heif_items;
#[cfg(feature = "jpeg")]
pub mod jpeg;
pub mod jumbf;
#[cfg(feature = "png")]
pub mod png;
#[cfg(feature = "riff")]
pub mod riff;
