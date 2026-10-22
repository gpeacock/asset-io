//! Experiment: hash the fragments of a fragmented MP4 in parallel.
//!
//! A fragmented BMFF file is an initialization part (`ftyp`, `moov`, ...) followed
//! by many independent fragments, each a `moof` and its `mdat`. A C2PA BMFF hash
//! (v2) lists one hash per fragment, so the fragments can be hashed on separate
//! threads and the digests joined in file order. The library itself stays free of
//! hashing; this example only reads the [`Structure`] geometry.
//!
//! Run: `cargo run --release --example parallel_hash --features all-formats -- [file] [threads]`
//!
//! With no file, a synthetic fragmented MP4 is generated. Test: `cargo test --example
//! parallel_hash --features all-formats`.

use asset_io::{HandlerRegistry, Name, Structure};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::File,
    io,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

type Digest32 = [u8; 32];

/// Bytes hashed per read, per worker.
const BUF: usize = 1 << 20;

/// Random-access reads that do not share a cursor, so workers need no lock.
trait ReadAt: Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

impl ReadAt for Vec<u8> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
        let src = self
            .as_slice()
            .get(start..)
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        let n = src.len().min(buf.len());
        buf[..n].copy_from_slice(&src[..n]);
        Ok(n)
    }
}

impl ReadAt for File {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        return std::os::unix::fs::FileExt::read_at(self, buf, offset);
        #[cfg(windows)]
        return std::os::windows::fs::FileExt::seek_read(self, buf, offset);
    }
}

/// One independently hashed run of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unit {
    offset: u64,
    len: u64,
    /// True for a `moof` and the `mdat`s that follow it.
    fragment: bool,
}

/// Split the file into hash units that tile it exactly: each `moof` with the
/// `mdat`s after it is a fragment, and the bytes between fragments (the
/// initialization part, `styp`, `sidx`, a trailing `mfra`) are one plain unit each.
fn units(structure: &Structure) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    let mut open = false; // the last unit is a fragment that `mdat`s still extend
    for seg in structure.segments() {
        let ext = seg.extent();
        let name = seg.name();
        let is_moof = name == Name::FourCC(*b"moof");
        let is_mdat = name == Name::FourCC(*b"mdat");
        let unit = |fragment| Unit {
            offset: ext.offset(),
            len: ext.len(),
            fragment,
        };
        if is_moof {
            out.push(unit(true));
            open = true;
        } else if open && is_mdat {
            out.last_mut().expect("open fragment").len += ext.len();
        } else {
            open = false;
            match out.last_mut() {
                Some(u) if !u.fragment => u.len += ext.len(),
                _ => out.push(unit(false)),
            }
        }
    }
    out
}

fn hash_unit(src: &dyn ReadAt, u: &Unit, buf: &mut [u8]) -> io::Result<Digest32> {
    let mut h = Sha256::new();
    let mut at = u.offset;
    let end = u.offset + u.len;
    while at < end {
        let want = buf.len().min((end - at) as usize);
        let n = src.read_at(at, &mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        h.update(&buf[..n]);
        at += n as u64;
    }
    Ok(h.finalize().into())
}

fn hash_sequential(src: &dyn ReadAt, units: &[Unit]) -> io::Result<Vec<Digest32>> {
    let mut buf = vec![0u8; BUF];
    units.iter().map(|u| hash_unit(src, u, &mut buf)).collect()
}

/// Hash every unit on `threads` workers. Workers take the next unclaimed unit from
/// a shared counter, so a few large fragments do not leave other threads idle.
/// Digests come back in unit order, identical to [`hash_sequential`].
fn hash_parallel(src: &dyn ReadAt, units: &[Unit], threads: usize) -> io::Result<Vec<Digest32>> {
    let next = AtomicUsize::new(0);
    let threads = threads.clamp(1, units.len().max(1));
    let parts: Vec<io::Result<Vec<(usize, Digest32)>>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut buf = vec![0u8; BUF];
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(u) = units.get(i) else {
                            return Ok(done);
                        };
                        done.push((i, hash_unit(src, u, &mut buf)?));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().expect("hash worker panicked"))
            .collect()
    });
    let mut out = vec![[0u8; 32]; units.len()];
    for part in parts {
        for (i, d) in part? {
            out[i] = d;
        }
    }
    Ok(out)
}

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    v
}

/// ftyp, moov, then `fragments` of moof + mdat, then an mfra.
fn synthetic(fragments: usize, mdat_len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut v = bx(b"ftyp", b"isom\0\0\0\0isomiso2");
    v.extend(bx(b"moov", &bx(b"mvhd", &[0; 100])));
    for _ in 0..fragments {
        let mfhd = bx(b"mfhd", &[0; 8]);
        v.extend(bx(b"moof", &mfhd));
        let media: Vec<u8> = (0..mdat_len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        v.extend(bx(b"mdat", &media));
    }
    v.extend(bx(b"mfra", &bx(b"mfro", &[0; 8])));
    v
}

fn timed<T>(label: &str, bytes: u64, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    let s = t.elapsed().as_secs_f64();
    println!(
        "{label:<14} {:>8.1} ms  {:>8.0} MB/s",
        s * 1e3,
        bytes as f64 / s / 1e6
    );
    r
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let path = args.next();
    let max_threads = args
        .next()
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));

    let registry = HandlerRegistry::builtin();
    let (structure, src): (Structure, Box<dyn ReadAt>) = match path {
        Some(p) => {
            let mut f = File::open(&p)?;
            let s = registry.parse_detect(&mut f)?;
            (s, Box::new(f))
        }
        None => {
            let data = synthetic(256, 1 << 20);
            let s = registry.parse("mp4", &mut io::Cursor::new(&data))?;
            (s, Box::new(data))
        }
    };
    let units = units(&structure);
    let fragments = units.iter().filter(|u| u.fragment).count();
    let total = structure.total_size();
    println!(
        "{} bytes, {} units ({fragments} fragments)",
        total,
        units.len()
    );

    let base = timed("sequential", total, || hash_sequential(&*src, &units))?;
    let mut n = 2;
    while n <= max_threads {
        let d = timed(&format!("{n} threads"), total, || {
            hash_parallel(&*src, &units, n)
        })?;
        assert_eq!(d, base, "parallel digests differ from sequential");
        n *= 2;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(data: &[u8]) -> Structure {
        HandlerRegistry::builtin()
            .parse("mp4", &mut io::Cursor::new(data))
            .unwrap()
    }

    #[test]
    fn units_tile_the_file_and_group_fragments() {
        let data = synthetic(5, 1000);
        let units = units(&parse(&data));

        let mut at = 0;
        for u in &units {
            assert_eq!(u.offset, at);
            at += u.len;
        }
        assert_eq!(at, data.len() as u64);

        // init part, five fragments, trailing mfra
        assert_eq!(units.len(), 7);
        assert!(!units[0].fragment && !units[6].fragment);
        assert!(units[1..6].iter().all(|u| u.fragment));
    }

    #[test]
    fn extra_mdats_stay_in_their_fragment() {
        let mut data = synthetic(1, 10);
        let mfra = data.split_off(data.len() - 24);
        data.extend(bx(b"mdat", b"more"));
        data.extend(mfra);
        let units = units(&parse(&data));
        assert_eq!(units.len(), 3);
        assert_eq!(units[1].len, (8 + 16) + (8 + 10) + (8 + 4));
    }

    #[test]
    fn parallel_matches_sequential_and_whole_file() {
        let data = synthetic(40, 3000);
        let units = units(&parse(&data));
        let seq = hash_sequential(&data, &units).unwrap();
        for threads in [1, 2, 3, 8, 100] {
            assert_eq!(hash_parallel(&data, &units, threads).unwrap(), seq);
        }

        // Each digest is the hash of exactly its unit's bytes.
        let u = &units[7];
        let bytes = &data[u.offset as usize..][..u.len as usize];
        assert_eq!(seq[7].as_slice(), Sha256::digest(bytes).as_slice());
    }

    #[test]
    fn read_past_end_is_an_error() {
        let data = vec![0u8; 10];
        let bad = [Unit {
            offset: 0,
            len: 20,
            fragment: false,
        }];
        assert!(hash_parallel(&data, &bad, 2).is_err());
    }
}
