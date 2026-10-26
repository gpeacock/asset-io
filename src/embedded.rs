// Copyright 2026 Adobe. All rights reserved.
// This file is licensed under the Apache License, Version 2.0 or the MIT license,
// at your option.

//! Opt-in discovery and scanning of embedded files without copying their data.

use crate::{ByteRange, Entity, Error, HandlerRegistry, ReadSeek, Result, SegmentKind, Structure};
use std::io::{self, Read, Seek, SeekFrom};

/// Resource budgets for an embedded scan. Exceeding a budget returns an error,
/// not a truncated result. The root asset is not counted.
#[derive(Debug, Clone, Copy)]
pub struct ScanLimits {
    /// Maximum nesting depth; immediate children have depth 1. Capped at 32.
    pub max_depth: usize,
    /// Maximum number of discovered assets, including unsupported ones.
    pub max_assets: usize,
    /// Sum of embedded asset lengths, counting nested bytes at each level.
    pub max_total_bytes: u64,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_assets: 64,
            max_total_bytes: 256 * 1024 * 1024,
        }
    }
}

/// One embedded file discovered by [`Structure::scan_embedded`].
///
/// Its structure's offsets are relative to its own stream, not the original
/// file. Use [`reader`](Self::reader) to read its metadata from the original
/// source. No embedded image data is retained in memory.
#[derive(Debug)]
pub struct EmbeddedAsset {
    /// The containing structure's entity, including its label and declared type.
    pub entity: Entity,
    /// Index of the parent in the scan result, or `None` for a root child.
    pub parent: Option<usize>,
    /// Immediate children have depth 1.
    pub depth: usize,
    /// The parsed contents, or the specific error (including unsupported format).
    /// Malformed or unsupported children do not hide other siblings.
    pub structure: Result<Structure>,
    ranges: Vec<ByteRange>,
}

impl EmbeddedAsset {
    /// Byte ranges in the original source, in logical order.
    pub fn ranges(&self) -> &[ByteRange] {
        &self.ranges
    }

    /// Open this asset as a bounded stream over the original source. This also
    /// works for an unsupported asset, so applications can inspect its raw data.
    pub fn reader<'a, R: Read + Seek + ?Sized>(
        &'a self,
        src: &'a mut R,
    ) -> Result<EmbeddedReader<'a, R>> {
        EmbeddedReader::new(src, &self.ranges)
    }
}

/// A seekable, payload-only view of an embedded file's possibly disjoint extents.
/// Reads and seeks are relative to the embedded file. Seeking beyond EOF is
/// allowed, like a normal file, but reading never escapes its extents.
pub struct EmbeddedReader<'a, R: ?Sized> {
    src: &'a mut R,
    ranges: &'a [ByteRange],
    starts: Vec<u64>,
    len: u64,
    pos: u64,
}

impl<'a, R: Read + Seek + ?Sized> EmbeddedReader<'a, R> {
    fn new(src: &'a mut R, ranges: &'a [ByteRange]) -> Result<Self> {
        let mut starts = Vec::with_capacity(ranges.len());
        let mut len = 0u64;
        for range in ranges {
            starts.push(len);
            len = len.checked_add(range.len()).ok_or(Error::LimitExceeded {
                what: "embedded asset length",
                limit: u64::MAX,
            })?;
        }
        Ok(Self {
            src,
            ranges,
            starts,
            len,
            pos: 0,
        })
    }
}

impl<R: Read + Seek + ?Sized> Read for EmbeddedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let i = self.starts.partition_point(|&start| start <= self.pos) - 1;
        let range = self.ranges[i];
        let relative = self.pos - self.starts[i];
        let n = (range.len() - relative).min(buf.len() as u64) as usize;
        self.src.seek(SeekFrom::Start(range.offset() + relative))?;
        let n = self.src.read(&mut buf[..n])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "embedded extent ended before its declared length",
            ));
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek + ?Sized> Seek for EmbeddedReader<'_, R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let pos = match from {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
        };
        if !(0..=u64::MAX as i128).contains(&pos) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"));
        }
        self.pos = pos as u64;
        Ok(self.pos)
    }
}

impl Structure {
    /// Embedded files and container-level thumbnails in declaration order.
    ///
    /// Handlers report self-contained files as [`SegmentKind::Embedded`].
    /// HEIF reports JPEG, MIME and URI-typed items, preserving a URI as the
    /// entity label. A URI is an identifier only: it is never fetched.
    /// Coded HEIF thumbnails are included but cannot be parsed as standalone
    /// files. EXIF-internal thumbnails and MPF subassets are separate APIs.
    pub fn embedded_assets(&self) -> impl Iterator<Item = &Entity> {
        self.entity_list().filter_map(|(kind, entity)| {
            matches!(kind, SegmentKind::Embedded | SegmentKind::Thumbnail).then_some(entity)
        })
    }

    /// Recursively scan declared embedded files using `registry`, in depth-first
    /// order. Ordinary parsing remains shallow. Formats are detected from bytes;
    /// this does not carve arbitrary image signatures out of opaque payloads.
    ///
    /// Each child's parse error is retained in [`EmbeddedAsset::structure`].
    /// Resource-budget and extent-mapping errors fail the entire scan. External
    /// HEIF references are never followed. No pixel decoding is performed.
    pub fn scan_embedded(
        &self,
        src: &mut dyn ReadSeek,
        registry: &HandlerRegistry,
        limits: ScanLimits,
    ) -> Result<Vec<EmbeddedAsset>> {
        let mut scanner = Scanner {
            registry,
            limits: ScanLimits {
                max_depth: limits.max_depth.min(32),
                ..limits
            },
            assets: Vec::new(),
            bytes: 0,
        };
        scanner.scan(
            src,
            self.embedded_assets()
                .take(limits.max_assets.saturating_add(1))
                .cloned()
                .collect(),
            &[ByteRange::new(0, self.total_size())?],
            None,
            1,
        )?;
        Ok(scanner.assets)
    }
}

struct Scanner<'a> {
    registry: &'a HandlerRegistry,
    limits: ScanLimits,
    assets: Vec<EmbeddedAsset>,
    bytes: u64,
}

impl Scanner<'_> {
    fn scan(
        &mut self,
        src: &mut dyn ReadSeek,
        entities: Vec<Entity>,
        parent_ranges: &[ByteRange],
        parent: Option<usize>,
        depth: usize,
    ) -> Result<()> {
        for entity in entities {
            if depth > self.limits.max_depth {
                return Err(Error::LimitExceeded {
                    what: "embedded depth",
                    limit: self.limits.max_depth as u64,
                });
            }
            if self.assets.len() >= self.limits.max_assets {
                return Err(Error::LimitExceeded {
                    what: "embedded assets",
                    limit: self.limits.max_assets as u64,
                });
            }
            self.bytes = self
                .bytes
                .checked_add(entity.len())
                .filter(|&n| n <= self.limits.max_total_bytes)
                .ok_or(Error::LimitExceeded {
                    what: "embedded bytes",
                    limit: self.limits.max_total_bytes,
                })?;
            let ranges = map_ranges(parent_ranges, &entity)?;
            let structure = self
                .registry
                .parse_detect(&mut EmbeddedReader::new(src, &ranges)?);
            let children = match &structure {
                Ok(s) => s
                    .embedded_assets()
                    .take(
                        self.limits
                            .max_assets
                            .saturating_sub(self.assets.len())
                            .saturating_add(1),
                    )
                    .cloned()
                    .collect(),
                Err(_) => Vec::new(),
            };
            let index = self.assets.len();
            self.assets.push(EmbeddedAsset {
                entity,
                parent,
                depth,
                structure,
                ranges: ranges.clone(),
            });
            self.scan(src, children, &ranges, Some(index), depth + 1)?;
        }
        Ok(())
    }
}

fn map_ranges(parent: &[ByteRange], entity: &Entity) -> Result<Vec<ByteRange>> {
    let mut out = Vec::new();
    for part in entity.parts() {
        let mut offset = part.range().offset();
        let mut left = part.range().len();
        for range in parent {
            if offset >= range.len() {
                offset -= range.len();
                continue;
            }
            let n = left.min(range.len() - offset);
            if n > 0 {
                if out.len() >= 65536 {
                    return Err(Error::LimitExceeded {
                        what: "embedded extents",
                        limit: 65536,
                    });
                }
                out.push(range.sub(offset, n)?);
            }
            left -= n;
            offset = 0;
            if left == 0 {
                break;
            }
        }
        if left != 0 {
            return Err(Error::invalid_segment(
                part.range().offset(),
                "embedded extent exceeds its containing asset",
            ));
        }
    }
    Ok(out)
}
