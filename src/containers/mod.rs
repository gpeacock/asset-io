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
