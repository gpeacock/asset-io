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
//! Each module exposes `handler()` for registration plus direct `parse` and
//! `write` functions. Most callers go through
//! [`HandlerRegistry`](crate::HandlerRegistry) instead.

#[cfg(feature = "bmff")]
pub mod bmff;
#[cfg(feature = "bmff")]
mod items;
#[cfg(feature = "jpeg")]
pub mod jpeg;
pub mod jumbf;
#[cfg(feature = "png")]
pub mod png;
#[cfg(feature = "riff")]
pub mod riff;
