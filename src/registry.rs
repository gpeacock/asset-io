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

//! Registry of container handlers.
//!
//! Handlers are looked up by format string (a file extension or MIME type) or by
//! sniffing the first bytes of a stream. A registry can extend another one: a
//! caller's custom handlers are consulted first and the fallback (usually
//! [`HandlerRegistry::builtin`]) only for formats they do not claim.

use crate::error::{Error, Result};
use crate::handler::{ContainerHandler, FormatSpec, ReadSeek, DETECT_LEN};
use crate::structure::Structure;
use crate::task::Task;
use std::collections::HashMap;
use std::io::SeekFrom;
use std::sync::{Arc, OnceLock};

/// Matching is case-insensitive and ignores surrounding whitespace and a leading dot.
fn normalize(format: &str) -> String {
    format.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// Handlers keyed by format string, with an optional fallback registry.
///
/// Registering a handler for a format string that is already claimed replaces the
/// earlier claim in this registry ("last registered wins"). The fallback is never
/// modified.
#[derive(Debug, Default)]
pub struct HandlerRegistry {
    by_format: HashMap<String, Arc<dyn ContainerHandler>>,
    /// Registration order, for detection (most recent first).
    ordered: Vec<Arc<dyn ContainerHandler>>,
    fallback: Option<Arc<HandlerRegistry>>,
}

impl HandlerRegistry {
    /// An empty registry with no fallback.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty registry that defers to `fallback` for anything it does not handle.
    pub fn with_fallback(fallback: Arc<HandlerRegistry>) -> Self {
        Self {
            fallback: Some(fallback),
            ..Self::default()
        }
    }

    /// An empty registry that defers to [`builtin`](Self::builtin).
    pub fn extending_builtin() -> Self {
        Self::with_fallback(Self::builtin())
    }

    /// The handlers compiled into this crate (per enabled feature), shared.
    pub fn builtin() -> Arc<HandlerRegistry> {
        static BUILTIN: OnceLock<Arc<HandlerRegistry>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                #[allow(unused_mut)]
                let mut r = HandlerRegistry::new();
                #[cfg(feature = "jpeg")]
                r.add_arc(crate::containers::jpeg::handler());
                #[cfg(feature = "png")]
                r.add_arc(crate::containers::png::handler());
                #[cfg(feature = "bmff")]
                r.add_arc(crate::containers::bmff::handler());
                #[cfg(feature = "riff")]
                r.add_arc(crate::containers::riff::handler());
                Arc::new(r)
            })
            .clone()
    }

    /// Register `handler`, returning the registry for chaining.
    pub fn with_handler(mut self, handler: impl ContainerHandler + 'static) -> Self {
        self.add_handler(handler);
        self
    }

    /// Register `handler` for every format string it declares.
    pub fn add_handler(&mut self, handler: impl ContainerHandler + 'static) {
        self.add_arc(Arc::new(handler));
    }

    /// Register an already shared handler.
    pub fn add_arc(&mut self, handler: Arc<dyn ContainerHandler>) {
        for spec in handler.formats() {
            for f in spec.format_strings() {
                self.by_format.insert(normalize(f), handler.clone());
            }
        }
        self.ordered.push(handler);
    }

    /// The handler for a format string (extension or MIME type).
    pub fn handler(&self, format: &str) -> Option<&Arc<dyn ContainerHandler>> {
        let key = normalize(format);
        self.by_format
            .get(&key)
            .or_else(|| self.fallback.as_deref().and_then(|f| f.handler(format)))
    }

    /// The [id](ContainerHandler::id) of the container for a format string. Two
    /// format strings are the same container iff their ids are equal.
    pub fn container_for(&self, format: &str) -> Option<&'static str> {
        Some(self.handler(format)?.id())
    }

    /// The format a format string names, from the handler that serves it. The
    /// fallback is consulted only when this registry does not claim the format.
    pub fn format_for(&self, format: &str) -> Option<&FormatSpec> {
        let key = normalize(format);
        match self.by_format.get(&key) {
            Some(h) => h
                .formats()
                .iter()
                .find(|spec| spec.format_strings().any(|f| normalize(f) == key)),
            None => self.fallback.as_deref().and_then(|f| f.format_for(format)),
        }
    }

    /// The media type for a format string (extension or media type): `"heic"`
    /// gives `"image/heic"`, and an alias such as `"image/x-webp"` gives
    /// `"image/webp"`. `None` if no handler claims the format.
    pub fn media_type_for(&self, format: &str) -> Option<&'static str> {
        self.format_for(format).map(|spec| spec.media_type)
    }

    /// The preferred file extension for a format string (extension or media
    /// type): `"image/jpeg"` and `"jpeg"` both give `"jpg"`. `None` if no handler
    /// claims the format.
    pub fn extension_for(&self, format: &str) -> Option<&'static str> {
        self.format_for(format).map(|spec| spec.extension)
    }

    /// The container's own encoding of one metadata item for a format string; see
    /// [`ContainerHandler::compose`].
    pub fn compose(&self, format: &str, kind: crate::SegmentKind, data: &[u8]) -> Result<Vec<u8>> {
        self.handler(format)
            .ok_or(Error::UnsupportedFormat)?
            .compose(kind, data)
    }

    /// The handler that recognises `header` (the leading bytes of a stream). Handlers
    /// registered here are tried most-recent first, then the fallback's.
    pub fn detect(&self, header: &[u8]) -> Option<&Arc<dyn ContainerHandler>> {
        let n = header.len().min(DETECT_LEN);
        let header = &header[..n];
        self.ordered
            .iter()
            .rev()
            .find(|h| h.detect(header))
            .or_else(|| self.fallback.as_deref().and_then(|f| f.detect(header)))
    }

    /// Parse a stream as `format`.
    pub fn parse(&self, format: &str, src: &mut dyn ReadSeek) -> Result<Structure> {
        let h = self.handler(format).ok_or(Error::UnsupportedFormat)?;
        Arc::clone(h).parse(src)
    }

    /// A task that parses a stream of `total` bytes as `format`, for a caller that
    /// does its own I/O; see [`Task`]. Reports [`Error::UnsupportedFormat`] (when
    /// first resumed, if the handler does not implement
    /// [`parse_async`](ContainerHandler::parse_async)) for a format nobody claims.
    pub fn parse_task(&self, format: &str, total: u64) -> Result<Task<'static, Structure>> {
        let h = Arc::clone(self.handler(format).ok_or(Error::UnsupportedFormat)?);
        Ok(Task::new(total, move |io| h.parse_async(io)))
    }

    /// Like [`parse_task`](Self::parse_task), identifying the container from the
    /// leading bytes of the stream, which the task asks for first.
    pub fn parse_detect_task(&self, total: u64) -> Task<'_, Structure> {
        Task::new(total, move |mut io| async move {
            let n = (DETECT_LEN as u64).min(total) as usize;
            let head = io.fetch(0, n).await?.to_vec();
            let h = Arc::clone(self.detect(&head).ok_or(Error::UnsupportedFormat)?);
            h.parse_async(io).await
        })
    }

    /// Parse a stream, identifying the container from its leading bytes. The stream
    /// is rewound before parsing.
    pub fn parse_detect(&self, src: &mut dyn ReadSeek) -> Result<Structure> {
        let mut head = [0u8; DETECT_LEN];
        src.seek(SeekFrom::Start(0))?;
        let mut n = 0;
        while n < head.len() {
            let k = src.read(&mut head[n..])?;
            if k == 0 {
                break;
            }
            n += k;
        }
        src.seek(SeekFrom::Start(0))?;
        let h = self.detect(&head[..n]).ok_or(Error::UnsupportedFormat)?;
        Arc::clone(h).parse(src)
    }

    /// Every format string this registry (and its fallback) serves, sorted.
    pub fn formats(&self) -> Vec<String> {
        let mut v: Vec<String> = self.by_format.keys().cloned().collect();
        if let Some(f) = &self.fallback {
            for k in f.formats() {
                if !v.contains(&k) {
                    v.push(k);
                }
            }
        }
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::SegmentKind;
    use crate::Updates;
    use crate::{ContainerHandler, Name, ReadWriteSeek, Sink, StructureBuilder};
    use std::borrow::Cow;
    use std::io::Cursor;

    /// A made-up container: `TOY!` then records of `[len][len bytes]`.
    /// Records are named by index with `Name::Id`, shown as `rec<N>`.
    #[derive(Debug)]
    struct Toy;

    impl ContainerHandler for Toy {
        fn id(&self) -> &'static str {
            "toy"
        }
        fn formats(&self) -> &[FormatSpec] {
            const F: &[FormatSpec] = &[FormatSpec::new("application/x-toy", "toy")];
            F
        }
        fn detect(&self, header: &[u8]) -> bool {
            header.starts_with(b"TOY!")
        }
        fn parse(self: Arc<Self>, src: &mut dyn ReadSeek) -> Result<Structure> {
            let mut data = Vec::new();
            src.seek(SeekFrom::Start(0))?;
            src.read_to_end(&mut data)?;
            let mut b = StructureBuilder::new(self, "application/x-toy", data.len() as u64);
            b.push_span(SegmentKind::Header, Name::FourCC(*b"TOY!"), 4, 0, 0)?;
            let mut at = 4usize;
            let mut idx = 0u64;
            while at < data.len() {
                let len = data[at] as usize;
                if at + 1 + len > data.len() {
                    return Err(Error::InvalidFormat("toy record overruns".into()));
                }
                b.push_span(SegmentKind::Other, Name::Id(idx), 1, len as u64, 0)?;
                at += 1 + len;
                idx += 1;
            }
            b.finish()
        }
        fn write(
            &self,
            _: &Structure,
            _: &mut dyn ReadSeek,
            _: &mut dyn ReadWriteSeek,
            _: &Updates,
            _: &mut dyn Sink,
        ) -> Result<Structure> {
            Err(Error::UnsupportedFormat)
        }
        fn box_name(&self, name: Name) -> Option<Cow<'static, str>> {
            match name {
                Name::Id(n) => Some(Cow::Owned(format!("rec{n}"))),
                Name::FourCC(cc) => Some(Cow::Owned(String::from_utf8_lossy(&cc).into_owned())),
                _ => None,
            }
        }
    }

    fn toy_bytes() -> Vec<u8> {
        let mut v = b"TOY!".to_vec();
        v.extend([3, 1, 2, 3]);
        v.extend([0]);
        v.extend([2, 9, 9]);
        v
    }

    #[test]
    fn custom_handler_parses_names_and_tiles() {
        let reg = HandlerRegistry::new().with_handler(Toy);
        let bytes = toy_bytes();
        let s = reg.parse("TOY", &mut Cursor::new(bytes.clone())).unwrap();
        assert_eq!(s.total_size(), bytes.len() as u64);
        assert_eq!(s.media_type(), "application/x-toy");

        let mut names = Vec::new();
        let mut next = 0;
        s.elements(&mut Cursor::new(bytes.clone()), |_, e| {
            assert_eq!(e.extent().offset(), next);
            next = e.extent().end();
            names.push(s.box_name(e.name()).map(|n| n.into_owned()));
            Ok(())
        })
        .unwrap();
        assert_eq!(next, bytes.len() as u64);
        assert_eq!(
            names,
            [
                Some("TOY!".to_string()),
                Some("rec0".to_string()),
                Some("rec1".to_string()),
                Some("rec2".to_string())
            ]
        );
    }

    #[test]
    fn detects_by_content_and_by_format_string() {
        let reg = HandlerRegistry::new().with_handler(Toy);
        assert!(reg.detect(b"TOY!....").is_some());
        assert!(reg.detect(b"nope").is_none());
        assert!(reg.handler(" .Toy ").is_some());
        assert!(reg.handler("APPLICATION/X-TOY").is_some());
        assert_eq!(reg.container_for("application/x-toy"), Some("toy"));
        let s = reg.parse_detect(&mut Cursor::new(toy_bytes())).unwrap();
        assert_eq!(s.container_id(), "toy");
        assert!(matches!(
            reg.parse("xyz", &mut Cursor::new(toy_bytes())),
            Err(Error::UnsupportedFormat)
        ));
    }

    #[test]
    fn later_registration_wins_and_fallback_is_consulted() {
        #[derive(Debug)]
        struct Toy2;
        impl ContainerHandler for Toy2 {
            fn id(&self) -> &'static str {
                "toy2"
            }
            fn formats(&self) -> &[FormatSpec] {
                const F: &[FormatSpec] = &[FormatSpec::new("application/x-toy2", "toy")];
                F
            }
            fn parse(self: Arc<Self>, _: &mut dyn ReadSeek) -> Result<Structure> {
                Err(Error::InvalidFormat("toy2".into()))
            }
            fn write(
                &self,
                _: &Structure,
                _: &mut dyn ReadSeek,
                _: &mut dyn ReadWriteSeek,
                _: &Updates,
                _: &mut dyn Sink,
            ) -> Result<Structure> {
                Err(Error::UnsupportedFormat)
            }
        }
        let base = Arc::new(HandlerRegistry::new().with_handler(Toy));
        let over = HandlerRegistry::with_fallback(base.clone()).with_handler(Toy2);

        // The override shadows the fallback for "toy"; the fallback still serves
        // the format string the override does not claim.
        assert!(matches!(
            over.parse("toy", &mut Cursor::new(toy_bytes())),
            Err(Error::InvalidFormat(_))
        ));
        assert!(over
            .parse("application/x-toy", &mut Cursor::new(toy_bytes()))
            .is_ok());
        // The fallback itself is untouched.
        assert!(base.parse("toy", &mut Cursor::new(toy_bytes())).is_ok());
    }

    #[cfg(all(feature = "jpeg", feature = "png"))]
    #[test]
    fn builtin_handlers_are_registered_and_extendable() {
        let b = HandlerRegistry::builtin();
        for f in ["jpg", "JPEG", "image/jpeg", "png", "image/png"] {
            assert!(b.handler(f).is_some(), "{f}");
        }
        assert_eq!(b.container_for("jpeg"), b.container_for("image/jpeg"));
        assert!(b.detect(&[0xFF, 0xD8, 0xFF, 0xE0]).is_some());
        assert!(b.detect(b"\x89PNG\r\n\x1a\n").is_some());

        // A caller's registry extends the built-ins without copying them.
        let ext = HandlerRegistry::extending_builtin().with_handler(Toy);
        assert!(ext.handler("png").is_some());
        assert!(ext.handler("toy").is_some());
        assert!(b.handler("toy").is_none());
    }
}
