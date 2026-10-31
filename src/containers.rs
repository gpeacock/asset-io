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

//! Container crates re-exported under their feature-selected module names.
//!
//! Each enabled handler module exposes `handler()`
//! for registration plus direct `parse` and `write` functions. Most callers go
//! through [`HandlerRegistry`] instead.
//!
//! # Isolation
//!
//! Each handler is a separate crate depending only on the public core API.
//! The core depends on no handlers. This facade is responsible for registration.
//!
//! A handler that needs helpers keeps them private in its own module directory,
//! as `bmff` does with its HEIF item reader.

use crate::HandlerRegistry;

#[cfg(feature = "bmff")]
pub use asset_io_bmff as bmff;
#[cfg(feature = "c2pa")]
pub use asset_io_c2pa as c2pa;
#[cfg(feature = "jpeg")]
pub use asset_io_jpeg as jpeg;
#[cfg(feature = "pdf")]
pub use asset_io_pdf as pdf;
#[cfg(feature = "png")]
pub use asset_io_png as png;
#[cfg(feature = "riff")]
pub use asset_io_riff as riff;
#[cfg(feature = "svg")]
pub use asset_io_svg as svg;
#[cfg(feature = "tiff")]
pub use asset_io_tiff as tiff;

/// Add the handler of every enabled format to `registry`.
///
/// The one place the facade assembles the handlers; [`HandlerRegistry::builtin`]
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
    #[cfg(feature = "tiff")]
    registry.add_arc(tiff::handler());
    #[cfg(feature = "c2pa")]
    registry.add_arc(c2pa::handler());
}
