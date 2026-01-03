//! High-performance, media type-agnostic streaming I/O for media asset metadata.
//!
//! This crate provides efficient, single-pass parsing and writing of media files
//! with JUMBF (JPEG Universal Metadata Box Format) and XMP metadata.
//!
//! # Design Principles
//!
//! - **Streaming**: Process files without loading them entirely into memory
//! - **Lazy loading**: Only read data when explicitly accessed
//! - **Zero-copy**: Use memory-mapped files when beneficial
//! - **Media type agnostic**: Unified API across JPEG, PNG, MP4, and more
//!
//! # Quick Start (Media Type-Agnostic API)
//!
//! The simplest way to use this library is with the [`Asset`] API,
//! which automatically detects the media type:
//!
//! ```no_run
//! use asset_io::{Asset, Updates};
//!
//! # fn main() -> asset_io::Result<()> {
//! // Open any supported file - media type is auto-detected
//! let mut asset = Asset::open("image.jpg")?;
//!
//! // Read metadata
//! if let Some(xmp) = asset.xmp()? {
//!     println!("XMP: {} bytes", xmp.len());
//! }
//! if let Some(jumbf) = asset.jumbf()? {
//!     println!("JUMBF: {} bytes", jumbf.len());
//! }
//!
//! // Modify and write using builder pattern
//! let updates = Updates::new()
//!     .set_xmp(b"<new>metadata</new>".to_vec())
//!     .remove_jumbf();
//! asset.write_to("output.jpg", &updates)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Handler-Specific API
//!
//! For more control, you can use media type-specific handlers:
//!
//! ```no_run
//! use asset_io::{ContainerIO, JpegIO, Updates};
//! use std::fs::File;
//!
//! # fn main() -> asset_io::Result<()> {
//! // Parse file structure in single pass
//! let mut file = File::open("image.jpg")?;
//! let handler = JpegIO::new();
//! let structure = handler.parse(&mut file)?;
//!
//! // Access XMP data (loaded lazily via handler)
//! if let Some(xmp) = handler.read_xmp(&structure, &mut file)? {
//!     println!("Found XMP: {} bytes", xmp.len());
//! }
//!
//! // Write with updates
//! let updates = Updates::default();
//! let mut output = File::create("output.jpg")?;
//! handler.write(&structure, &mut file, &mut output, &updates)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Processing During Read/Write
//!
//! For C2PA workflows and similar use cases, you can process data (e.g., hash it)
//! while reading or writing:
//!
//! ```no_run
//! use asset_io::{Asset, Updates, SegmentKind, ExclusionMode};
//! use sha2::{Sha256, Digest};
//!
//! # fn main() -> asset_io::Result<()> {
//! let mut asset = Asset::open("signed.jpg")?;
//!
//! // Hash while reading, excluding specific segments
//! let updates = Updates::new()
//!     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
//!
//! let mut hasher = Sha256::new();
//! asset.read_with_processing(&updates, &mut |chunk| hasher.update(chunk))?;
//! let hash = hasher.finalize();
//! println!("Asset hash: {:x}", hash);
//!
//! // Or hash while writing (single-pass workflow)
//! let mut output = std::fs::File::create("output.jpg")?;
//! let updates = Updates::new()
//!     .set_jumbf(b"placeholder".to_vec())
//!     .exclude_from_processing(vec![SegmentKind::Jumbf], ExclusionMode::DataOnly);
//!
//! let mut hasher = Sha256::new();
//! let dest_structure = asset.write_with_processing(
//!     &mut output,
//!     &updates,
//!     &mut |chunk| hasher.update(chunk),
//! )?;
//! // Now you have the hash and can update the JUMBF in-place
//! # Ok(())
//! # }
//! ```

mod asset;
mod containers;
mod error;
mod media_type;
mod processing_writer;
mod segment;
mod structure;
mod thumbnail;
mod updates;
#[cfg(feature = "exif")]
mod tiff;
#[cfg(feature = "xmp")]
mod xmp;

// Public exports
pub use asset::{Asset, AssetBuilder};
pub use containers::ContainerKind;
pub use error::{Error, Result};
pub use segment::{ByteRange, ExclusionMode, Segment, SegmentKind};
pub use structure::Structure;
pub use thumbnail::{Thumbnail, ThumbnailKind};
pub use updates::Updates;
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;

// Internal re-exports
pub(crate) use containers::{detect_container, get_handler, Handler};
pub(crate) use media_type::MediaType;
pub(crate) use segment::{ChunkedSegmentReader, SegmentMetadata};
pub(crate) use updates::MetadataUpdate;

// Test utilities - only compiled for tests or when explicitly enabled
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
