//! Generic JUMBF (ISO/IEC 19566-5) box handling.
//!
//! Just enough to split a superbox's children: boxes are `LBox TBox [XLBox]`, and a
//! superbox (`jumb`) starts with a description box (`jumd`, whose first 16 bytes
//! are the superbox type) followed by its children. This module attaches no
//! meaning to any type or label.
//!
//! [`addition_to`] and [`extend`] convert between a store and the original-plus-update
//! pair ISO BMFF keeps; [`Structure::store`](crate::Structure::store) uses them.

use crate::error::{Error, Result};
use std::ops::Range;

/// Most children looked at in one superbox.
const MAX_CHILDREN: usize = 1 << 16;

const JUMB: [u8; 4] = *b"jumb";
const JUMD: [u8; 4] = *b"jumd";

/// The 16-byte type of the JUMBF superbox at the start of `b`, from its description
/// box: `LBox "jumb" LBox "jumd" type[16]`. `None` if `b` is shorter than 32 bytes
/// or does not begin that way. Boxes with an extended length are not interpreted.
pub fn superbox_type(b: &[u8]) -> Option<[u8; 16]> {
    if b.len() < 32 || b[4..8] != JUMB || b[12..16] != JUMD {
        return None;
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&b[16..32]);
    Some(uuid)
}

fn bad(reason: &str) -> Error {
    Error::InvalidFormat(format!("JUMBF: {reason}"))
}

#[derive(Debug, Clone, Copy)]
struct BoxRef {
    start: usize,
    content: usize,
    end: usize,
    ty: [u8; 4],
}

/// The box at `at` inside `b[..end]`. A box larger than its container is an error.
fn read_box(b: &[u8], at: usize, end: usize) -> Result<BoxRef> {
    let avail = end.checked_sub(at).ok_or_else(|| bad("box past end"))?;
    if avail < 8 {
        return Err(bad("truncated box header"));
    }
    let lbox = u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let ty = [b[at + 4], b[at + 5], b[at + 6], b[at + 7]];
    let (size, header) = match lbox {
        0 => (avail, 8),
        1 => {
            if avail < 16 {
                return Err(bad("truncated extended box header"));
            }
            let mut x = [0u8; 8];
            x.copy_from_slice(&b[at + 8..at + 16]);
            let size = usize::try_from(u64::from_be_bytes(x)).map_err(|_| bad("box too large"))?;
            (size, 16)
        }
        n => (n as usize, 8),
    };
    if size < header || size > avail {
        return Err(bad("box size is outside its container"));
    }
    Ok(BoxRef {
        start: at,
        content: at + header,
        end: at + size,
        ty,
    })
}

#[derive(Debug)]
struct Parsed {
    /// Type of the superbox, from its description box.
    uuid: [u8; 16],
    /// The description box, which stays the superbox's own.
    description: Range<usize>,
    children: Vec<Range<usize>>,
}

fn parse(b: &[u8]) -> Result<Parsed> {
    let outer = read_box(b, 0, b.len())?;
    if outer.ty != JUMB || outer.end != b.len() {
        return Err(bad("not a single superbox"));
    }
    let desc = read_box(b, outer.content, outer.end)?;
    if desc.ty != JUMD || desc.end - desc.content < 17 {
        return Err(bad("superbox has no description box"));
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&b[desc.content..desc.content + 16]);

    let mut children = Vec::new();
    let mut at = desc.end;
    while at < outer.end {
        if children.len() >= MAX_CHILDREN {
            return Err(Error::LimitExceeded {
                what: "JUMBF children",
                limit: MAX_CHILDREN as u64,
            });
        }
        let c = read_box(b, at, outer.end)?;
        children.push(c.start..c.end);
        at = c.end;
    }
    Ok(Parsed {
        uuid,
        description: desc.start..desc.end,
        children,
    })
}

/// What `full` adds to `frozen`: a superbox with `full`'s description and the
/// children that follow `frozen`'s.
///
/// Returns `None` unless `frozen`'s children are exactly the first children of
/// `full`, byte for byte, under the same superbox type, and `full` has at least one
/// more. Malformed input is an error.
pub fn addition_to(frozen: &[u8], full: &[u8]) -> Result<Option<Vec<u8>>> {
    let f = parse(frozen)?;
    let n = parse(full)?;
    if f.uuid != n.uuid || n.children.len() <= f.children.len() {
        return Ok(None);
    }
    let same = f
        .children
        .iter()
        .zip(&n.children)
        .all(|(a, b)| frozen[a.clone()] == full[b.clone()]);
    if !same {
        return Ok(None);
    }

    let added = n.children[f.children.len()..]
        .iter()
        .map(|c| &full[c.clone()]);
    Ok(Some(superbox(&full[n.description], added)))
}

/// The inverse of [`addition_to`]: `frozen` extended by the children of `addition`.
///
/// Both must be single superboxes of the same type. The result has `frozen`'s
/// description box and children, then `addition`'s children, which is how a reader
/// sees an original store plus its update. Malformed input, or two different
/// superbox types, is an error.
pub fn extend(frozen: &[u8], addition: &[u8]) -> Result<Vec<u8>> {
    let f = parse(frozen)?;
    let a = parse(addition)?;
    if f.uuid != a.uuid {
        return Err(bad(
            "an update must be the same superbox type as its original",
        ));
    }
    let children = f
        .children
        .iter()
        .map(|c| &frozen[c.clone()])
        .chain(a.children.iter().map(|c| &addition[c.clone()]));
    Ok(superbox(&frozen[f.description], children))
}

/// A `jumb` box around `description` and `children`, using an extended length only
/// when the size needs one.
fn superbox<'a>(description: &[u8], children: impl Iterator<Item = &'a [u8]> + Clone) -> Vec<u8> {
    let body_len = description.len() + children.clone().map(<[u8]>::len).sum::<usize>();
    let mut out = Vec::with_capacity(body_len + 16);
    match u32::try_from(body_len + 8) {
        Ok(size) => {
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(&JUMB);
        }
        Err(_) => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&JUMB);
            out.extend_from_slice(&(body_len as u64 + 16).to_be_bytes());
        }
    }
    out.extend_from_slice(description);
    for c in children {
        out.extend_from_slice(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: [u8; 16] = [7; 16];
    const OTHER: [u8; 16] = [9; 16];

    fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(body);
        v
    }

    fn superbox(uuid: &[u8; 16], children: &[Vec<u8>]) -> Vec<u8> {
        let mut d = uuid.to_vec();
        d.push(3);
        d.extend_from_slice(b"c2pa\0");
        let mut body = boxed(b"jumd", &d);
        for c in children {
            body.extend_from_slice(c);
        }
        boxed(b"jumb", &body)
    }

    fn manifest(data: &[u8]) -> Vec<u8> {
        superbox(&[1; 16], &[boxed(b"json", data)])
    }

    #[test]
    fn the_children_after_the_frozen_ones_are_the_addition() {
        let (m1, m2, m3) = (manifest(b"1"), manifest(b"2"), manifest(b"3"));
        let frozen = superbox(&STORE, &[m1.clone()]);

        let one = addition_to(&frozen, &superbox(&STORE, &[m1.clone(), m2.clone()])).unwrap();
        assert_eq!(one.unwrap(), superbox(&STORE, &[m2.clone()]));

        let two = addition_to(&frozen, &superbox(&STORE, &[m1, m2.clone(), m3.clone()])).unwrap();
        assert_eq!(two.unwrap(), superbox(&STORE, &[m2, m3]));
    }

    #[test]
    fn extend_undoes_addition_to() {
        let (m1, m2, m3) = (manifest(b"1"), manifest(b"2"), manifest(b"3"));
        let frozen = superbox(&STORE, &[m1.clone()]);
        let full = superbox(&STORE, &[m1, m2, m3]);
        let add = addition_to(&frozen, &full).unwrap().unwrap();
        assert_eq!(extend(&frozen, &add).unwrap(), full);
        // Extending by nothing but a description keeps the store.
        let empty = superbox(&STORE, &[]);
        assert_eq!(extend(&frozen, &empty).unwrap(), frozen);
        // A different superbox type is not an extension.
        assert!(extend(&frozen, &superbox(&OTHER, &[manifest(b"x")])).is_err());
        assert!(extend(&frozen, &frozen[..frozen.len() - 1]).is_err());
    }

    #[test]
    fn anything_but_a_pure_extension_is_none() {
        let (m1, m2) = (manifest(b"1"), manifest(b"2"));
        let frozen = superbox(&STORE, &[m1.clone(), m2.clone()]);
        // Nothing added, fewer children, a changed child, another type.
        assert!(addition_to(&frozen, &frozen).unwrap().is_none());
        assert!(addition_to(&frozen, &superbox(&STORE, &[m1.clone()]))
            .unwrap()
            .is_none());
        let changed = superbox(&STORE, &[m1.clone(), manifest(b"X"), manifest(b"3")]);
        assert!(addition_to(&frozen, &changed).unwrap().is_none());
        let other = superbox(&OTHER, &[m1, m2, manifest(b"3")]);
        assert!(addition_to(&frozen, &other).unwrap().is_none());
    }

    #[test]
    fn malformed_stores_are_errors() {
        let store = superbox(&STORE, &[manifest(b"x")]);
        let longer = superbox(&STORE, &[manifest(b"x"), manifest(b"y")]);
        assert!(addition_to(&store, &longer[..longer.len() - 1]).is_err());
        assert!(addition_to(&store[..store.len() - 1], &longer).is_err());
        let mut trailing = longer.clone();
        trailing.push(0);
        assert!(addition_to(&store, &trailing).is_err());
        assert!(addition_to(&store, &boxed(b"free", b"")).is_err());
        assert!(addition_to(&store, &[]).is_err());
        let mut huge = longer.clone();
        huge[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(addition_to(&store, &huge).is_err());
        // A corrupted byte anywhere must give an error or a result, never a panic.
        for i in 0..longer.len() {
            let mut c = longer.clone();
            c[i] ^= 0xFF;
            let _ = addition_to(&store, &c);
        }
    }
}
