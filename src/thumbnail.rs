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

//! Embedded thumbnails, for any container.
//!
//! [`Structure::embedded_thumbnail`] works on every container because it does not
//! look at the format. It uses the entities the container's handler reported:
//!
//! 1. A [`SegmentKind::Thumbnail`] entity: a pre-rendered preview the container
//!    stores as its own element. Today that is a HEIF `thmb` item, which the BMFF
//!    handler finds (see `containers::bmff`).
//! 2. Otherwise, with the `exif` feature, the JPEG thumbnail in the EXIF data
//!    (IFD1) of the first [`SegmentKind::Exif`] entity. JPEG, PNG, WebP and
//!    HEIF/AVIF all report EXIF, and the TIFF parsing lives in the `tiff` module.
//!
//! So handlers only say where things are; finding the thumbnail inside them is
//! done here, once.

use crate::error::Result;
use crate::segment::SegmentKind;
use crate::structure::Structure;
use std::borrow::Cow;
use std::io::{Read, Seek};

/// An embedded thumbnail with its data
///
/// This struct contains the actual thumbnail bytes extracted from an image file.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Thumbnail {
    /// The raw thumbnail data, in the format [`media_type`](Self::media_type) names
    pub data: Vec<u8>,

    media_type: Option<Cow<'static, str>>,

    /// Width in pixels (if known)
    pub width: Option<u32>,

    /// Height in pixels (if known)
    pub height: Option<u32>,
}

impl Thumbnail {
    /// A thumbnail with data of unknown media type and size.
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            media_type: None,
            width: None,
            height: None,
        }
    }

    /// Set the media type of the data, for example `image/jpeg`.
    pub fn with_media_type(mut self, media_type: impl Into<Cow<'static, str>>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    /// Set the size in pixels.
    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Media type of the data, or `None` when the container cannot tell (a coded
    /// HEIF item such as `hvc1`, which is a bitstream and not a file). Reported the
    /// same way as [`SubAsset::media_type`](crate::SubAsset::media_type).
    pub fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }
}

impl Structure {
    /// The asset's embedded thumbnail, if it has one.
    ///
    /// A thumbnail the container stores as its own element
    /// ([`SegmentKind::Thumbnail`], a HEIF `thmb` item) is preferred. Its media type
    /// is the one the container reports for it (see [`Entity::media_type`](crate::Entity::media_type)):
    /// `image/jpeg` for a `jpeg` item, the content type of a `mime` item. For a
    /// coded item (`hvc1`, `av01`, ...) it is `None`, and `data` is the coded item,
    /// which needs the
    /// codec configuration stored elsewhere in the file to decode. Failing that,
    /// with the `exif` feature, the JPEG thumbnail in the EXIF data (IFD1) is
    /// returned.
    pub fn embedded_thumbnail<R: Read + Seek>(&self, src: &mut R) -> Result<Option<Thumbnail>> {
        if let Some(e) = self.entities(SegmentKind::Thumbnail).next() {
            let mut t = Thumbnail::new(self.read_entity(src, e)?);
            if let Some(m) = e.media_type() {
                t = t.with_media_type(m.to_owned());
            }
            return Ok(Some(t));
        }
        #[cfg(feature = "exif")]
        {
            self.exif_thumbnail(src)
        }
        #[cfg(not(feature = "exif"))]
        {
            Ok(None)
        }
    }
}
