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

//! Memory-mapped files, for zero-copy reads.

use crate::error::Result;
use crate::registry::HandlerRegistry;
use crate::structure::Structure;
use std::fs::File;
use std::io::Cursor;
use std::ops::Deref;

/// A read-only memory map of a file.
///
/// Pair it with [`Structure::slice`], [`Structure::slice_item`] and
/// [`Structure::read_slice`] to read without copying.
///
/// ```no_run
/// # use asset_io::{HandlerRegistry, ItemKind, MappedFile};
/// # fn main() -> asset_io::Result<()> {
/// let file = std::fs::File::open("image.jpg")?;
/// // SAFETY: the file is not modified or truncated while mapped.
/// let map = unsafe { MappedFile::map(&file)? };
/// let structure = map.parse_detect(&HandlerRegistry::builtin())?;
/// if let Some(xmp) = structure.item(ItemKind::Xmp)? {
///     let bytes = structure.slice_item(&map, xmp)?; // borrowed from the map
///     println!("{} bytes of XMP", bytes.len());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct MappedFile {
    map: memmap2::Mmap,
}

impl MappedFile {
    /// Map `file` read-only.
    ///
    /// # Safety
    ///
    /// The file must not be truncated or modified, by this process or another,
    /// while the map is alive. Doing so is undefined behavior, and truncation
    /// can raise `SIGBUS`. Do not map files that untrusted parties can write.
    /// Use ordinary reads (`read`) for those.
    pub unsafe fn map(file: &File) -> Result<Self> {
        Ok(Self {
            map: memmap2::Mmap::map(file)?,
        })
    }

    /// The mapped bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.map
    }

    /// Parse the mapped file, identifying the container from its content.
    pub fn parse_detect(&self, registry: &HandlerRegistry) -> Result<Structure> {
        registry.parse_detect(&mut Cursor::new(self.as_bytes()))
    }

    /// Parse the mapped file as `format` (extension or MIME type).
    pub fn parse(&self, registry: &HandlerRegistry, format: &str) -> Result<Structure> {
        registry.parse(format, &mut Cursor::new(self.as_bytes()))
    }
}

impl Deref for MappedFile {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.map
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        &self.map
    }
}
