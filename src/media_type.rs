//! Media type definitions
//!
//! This module defines specific media types that can be stored in various container formats.

use crate::Container;

/// Specific media type - what the content represents
///
/// While a `Container` defines how a file is structured (JFIF, PNG, etc.),
/// a `MediaType` defines what the content actually is (JPEG image, PNG image, etc.).
///
/// Some containers like BMFF can hold many different media types (HEIC, AVIF, MP4, MOV),
/// while others like JFIF typically hold just one (JPEG).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    // JFIF container variants
    #[cfg(feature = "jpeg")]
    /// Standard JPEG image
    Jpeg,

    // PNG container (single variant)
    #[cfg(feature = "png")]
    /// PNG image
    Png,
    // Future media types (commented out until container handlers implemented)
    // BMFF container variants (ISO Base Media File Format)
    // Heic,      // HEIC image (HEVC/H.265 codec)
    // Heif,      // HEIF image
    // Avif,      // AVIF image (AV1 codec)
    // Mp4Video,  // MP4 video
    // Mp4Audio,  // M4A audio
    // QuickTime, // QuickTime MOV video
    //
    // RIFF container variants
    // WebP,
    // Wav,
    // Avi,
    //
    // TIFF container
    // Tiff,
    // Dng,
    //
    // Other formats
    // Gif,
    // Svg,
    // Pdf,
    // Mp3,
}

impl MediaType {
    /// Get all media types that are available in this build
    ///
    /// Returns a slice of all `MediaType` variants that were compiled in
    /// based on the enabled features.
    ///
    /// # Example
    ///
    /// ```
    /// use asset_io::MediaType;
    ///
    /// let supported = MediaType::all();
    /// println!("This build supports {} media types", supported.len());
    /// for media_type in supported {
    ///     println!("  - {} ({})", media_type.to_mime(), media_type.to_extension());
    /// }
    /// ```
    pub fn all() -> &'static [MediaType] {
        &[
            #[cfg(feature = "jpeg")]
            MediaType::Jpeg,
            #[cfg(feature = "png")]
            MediaType::Png,
        ]
    }

    /// Get the container format for this media type
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "jpeg")]
    /// # {
    /// use asset_io::MediaType;
    ///
    /// let media = MediaType::Jpeg;
    /// assert_eq!(media.container(), asset_io::Container::Jfif);
    /// # }
    /// ```
    pub fn container(&self) -> Container {
        match self {
            #[cfg(feature = "jpeg")]
            MediaType::Jpeg => Container::Jfif,
            #[cfg(feature = "png")]
            MediaType::Png => Container::Png,
        }
    }

    /// Get the primary MIME type for this media type
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "jpeg")]
    /// # {
    /// use asset_io::MediaType;
    ///
    /// let media = MediaType::Jpeg;
    /// assert_eq!(media.to_mime(), "image/jpeg");
    /// # }
    /// ```
    pub fn to_mime(&self) -> &'static str {
        match self {
            #[cfg(feature = "jpeg")]
            MediaType::Jpeg => "image/jpeg",
            #[cfg(feature = "png")]
            MediaType::Png => "image/png",
        }
    }

    /// Get the primary file extension for this media type (without dot)
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "jpeg")]
    /// # {
    /// use asset_io::MediaType;
    ///
    /// let media = MediaType::Jpeg;
    /// assert_eq!(media.to_extension(), "jpg");
    /// # }
    /// ```
    pub fn to_extension(&self) -> &'static str {
        match self {
            #[cfg(feature = "jpeg")]
            MediaType::Jpeg => "jpg",
            #[cfg(feature = "png")]
            MediaType::Png => "png",
        }
    }
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_mime())
    }
}
