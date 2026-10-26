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

//! Conformance checks for [`ContainerHandler`]s.
//!
//! Every handler has to keep the same promises, whatever its format: the
//! structure it parses tiles the stream, a write that changes nothing changes
//! nothing, the plan, the write and the stepper agree, the sink sees every output
//! byte once and in order, and bad input is an error, never a panic.
//! [`Conformance`] checks all of that on samples you give it, so a handler's own
//! tests only have to cover what is particular to its format.
//!
//! ```no_run
//! use asset_io::testing::Conformance;
//! use asset_io::containers::png;
//!
//! # let sample: Vec<u8> = Vec::new();
//! Conformance::new(png::handler()).check("sample.png", &sample);
//! ```
//!
//! A check panics with the name of the sample and what was wrong, so it can be
//! used directly in a `#[test]`; [`try_check`](Conformance::try_check) returns the
//! description instead.

use crate::error::{Error, Result};
use crate::execute::Action;
use crate::handler::ContainerHandler;
use crate::item::ItemKind;
use crate::plan::{validate, WritePlan};
use crate::segment::{Role, SegmentKind};
use crate::sink::{Chunk, Sink};
use crate::structure::Structure;
use crate::task::{Progress, Task};
use crate::updates::Updates;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// The XMP the checks write unless told otherwise: an xpacket, which every format
/// that holds XMP accepts.
const DEFAULT_XMP: &[u8] = b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><!-- conformance --></x:xmpmeta>\
<?xpacket end=\"w\"?>";

/// A set of checks for one handler. Build it with [`new`](Self::new), say what the
/// format can hold, and run it on samples with [`check`](Self::check).
#[derive(Clone)]
pub struct Conformance {
    handler: Arc<dyn ContainerHandler>,
    xmp: Option<Vec<u8>>,
    jumbf: Option<Vec<u8>>,
    extra: Vec<(String, Arc<Updates>)>,
    mutations: bool,
}

impl Conformance {
    /// Checks for `handler`. By default it is expected to store XMP, and not to
    /// take JUMBF, which has to be in a form the format accepts: set it with
    /// [`jumbf`](Self::jumbf).
    pub fn new(handler: Arc<dyn ContainerHandler>) -> Self {
        Self {
            handler,
            xmp: Some(DEFAULT_XMP.to_vec()),
            jumbf: None,
            extra: Vec::new(),
            mutations: true,
        }
    }

    /// The XMP packet to write and read back; `None` if the format cannot hold XMP.
    pub fn xmp(mut self, packet: Option<Vec<u8>>) -> Self {
        self.xmp = packet;
        self
    }

    /// The JUMBF to write and read back; `None` if the format does not take JUMBF.
    pub fn jumbf(mut self, store: Option<Vec<u8>>) -> Self {
        self.jumbf = store;
        self
    }

    /// Another update to run every check with, such as an option only this format
    /// has. A handler may refuse it for a sample (an error other than
    /// [`Error::UnsupportedFormat`]); what it writes is checked as for any other.
    pub fn updates(mut self, name: &str, updates: Updates) -> Self {
        self.extra.push((name.to_owned(), Arc::new(updates)));
        self
    }

    /// Whether to run the sample's truncations and byte flips (on by default).
    pub fn mutations(mut self, on: bool) -> Self {
        self.mutations = on;
        self
    }

    /// Run every check on `sample`, panicking on the first failure.
    pub fn check(&self, name: &str, sample: &[u8]) {
        if let Err(why) = self.try_check(name, sample) {
            panic!("{why}");
        }
    }

    /// Like [`check`](Self::check), but a sample the handler cannot parse passes:
    /// there is nothing to hold it to. For feeding arbitrary bytes, as a fuzzer does.
    pub fn check_if_parses(&self, name: &str, sample: &[u8]) {
        if self.handler.clone().parse(&mut Cursor::new(sample)).is_ok() {
            self.check(name, sample);
        }
    }

    /// Run every check on `sample`; the error says which failed and why.
    pub fn try_check(&self, name: &str, sample: &[u8]) -> std::result::Result<(), String> {
        self.run(sample).map_err(|why| format!("{name}: {why}"))
    }

    fn run(&self, sample: &[u8]) -> std::result::Result<(), String> {
        let s = self
            .handler
            .clone()
            .parse(&mut Cursor::new(sample))
            .map_err(|e| format!("does not parse: {e}"))?;
        tiles(&s, sample.len() as u64).map_err(|e| format!("parse: {e}"))?;
        parse_by_hand(&self.handler, sample, &s)?;

        // A handler that cannot write has only the parse to check.
        match s.write_plan(&mut Cursor::new(sample), &Updates::new()) {
            Err(Error::UnsupportedFormat) => return Ok(()),
            Err(e) => return Err(format!("cannot plan a write that changes nothing: {e}")),
            Ok(_) => {}
        }

        let keep = self
            .write(&s, sample, &Updates::new())?
            .ok_or("a write that changes nothing was refused")?;
        if keep.bytes != sample {
            return Err("a write that changes nothing changed the bytes".into());
        }
        if describe(&keep.structure) != describe(&s) {
            return Err("a write that changes nothing returned another structure".into());
        }

        for (name, updates, expect) in self.cases() {
            let Some(done) = self
                .write(&s, sample, &updates)
                .map_err(|e| format!("{name}: {e}"))?
            else {
                continue;
            };
            let src = &mut Cursor::new(&done.bytes);
            let wrote = self
                .handler
                .clone()
                .parse(src)
                .map_err(|e| format!("{name}: the output does not parse: {e}"))?;
            tiles(&wrote, done.bytes.len() as u64).map_err(|e| format!("{name}: output: {e}"))?;
            if describe(&wrote) != describe(&done.structure) {
                return Err(format!(
                    "{name}: the output is not the structure that was planned\n planned: {}\n parsed:  {}",
                    describe(&done.structure),
                    describe(&wrote)
                ));
            }
            let mut read = Collect::default();
            wrote
                .read(&mut Cursor::new(&done.bytes), &mut read)
                .map_err(|e| format!("{name}: reading the output: {e}"))?;
            read.check(&wrote, &done.bytes)
                .map_err(|e| format!("{name}: reading the output: {e}"))?;
            expect
                .check(&wrote, &done.bytes, &s, sample)
                .map_err(|e| format!("{name}: {e}"))?;

            // What was written, written again unchanged, is the same file.
            let again = self
                .write(&wrote, &done.bytes, &Updates::new())
                .map_err(|e| format!("{name}: rewriting the output: {e}"))?;
            if let Some(again) = again {
                if again.bytes != done.bytes {
                    return Err(format!("{name}: rewriting the output changed it"));
                }
            }
        }

        if self.mutations {
            self.mutated(sample)?;
        }
        Ok(())
    }

    /// The updates every sample is written with, and what each must do.
    fn cases(&self) -> Vec<(String, Arc<Updates>, Expect)> {
        let mut cases = vec![];
        let mut add =
            |name: &str, u: Updates, e: Expect| cases.push((name.to_owned(), Arc::new(u), e));
        if let Some(x) = &self.xmp {
            add(
                "set xmp",
                Updates::new().set_xmp(x.clone()),
                Expect {
                    xmp: Want::Is(x.clone()),
                    jumbf: Want::Same,
                },
            );
            add(
                "remove xmp",
                Updates::new().remove_xmp(),
                Expect {
                    xmp: Want::Absent,
                    jumbf: Want::Same,
                },
            );
        }
        if let Some(j) = &self.jumbf {
            add(
                "set jumbf",
                Updates::new().set_jumbf(j.clone()),
                Expect {
                    xmp: Want::Same,
                    jumbf: Want::Is(j.clone()),
                },
            );
            add(
                "remove jumbf",
                Updates::new().remove_jumbf(),
                Expect {
                    xmp: Want::Same,
                    jumbf: Want::Absent,
                },
            );
        }
        if let (Some(x), Some(j)) = (&self.xmp, &self.jumbf) {
            add(
                "set both",
                Updates::new().set_xmp(x.clone()).set_jumbf(j.clone()),
                Expect {
                    xmp: Want::Is(x.clone()),
                    jumbf: Want::Is(j.clone()),
                },
            );
        }
        add(
            "remove all",
            Updates::remove_all(),
            Expect {
                xmp: Want::Absent,
                jumbf: Want::Absent,
            },
        );
        for (name, u) in &self.extra {
            cases.push((
                name.clone(),
                u.clone(),
                Expect {
                    xmp: Want::Any,
                    jumbf: Want::Any,
                },
            ));
        }
        cases
    }

    /// Write with `updates` every way there is, checking they agree. `None` if the
    /// handler refuses the update for this sample.
    fn write(
        &self,
        s: &Structure,
        sample: &[u8],
        updates: &Updates,
    ) -> std::result::Result<Option<Written>, String> {
        let plan = match s.write_plan(&mut Cursor::new(sample), updates) {
            Ok(p) => p,
            Err(Error::UnsupportedFormat) => {
                return Err("a write the format should take is unsupported".into())
            }
            Err(_) => {
                // A refused update is refused whichever way it is asked for.
                return match s.write(
                    &mut Cursor::new(sample),
                    &mut Vec::new(),
                    updates,
                    &mut crate::NullSink,
                ) {
                    Err(Error::UnsupportedFormat) => {
                        Err("a write the format should take is unsupported".into())
                    }
                    Err(_) => Ok(None),
                    Ok(_) => Err("write succeeded where write_plan failed".into()),
                };
            }
        };
        validate(&plan, sample.len() as u64)
            .map_err(|e| format!("the plan does not tile its structure: {e}"))?;

        let mut bytes = Vec::new();
        let mut sink = Collect::default();
        let structure = s
            .write(&mut Cursor::new(sample), &mut bytes, updates, &mut sink)
            .map_err(|e| format!("write failed after a plan: {e}"))?;
        if describe(&structure) != describe(plan.structure()) {
            return Err("write returned a structure other than the plan's".into());
        }
        if bytes.len() as u64 != structure.total_size() {
            return Err(format!(
                "wrote {} bytes for a structure of {}",
                bytes.len(),
                structure.total_size()
            ));
        }
        let planned = s
            .plan(&mut Cursor::new(sample), updates)
            .map_err(|e| format!("plan failed after a plan: {e}"))?;
        if describe(&planned) != describe(&structure) {
            return Err("Structure::plan is not what write returns".into());
        }
        sink.check(&structure, &bytes)?;
        selective_sink(s, sample, updates, &structure, &bytes)?;

        if step_by_step(&plan, sample)? != bytes {
            return Err("running the plan step by step wrote other bytes than write".into());
        }
        Ok(Some(Written { bytes, structure }))
    }

    /// Damaged input must be an error or a structure that is still sound; never
    /// a panic.
    fn mutated(&self, sample: &[u8]) -> std::result::Result<(), String> {
        let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
        let n = sample.len();
        let mut cuts: Vec<usize> = (0..n.min(48)).collect();
        cuts.extend((1..=48).map(|i| n * i / 49));
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            inputs.push((format!("truncated to {cut} bytes"), sample[..cut].to_vec()));
        }
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..96.min(n * 2) {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let at = (rng % n.max(1) as u64) as usize;
            let mut v = sample.to_vec();
            if let Some(b) = v.get_mut(at) {
                *b ^= (rng >> 32) as u8 | 1;
            }
            inputs.push((format!("byte {at} changed"), v));
        }
        for (what, bytes) in inputs {
            let tried = catch_unwind(AssertUnwindSafe(|| self.damaged(&bytes)));
            match tried {
                Err(_) => return Err(format!("panicked on input {what}")),
                Ok(Err(why)) => return Err(format!("on input {what}: {why}")),
                Ok(Ok(())) => {}
            }
        }
        Ok(())
    }

    fn damaged(&self, bytes: &[u8]) -> std::result::Result<(), String> {
        let Ok(s) = self.handler.clone().parse(&mut Cursor::new(bytes)) else {
            return Ok(());
        };
        tiles(&s, bytes.len() as u64).map_err(|e| format!("parse: {e}"))?;
        let mut updates = vec![Updates::new()];
        if let Some(x) = &self.xmp {
            updates.push(Updates::new().set_xmp(x.clone()));
        }
        if let Some(j) = &self.jumbf {
            updates.push(Updates::new().set_jumbf(j.clone()));
        }
        updates.push(Updates::remove_all());
        for u in &updates {
            if let Ok(plan) = s.write_plan(&mut Cursor::new(bytes), u) {
                validate(&plan, bytes.len() as u64)
                    .map_err(|e| format!("the plan does not tile: {e}"))?;
                // Reading damaged data may fail; it must not misbehave.
                let _ = plan.execute(
                    &mut Cursor::new(bytes),
                    &mut Vec::new(),
                    &mut crate::NullSink,
                );
            }
        }
        Ok(())
    }
}

struct Written {
    bytes: Vec<u8>,
    structure: Structure,
}

enum Want {
    /// Whatever the source had.
    Same,
    /// This value.
    Is(Vec<u8>),
    /// Nothing.
    Absent,
    /// Not checked.
    Any,
}

struct Expect {
    xmp: Want,
    jumbf: Want,
}

impl Expect {
    fn check(
        &self,
        out: &Structure,
        bytes: &[u8],
        src: &Structure,
        source: &[u8],
    ) -> std::result::Result<(), String> {
        type Item = std::result::Result<Option<Vec<u8>>, String>;
        let read = |s: &Structure, b: &[u8]| -> (Item, Item) {
            (
                s.xmp(&mut Cursor::new(b))
                    .map_err(|e| format!("reading XMP: {e}")),
                s.store(&mut Cursor::new(b))
                    .map_err(|e| format!("reading JUMBF: {e}")),
            )
        };
        let (xmp, jumbf) = read(out, bytes);
        let (xmp0, jumbf0) = read(src, source);
        for (what, kind, want, got, before) in [
            ("XMP", ItemKind::Xmp, &self.xmp, xmp, xmp0),
            ("JUMBF", ItemKind::Jumbf, &self.jumbf, jumbf, jumbf0),
        ] {
            match want {
                Want::Any => {}
                // A source whose item cannot be read (a corrupt or ambiguous one) has
                // nothing to compare with.
                Want::Same => {
                    if let Ok(before) = before {
                        if got? != before {
                            return Err(format!("{what} changed but was not asked to"));
                        }
                    }
                }
                Want::Is(v) => {
                    let got = got?;
                    if got.as_deref() != Some(v.as_slice()) {
                        return Err(format!(
                            "{what} is not what was set: read back {:?}",
                            got.map(|g| g.len())
                        ));
                    }
                }
                Want::Absent => {
                    if let Some(got) = got? {
                        if !carried(out, bytes, kind, &got) {
                            return Err(format!("{what} is still there after it was removed"));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Whether `data` is an item of `kind` outside the metadata segments, which
/// [`Updates`] does not edit: an XMP item that a HEIF file keeps in `mdat` is carried
/// through a write unchanged.
fn carried(s: &Structure, bytes: &[u8], kind: ItemKind, data: &[u8]) -> bool {
    s.items_of(kind).any(|e| {
        e.extents().all(|x| {
            s.segment(x.segment())
                .is_ok_and(|g| g.kind() != SegmentKind::Metadata)
        }) && s
            .read_item(&mut Cursor::new(bytes), e)
            .is_ok_and(|d| d == data)
    })
}

/// Everything a structure says, as text, for comparing two of them.
fn describe(s: &Structure) -> String {
    let mut out = String::new();
    for seg in s.segments() {
        out.push_str(&format!("{seg:?}\n"));
    }
    for sub in s.subassets() {
        out.push_str(&format!(
            "sub {:?} {:?} {:?}\n",
            sub.range(),
            sub.segments(),
            sub.media_type()
        ));
    }
    out.push_str(&format!(
        "trailing {:?} total {} {}\n",
        s.trailing(),
        s.total_size(),
        s.media_type()
    ));
    // Items in file order: how a handler declares them is not part of what a
    // structure says.
    let mut items: Vec<_> = s.items().collect();
    items.sort_by_key(|e| e.extents().next().map(|p| p.range().offset()));
    for e in items {
        out.push_str(&format!("item {:?}", e.info()));
        for p in e.extents() {
            out.push_str(&format!(" {:?}:{:?}", p.segment(), p.range()));
        }
        out.push('\n');
    }
    out
}

/// The segments follow one another from the first byte to the last, and an
/// item's extents lie in their segments.
fn tiles(s: &Structure, len: u64) -> std::result::Result<(), String> {
    let mut at = 0u64;
    for (i, seg) in s.segments().iter().enumerate() {
        let e = seg.range();
        if e.offset() != at {
            return Err(format!("segment {i} starts at {}, not {at}", e.offset()));
        }
        at = e.end();
    }
    if at != len || s.total_size() != len {
        return Err(format!(
            "segments end at {at} and the structure says {} for a stream of {len}",
            s.total_size()
        ));
    }
    for e in s.items() {
        for p in e.extents() {
            let seg = s.segment(p.segment()).map_err(|e| e.to_string())?;
            let (r, x) = (p.range(), seg.range());
            if r.offset() < x.offset() || r.end() > x.end() {
                return Err(format!(
                    "{} item has an extent outside its segment",
                    e.kind()
                ));
            }
        }
    }
    Ok(())
}

/// Parse with the reads answered by hand, each exactly as large as asked, which
/// shows a handler that depends on how much a read returns.
fn parse_by_hand(
    handler: &Arc<dyn ContainerHandler>,
    sample: &[u8],
    want: &Structure,
) -> std::result::Result<(), String> {
    let h = handler.clone();
    let mut task = Task::new(sample.len() as u64, |io| h.parse_async(io));
    let got = loop {
        match task
            .resume()
            .map_err(|e| format!("parse with reads answered by hand: {e}"))?
        {
            Progress::Read(mut req) => {
                let from = (req.offset() as usize).min(sample.len());
                let to = (from + req.min_len()).min(sample.len());
                req.buffer().extend_from_slice(&sample[from..to]);
            }
            Progress::Done(s) => break s,
        }
    };
    if describe(&got) != describe(want) {
        return Err("parsing with reads answered by hand gives another structure".into());
    }
    Ok(())
}

/// Carry out a plan through the stepper, answering its reads from `source`.
fn step_by_step(plan: &WritePlan<'_>, source: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut run = plan.start();
    let mut out = Vec::new();
    loop {
        match run
            .step(&mut crate::NullSink)
            .map_err(|e| format!("stepper: {e}"))?
        {
            Action::Read { offset, buf } => {
                let at = offset as usize;
                let src = source
                    .get(at..at + buf.len())
                    .ok_or("the plan reads past the source")?;
                buf.copy_from_slice(src);
            }
            Action::Write(b) => out.extend_from_slice(b),
            Action::Done => return Ok(out),
        }
    }
}

/// A sink that wants only headers and trailers.
#[derive(Default)]
struct Pick(Vec<u8>);

impl Sink for Pick {
    fn wants(&self, _: &crate::segment::Segment, role: Role) -> bool {
        role != Role::Payload
    }
    fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
        self.0.extend_from_slice(c.data);
        Ok(())
    }
}

/// A sink that skips runs still gets the rest, which are the bytes a walk over the
/// finished structure finds: two independent routes to the same hash.
fn selective_sink(
    s: &Structure,
    sample: &[u8],
    updates: &Updates,
    out: &Structure,
    bytes: &[u8],
) -> std::result::Result<(), String> {
    let mut pick = Pick::default();
    s.write(
        &mut Cursor::new(sample),
        &mut Vec::new(),
        updates,
        &mut pick,
    )
    .map_err(|e| format!("write with a selective sink: {e}"))?;
    let mut walked = Vec::new();
    for r in out.regions().filter(|r| r.role != Role::Payload) {
        let (from, to) = (r.range.offset() as usize, r.range.end() as usize);
        walked.extend_from_slice(
            bytes
                .get(from..to)
                .ok_or("a region lies outside the output")?,
        );
    }
    if pick.0 != walked {
        return Err(
            "a sink that skips payloads did not get the headers and trailers a walk finds".into(),
        );
    }
    Ok(())
}

/// A sink that keeps what it is given.
#[derive(Default)]
struct Collect {
    chunks: Vec<(usize, Role, u64, usize)>,
    data: Vec<u8>,
}

impl Sink for Collect {
    fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
        self.chunks
            .push((c.segment.index(), c.role, c.offset, c.data.len()));
        self.data.extend_from_slice(c.data);
        Ok(())
    }
}

impl Collect {
    /// Every output byte arrived once, in order, in the segment and role the
    /// structure gives it.
    fn check(&self, out: &Structure, bytes: &[u8]) -> std::result::Result<(), String> {
        if self.data != bytes {
            return Err("the sink did not see the bytes that were written".into());
        }
        let mut at = 0u64;
        for &(seg, role, offset, len) in &self.chunks {
            if offset != at {
                return Err(format!("the sink got a chunk at {offset}, expecting {at}"));
            }
            let s = out
                .segments()
                .get(seg)
                .ok_or("the sink got a chunk of a segment that does not exist")?;
            let region = match role {
                Role::Header => s.header(),
                Role::Payload => s.payload(),
                _ => s.trailer(),
            };
            if offset < region.offset() || offset + len as u64 > region.end() {
                return Err(format!("the sink got bytes at {offset} for {role:?} of segment {seg}, which is elsewhere"));
            }
            at += len as u64;
        }
        if at != out.total_size() {
            return Err("the sink did not see the whole output".into());
        }
        Ok(())
    }
}

/// Write through the handler, for the unit tests of the built-in handlers.
#[cfg(test)]
pub(crate) fn write<R, W, S>(
    s: &Structure,
    src: &mut R,
    out: &mut W,
    updates: &Updates,
    sink: &mut S,
) -> Result<Structure>
where
    R: std::io::Read + std::io::Seek,
    W: std::io::Write,
    S: Sink,
{
    s.write(src, out, updates, sink)
}
