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
//! Each handler module ([`jpeg`], [`png`], [`bmff`], [`riff`], [`svg`], [`pdf`]) exposes `handler()`
//! for registration plus direct `parse` and `write` functions. Most callers go
//! through [`HandlerRegistry`] instead.
//!
//! # Isolation
//!
//! Handlers are independent of one another and depend on the core only through its
//! public API (items reachable from the crate root, plus [`plan`](crate::plan),
//! [`jumbf`](crate::jumbf) and [`testing`](crate::testing)). The core in turn knows
//! no handler except through `register_builtin`. `tests/container_isolation.rs`
//! enforces this, so a handler can later move to its own crate unchanged.
//!
//! A handler that needs helpers keeps them private in its own module directory,
//! as `bmff` does with its HEIF item reader.

use crate::HandlerRegistry;

#[cfg(feature = "bmff")]
pub mod bmff;
#[cfg(feature = "jpeg")]
pub mod jpeg;
#[cfg(feature = "pdf")]
pub mod pdf;
#[cfg(feature = "png")]
pub mod png;
#[cfg(feature = "riff")]
pub mod riff;
#[cfg(feature = "svg")]
pub mod svg;

/// Add the handler of every enabled format to `registry`.
///
/// The one place the core reaches into the handlers; [`HandlerRegistry::builtin`]
/// calls it.
#[allow(unused_variables)]
pub(crate) fn register_builtin(registry: &mut HandlerRegistry) {
    #[cfg(feature = "jpeg")]
    registry.add_arc(jpeg::handler());
    #[cfg(feature = "png")]
    registry.add_arc(png::handler());
    #[cfg(feature = "bmff")]
    registry.add_arc(bmff::handler());
    #[cfg(feature = "riff")]
    registry.add_arc(riff::handler());
    #[cfg(feature = "svg")]
    registry.add_arc(svg::handler());
    #[cfg(feature = "pdf")]
    registry.add_arc(pdf::handler());
}
