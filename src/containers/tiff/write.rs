// Copyright 2023 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

use super::*;
use crate::{MetadataUpdate, Role};
use std::borrow::Cow;

enum Data<'a> {
    New(&'a [u8]),
    Copy(ByteRange),
}

impl Data<'_> {
    fn len(&self) -> u64 {
        match self {
            Self::New(bytes) => bytes.len() as u64,
            Self::Copy(range) => range.len(),
        }
    }
}

struct Piece<'a> {
    at: u64,
    data: Data<'a>,
}

fn advance(at: u64, length: u64) -> Result<u64> {
    at.checked_add(length).ok_or(Error::LimitExceeded {
        what: "TIFF output size",
        limit: u64::MAX,
    })
}

fn align(at: u64, state: &TiffLayout) -> Result<u64> {
    let alignment = if state.big { 8 } else { 2 };
    Ok(advance(at, alignment - 1)? / alignment * alignment)
}

fn changed(update: &MetadataUpdate, existing: Option<&Entry>) -> bool {
    match update {
        MetadataUpdate::Keep => false,
        MetadataUpdate::Remove => existing.is_some(),
        MetadataUpdate::Set(_) => true,
    }
}

fn tag_entry(state: &TiffLayout, tag: u16, length: u64) -> Result<Entry> {
    let mut raw = state.order.put(tag as u64, 2)?;
    raw.extend(state.order.put(if tag == C2PA_TAG { 7 } else { 1 }, 2)?);
    raw.extend(state.order.put(length, state.width())?);
    raw.extend(vec![0; state.width()]);
    Ok(Entry {
        tag,
        field_type: if tag == C2PA_TAG { 7 } else { 1 },
        count: length,
        range: ByteRange::new(0, state.entry_width() as u64)?,
        value: None,
        raw,
    })
}

fn move_directory(state: &TiffLayout, dir: &mut Directory, at: u64) -> Result<()> {
    let len = state.count_width() as u64
        + dir.entries.len() as u64 * state.entry_width() as u64
        + state.width() as u64;
    dir.range = ByteRange::new(at, len)?;
    for (i, entry) in dir.entries.values_mut().enumerate() {
        entry.range = ByteRange::new(
            at + state.count_width() as u64 + i as u64 * state.entry_width() as u64,
            state.entry_width() as u64,
        )?;
        if let Some(value) = &mut entry.value {
            if value.len() <= state.width() as u64 {
                *value =
                    ByteRange::new(entry.range.offset() + 4 + state.width() as u64, value.len())?;
            }
        }
    }
    Ok(())
}

fn write_value<'a>(
    state: &TiffLayout,
    entry: &mut Entry,
    data: Data<'a>,
    inline: Option<&[u8]>,
    end: &mut u64,
    payloads: &mut Vec<Piece<'a>>,
) -> Result<()> {
    let field = 4 + state.width();
    if data.len() <= state.width() as u64 {
        let bytes = match &data {
            Data::New(bytes) => bytes,
            Data::Copy(_) => inline.ok_or(Error::NotFound {
                what: "inline TIFF value",
            })?,
        };
        entry.raw[field..].fill(0);
        entry.raw[field..field + bytes.len()].copy_from_slice(bytes);
        entry.value = Some(ByteRange::new(
            entry.range.offset() + field as u64,
            data.len(),
        )?);
    } else {
        let at = align(*end, state)?;
        entry.raw[field..].copy_from_slice(&state.order.put(at, state.width())?);
        entry.value = Some(ByteRange::new(at, data.len())?);
        *end = advance(at, data.len())?;
        payloads.push(Piece { at, data });
    }
    Ok(())
}

pub(super) fn plan<'a>(source: &Structure, updates: &'a Updates) -> Result<WritePlan<'a>> {
    let original = layout(source)?;
    let old_manifest = original.manifest_entry();
    let old_xmp = original.xmp_entry();
    let change_xmp = changed(updates.xmp(), old_xmp);
    let change_manifest = changed(updates.jumbf(), old_manifest);
    if !change_xmp && !change_manifest {
        return emit(
            source,
            Arc::new(original.clone()),
            source.total_size(),
            Vec::new(),
            Vec::new(),
        );
    }
    // Placeholder filling must preserve the IFD and every other byte.
    if !change_xmp {
        if let (MetadataUpdate::Set(bytes), Some(entry)) = (updates.jumbf(), old_manifest) {
            if entry.count == bytes.len() as u64 {
                let range = entry.value.ok_or(Error::NotFound {
                    what: "TIFF manifest range",
                })?;
                let mut state = original.clone();
                if range.len() <= state.width() as u64 {
                    let width = state.width();
                    for dir in state.directories.values_mut() {
                        if let Some(e) = dir.entries.get_mut(&C2PA_TAG) {
                            if e.range == entry.range {
                                e.raw[4 + width..4 + width + bytes.len()].copy_from_slice(bytes);
                            }
                        }
                    }
                }
                return emit(
                    source,
                    Arc::new(state),
                    source.total_size(),
                    vec![(range.offset(), Cow::Borrowed(bytes.as_slice()))],
                    Vec::new(),
                );
            }
        }
    }
    let mut state = original.clone();
    let mut pages: Vec<Directory> = original
        .pages()
        .filter(|d| {
            !(original.pages.len() > 1 && d.entries.len() == 1 && d.entry(C2PA_TAG).is_some())
        })
        .cloned()
        .collect();
    for dir in &mut pages {
        dir.entries.remove(&C2PA_TAG);
    }
    let first = pages
        .first_mut()
        .ok_or(Error::InvalidFormat("TIFF has no image IFD".into()))?;
    match updates.xmp() {
        MetadataUpdate::Keep => {}
        MetadataUpdate::Remove => {
            first.entries.remove(&XMP_TAG);
        }
        MetadataUpdate::Set(bytes) => {
            first
                .entries
                .insert(XMP_TAG, tag_entry(&state, XMP_TAG, bytes.len() as u64)?);
        }
    }
    let manifest = match updates.jumbf() {
        MetadataUpdate::Keep => old_manifest.and_then(|e| e.value).map(Data::Copy),
        MetadataUpdate::Remove => None,
        MetadataUpdate::Set(bytes) => Some(Data::New(bytes.as_slice())),
    };
    if let Some(data) = &manifest {
        if pages.len() > 1 {
            pages.push(Directory {
                range: ByteRange::new(0, 0)?,
                entries: BTreeMap::new(),
                next: 0,
                children: Vec::new(),
            });
        }
        pages
            .last_mut()
            .ok_or(Error::NotFound {
                what: "TIFF image IFD",
            })?
            .entries
            .insert(C2PA_TAG, tag_entry(&state, C2PA_TAG, data.len())?);
    }
    let mut end = source.total_size();
    for dir in &mut pages {
        let at = align(end, &state)?;
        state.order.put(at, state.width())?;
        move_directory(&state, dir, at)?;
        end = dir.range.end();
    }
    for i in 0..pages.len() {
        pages[i].next = pages.get(i + 1).map_or(0, |d| d.range.offset());
    }
    let mut payloads = Vec::new();
    if let MetadataUpdate::Set(bytes) = updates.xmp() {
        let entry = pages[0].entries.get_mut(&XMP_TAG).ok_or(Error::NotFound {
            what: "TIFF XMP tag",
        })?;
        write_value(
            &state,
            entry,
            Data::New(bytes),
            None,
            &mut end,
            &mut payloads,
        )?;
    }
    if let Some(data) = manifest {
        let inline = old_manifest.map(|e| {
            &e.raw
                [4 + state.width()..4 + state.width() + e.count.min(state.width() as u64) as usize]
        });
        let entry = pages
            .last_mut()
            .and_then(|d| d.entries.get_mut(&C2PA_TAG))
            .ok_or(Error::NotFound {
                what: "TIFF manifest tag",
            })?;
        write_value(&state, entry, data, inline, &mut end, &mut payloads)?;
    }
    let pointer = pages[0].range.offset();
    let mut appended = Vec::new();
    state.pages.clear();
    for dir in pages {
        let mut bytes = state
            .order
            .put(dir.entries.len() as u64, state.count_width())?;
        for entry in dir.entries.values() {
            bytes.extend_from_slice(&entry.raw);
        }
        bytes.extend(state.order.put(dir.next, state.width())?);
        appended.push((dir.range.offset(), Cow::Owned(bytes)));
        state.pages.push(dir.range.offset());
        state.directories.insert(dir.range.offset(), dir);
    }
    let mut reachable = BTreeSet::new();
    let mut queue = vec![pointer];
    while let Some(at) = queue.pop() {
        if !reachable.insert(at) {
            continue;
        }
        let dir = &state.directories[&at];
        queue.extend(dir.children.iter().copied());
        if dir.next != 0 {
            queue.push(dir.next);
        }
    }
    state.directories.retain(|at, _| reachable.contains(at));
    validate_layout(&state)?;
    emit(
        source,
        Arc::new(state),
        end,
        vec![(
            if original.big { 8 } else { 4 },
            Cow::Owned(original.order.put(pointer, original.width())?),
        )],
        {
            let mut pieces: Vec<_> = appended
                .into_iter()
                .map(|(at, data)| (at, Output::Bytes(data)))
                .collect();
            pieces.extend(payloads.into_iter().map(|piece| {
                (
                    piece.at,
                    match piece.data {
                        Data::New(bytes) => Output::Bytes(Cow::Borrowed(bytes)),
                        Data::Copy(range) => Output::Copy(range),
                    },
                )
            }));
            pieces.sort_by_key(|p| p.0);
            pieces
        },
    )
}

enum Output<'a> {
    Bytes(Cow<'a, [u8]>),
    Copy(ByteRange),
}

fn emit<'a>(
    source: &Structure,
    state: Arc<TiffLayout>,
    total: u64,
    patches: Vec<(u64, Cow<'a, [u8]>)>,
    appended: Vec<(u64, Output<'a>)>,
) -> Result<WritePlan<'a>> {
    let structure = structure(source.handler().clone(), total, state)?;
    let mut pieces = Vec::new();
    let mut at = 0;
    for (offset, bytes) in patches {
        if offset > at {
            pieces.push((at, Output::Copy(ByteRange::new(at, offset - at)?)));
        }
        at = advance(offset, bytes.len() as u64)?;
        pieces.push((offset, Output::Bytes(bytes)));
    }
    if at < source.total_size() {
        pieces.push((
            at,
            Output::Copy(ByteRange::new(at, source.total_size() - at)?),
        ));
    }
    at = source.total_size();
    for (offset, data) in appended {
        if offset > at {
            pieces.push((
                at,
                Output::Bytes(Cow::Owned(vec![0; (offset - at) as usize])),
            ));
        }
        let len = match &data {
            Output::Bytes(bytes) => bytes.len() as u64,
            Output::Copy(range) => range.len(),
        };
        at = advance(offset, len)?;
        pieces.push((offset, data));
    }
    let mut plan = WritePlan::new(structure);
    let mut segment = 0;
    for (offset, data) in pieces {
        let length = match &data {
            Output::Bytes(bytes) => bytes.len() as u64,
            Output::Copy(range) => range.len(),
        };
        let mut used = 0;
        while used < length {
            let seg = plan.structure().segments()[segment];
            if offset + used >= seg.range().end() {
                segment += 1;
                continue;
            }
            let n = (length - used).min(seg.range().end() - offset - used);
            let id = SegmentId::from_index(segment)?;
            let role = if seg.kind() == SegmentKind::Preamble {
                Role::Header
            } else {
                Role::Payload
            };
            match &data {
                Output::Bytes(bytes) => {
                    let bytes = match bytes {
                        Cow::Borrowed(bytes) => {
                            Cow::Borrowed(&bytes[used as usize..(used + n) as usize])
                        }
                        Cow::Owned(bytes) => {
                            Cow::Owned(bytes[used as usize..(used + n) as usize].to_vec())
                        }
                    };
                    plan.put(id, role, bytes);
                }
                Output::Copy(range) => {
                    plan.copy(id, role, ByteRange::new(range.offset() + used, n)?);
                }
            }
            used += n;
        }
    }
    Ok(plan)
}
