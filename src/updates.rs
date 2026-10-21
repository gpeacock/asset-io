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

//! Metadata updates to apply when writing an asset.

/// Metadata update strategy (internal)
///
/// Specifies how to handle a particular type of metadata when writing an asset.
/// By default, all metadata is kept unchanged.
#[derive(Debug, Clone, Default)]
pub(crate) enum MetadataUpdate {
    /// Keep existing metadata (default)
    #[default]
    Keep,
    /// Remove existing metadata
    Remove,
    /// Replace or add metadata
    Set(Vec<u8>),
}

/// What to do with merkle boxes (internal)
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // `Set` is only built when the `bmff` feature is on
pub(crate) enum MerkleUpdate {
    /// Keep existing boxes (default)
    #[default]
    Keep,
    /// Remove existing boxes
    Remove,
    /// Replace existing boxes with these payloads, one box each
    Set(Vec<Vec<u8>>),
}

/// Updates to apply when writing a file
///
/// This struct uses a builder pattern where the default is to keep all existing
/// metadata unchanged. Use the builder methods to explicitly specify changes.
///
/// # Example
///
/// ```
/// use asset_io::Updates;
///
/// // Default: keep everything
/// let keep = Updates::new();
///
/// // Remove XMP, keep everything else
/// let no_xmp = Updates::new().remove_xmp();
///
/// // Set new JUMBF, remove XMP, keep everything else
/// let updates = Updates::new()
///     .set_jumbf(b"new jumbf data".to_vec())
///     .remove_xmp();
/// ```
#[derive(Debug, Default)]
pub struct Updates {
    /// XMP data update strategy (use builder methods to modify)
    pub(crate) xmp: MetadataUpdate,

    /// JUMBF data update strategy (use builder methods to modify)
    pub(crate) jumbf: MetadataUpdate,

    /// The JUMBF being set adds to the existing store
    /// (see [`Updates::as_update`]).
    pub(crate) jumbf_is_update: bool,

    /// The JUMBF type the JUMBF update applies to; `None` means any JUMBF
    /// (see [`Updates::set_jumbf`]).
    pub(crate) jumbf_scope: Option<[u8; 16]>,

    /// Merkle box update strategy (see [`Updates::set_merkle`])
    pub(crate) merkle: MerkleUpdate,
}

impl Updates {
    /// Create a new `Updates` builder with all metadata set to keep (no changes)
    ///
    /// This is the same as `Updates::default()` but more explicit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set XMP metadata to a new value
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new()
    ///     .set_xmp(b"<xmp>...</xmp>".to_vec());
    /// ```
    pub fn set_xmp(mut self, xmp: Vec<u8>) -> Self {
        self.xmp = MetadataUpdate::Set(xmp);
        self
    }

    /// Remove XMP metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().remove_xmp();
    /// ```
    pub fn remove_xmp(mut self) -> Self {
        self.xmp = MetadataUpdate::Remove;
        self
    }

    /// Keep existing XMP metadata (explicit, same as default)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().keep_xmp();
    /// ```
    pub fn keep_xmp(mut self) -> Self {
        self.xmp = MetadataUpdate::Keep;
        self
    }

    /// Set JUMBF metadata to a new value.
    ///
    /// The JUMBF replaces the existing JUMBF of the same type: the type in the
    /// description box of `jumbf`, which this crate compares but does not interpret.
    /// JUMBF of other types is left as it is, and if there is none of the same type
    /// the new JUMBF is added. If `jumbf` has no readable type, it replaces all JUMBF.
    ///
    /// This applies to containers that carry JUMBF of any type, such as JPEG.
    /// Containers with an element reserved for one store (the PNG `caBX` chunk, the
    /// ISO BMFF C2PA box) replace that element.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new()
    ///     .set_jumbf(b"jumbf data".to_vec());
    /// ```
    pub fn set_jumbf(mut self, jumbf: Vec<u8>) -> Self {
        self.jumbf_scope = crate::containers::jumbf::superbox_type(&jumbf);
        self.jumbf = MetadataUpdate::Set(jumbf);
        self.jumbf_is_update = false;
        self
    }

    /// Whether the JUMBF update applies to existing JUMBF of type `ty`, as a
    /// container that carries JUMBF of any type reports it. A type that is not
    /// known is never touched by an update limited to one type.
    #[allow(dead_code)] // only containers that carry JUMBF of any type call it
    pub(crate) fn jumbf_targets(&self, ty: Option<[u8; 16]>) -> bool {
        match self.jumbf_scope {
            None => true,
            Some(scope) => ty == Some(scope),
        }
    }

    /// Mark the JUMBF given to [`set_jumbf`](Self::set_jumbf) as an update
    /// manifest added to the existing store: `set_jumbf(store).as_update()`. Call it
    /// after `set_jumbf`; a later `set_jumbf`, `remove_jumbf` or `keep_jumbf`
    /// clears it.
    ///
    /// Most containers ignore this. A container that keeps the original store apart
    /// from later additions (ISO BMFF) uses it: if the new store begins with the
    /// existing store's manifests, byte for byte, it leaves them in place and
    /// stores only the rest as an update. Otherwise (nothing to update, or the
    /// existing manifests changed) it writes the new store as a whole. The hint is
    /// needed because a standard manifest added to a signed file extends the store
    /// in the same way, and must not be treated as an update.
    pub fn as_update(mut self) -> Self {
        self.jumbf_is_update = true;
        self
    }

    /// Remove all JUMBF metadata, of every type. To remove one type and keep the
    /// rest, use [`remove_jumbf_of`](Self::remove_jumbf_of).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().remove_jumbf();
    /// ```
    pub fn remove_jumbf(mut self) -> Self {
        self.jumbf = MetadataUpdate::Remove;
        self.jumbf_scope = None;
        self.jumbf_is_update = false;
        self
    }

    /// Remove the JUMBF of one type (see [`Entity::jumbf_type`](crate::Entity::jumbf_type))
    /// and keep any other. Containers with an element reserved for one store
    /// remove that element.
    pub fn remove_jumbf_of(mut self, jumbf_type: [u8; 16]) -> Self {
        self.jumbf = MetadataUpdate::Remove;
        self.jumbf_scope = Some(jumbf_type);
        self.jumbf_is_update = false;
        self
    }

    /// Keep existing JUMBF metadata (explicit, same as default)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().keep_jumbf();
    /// ```
    pub fn keep_jumbf(mut self) -> Self {
        self.jumbf = MetadataUpdate::Keep;
        self.jumbf_scope = None;
        self.jumbf_is_update = false;
        self
    }

    /// Replace the file's merkle boxes with one box per payload.
    ///
    /// Merkle boxes carry proofs for hashed media blocks. Only ISO BMFF has them;
    /// other containers ignore this. The boxes are written immediately after the
    /// last `mdat`, and any existing merkle boxes are dropped. All payloads are
    /// usually the same length (the C2PA specification pads them to one length).
    ///
    /// The content usually depends on hashes of the media, which exist only after
    /// the write. Write zeroed payloads of the final length, hash the media through
    /// the [`Sink`](crate::Sink) during the write, then fill each box in place with
    /// [`Structure::patch_exact`](crate::Structure::patch_exact). The boxes are the
    /// entities labelled [`bmff::MERKLE`](crate::containers::bmff::MERKLE) of kind
    /// [`SegmentKind::Other`](crate::SegmentKind::Other).
    ///
    /// A file that has merkle boxes cannot take a new manifest unless the boxes are
    /// also set or removed, and an update manifest cannot change them.
    #[cfg(feature = "bmff")]
    pub fn set_merkle(mut self, payloads: Vec<Vec<u8>>) -> Self {
        self.merkle = MerkleUpdate::Set(payloads);
        self
    }

    /// Remove the file's merkle boxes. See [`set_merkle`](Self::set_merkle).
    pub fn remove_merkle(mut self) -> Self {
        self.merkle = MerkleUpdate::Remove;
        self
    }

    /// Keep the file's merkle boxes (explicit, same as default).
    pub fn keep_merkle(mut self) -> Self {
        self.merkle = MerkleUpdate::Keep;
        self
    }

    /// Create updates that keep all existing metadata (no changes)
    ///
    /// This is an alias for `Updates::new()` or `Updates::default()`.
    pub fn keep_all() -> Self {
        Self::default()
    }

    /// Create updates that remove all metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::remove_all();
    /// ```
    pub fn remove_all() -> Self {
        Self::new().remove_xmp().remove_jumbf().remove_merkle()
    }

    /// Create updates that set new XMP (legacy constructor)
    ///
    /// Prefer using `Updates::new().set_xmp(xmp)` for consistency with
    /// the builder pattern.
    pub fn with_xmp(xmp: Vec<u8>) -> Self {
        Self::new().set_xmp(xmp)
    }

    /// Create updates that set new JUMBF (legacy constructor)
    ///
    /// Prefer using `Updates::new().set_jumbf(jumbf)` for consistency with
    /// the builder pattern.
    pub fn with_jumbf(jumbf: Vec<u8>) -> Self {
        Self::new().set_jumbf(jumbf)
    }
}
