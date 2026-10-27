// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

//! Raw C2PA manifest-store sidecars, without additional container framing.
//!
//! A nonempty sidecar is one metadata segment and one JUMBF item covering the
//! entire stream. An empty sidecar has neither. Explicit parsing accepts opaque
//! bytes; detection recognizes the standard-length C2PA JUMBF prefix, not every
//! JUMBF type. This handler does not validate manifests or signatures.

use asset_io_core::jumbf::superbox_type;
use asset_io_core::{
    BoxFuture, ContainerHandler, Error, FormatSpec, Io, Item, ItemInfo, ItemKind, MetadataUpdate,
    Name, PlanBuilder, Result, SegmentId, SegmentKind, Structure, StructureBuilder, Updates,
    WritePlan,
};
use std::io::{Read, Seek};
use std::sync::Arc;

const STORE_TYPE: [u8; 16] = [
    0x63, 0x32, 0x70, 0x61, 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

/// The C2PA sidecar container's [`ContainerHandler::id`].
pub const ID: &str = "c2pa";

/// A raw manifest-store sidecar handler.
#[derive(Debug)]
pub struct C2paHandler;

/// A handler for explicit registration.
pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(C2paHandler)
}

impl ContainerHandler for C2paHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const FORMATS: &[FormatSpec] = &[FormatSpec::new("application/c2pa", "c2pa")
            .media_type_aliases(&["application/x-c2pa-manifest-store"])];
        FORMATS
    }

    fn detect(&self, header: &[u8]) -> bool {
        superbox_type(header) == Some(STORE_TYPE)
    }

    fn parse_async(self: Arc<Self>, mut io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async move {
            let total = io.len();
            let mut builder = StructureBuilder::new(self, "application/c2pa", total);
            if total != 0 {
                let info = item_info(io.fetch(0, total.min(32) as usize).await?);
                builder.push_item(SegmentKind::Metadata, Name::None, 0, total, 0, info)?;
            }
            builder.finish()
        })
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        source: &'f Structure,
        _io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move { plan(source, updates) })
    }

    fn compose(&self, kind: ItemKind, data: &[u8]) -> Result<Vec<u8>> {
        if kind != ItemKind::Jumbf {
            return Err(Error::UnsupportedFormat);
        }
        Ok(data.to_vec())
    }

    fn check_patch(&self, _structure: &Structure, item: &Item, len: usize) -> Result<()> {
        if len as u64 != item.len() {
            return Err(Error::InvalidFormat(
                "C2PA sidecar in-place updates require exact-length payloads".into(),
            ));
        }
        Ok(())
    }
}

/// Describe a sidecar's bytes without validating the manifest store.
pub fn parse<R: Read + Seek>(source: &mut R) -> Result<Structure> {
    handler().parse(source)
}

fn item_info(data: &[u8]) -> ItemInfo {
    ItemInfo::jumbf().jumbf_type(superbox_type(data))
}

fn plan<'a>(source: &Structure, updates: &'a Updates) -> Result<WritePlan<'a>> {
    match updates.xmp() {
        MetadataUpdate::Keep | MetadataUpdate::Remove => {}
        _ => {
            return Err(Error::UnsupportedFeature(
                "C2PA sidecars cannot contain XMP",
            ))
        }
    }

    let mut builder = PlanBuilder::new(source);
    let keep = match updates.jumbf() {
        MetadataUpdate::Keep => true,
        MetadataUpdate::Remove => !source
            .items_of(ItemKind::Jumbf)
            .any(|item| updates.jumbf_targets(item.jumbf_type())),
        MetadataUpdate::Set(data) => {
            if !data.is_empty() {
                let id = builder.put_segment(
                    SegmentKind::Metadata,
                    Name::None,
                    &[][..],
                    data.as_slice(),
                    &[][..],
                )?;
                builder.item(item_info(data), &[id])?;
            }
            false
        }
        _ => return Err(Error::UnsupportedFeature("C2PA sidecar JUMBF update")),
    };
    if keep {
        for i in 0..source.segments().len() {
            builder.copy_segment(SegmentId::from_index(i)?)?;
        }
        builder.carry_items(|_| true);
    }
    builder.finish()
}
