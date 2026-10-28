//! The file format.
//!
//! ```text
//! magic(69 04 7f 82 2b 43, base64 aQR/gitD) u16-LE(version 1)
//! per stream (NSTREAMS), in order: varint(n << 1 | z) [varint(raw length) if z] n bytes (zstd if z)
//! varint(symlinks) per symlink: varint(len) path varint(len) target
//! per file (in path order): u64-LE(xxh3 of the file)
//! ```

use crate::rd::{Rd, put_uv};
use crate::{Archive, Deconstructed, Error, NSTREAMS, Result, Streams, corrupt};
use xxhash_rust::xxh3::xxh3_64;

const MAGIC: [u8; 6] = [0x69, 0x04, 0x7f, 0x82, 0x2b, 0x43];
const VERSION: u16 = 1;
/// The largest output of a zstd byte (a 4-byte RLE block makes 128 KiB).
const ZSTD_MAX_RATIO: usize = 1 << 15;

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: i32,
    pub ldm: bool,
    /// zstd workers (the output is the same for any number from 1 up).
    pub threads: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 19, ldm: true, threads: std::thread::available_parallelism().map_or(1, |n| n.get() as u32) }
    }
}

/// Compressed size per stream.
pub fn compress(d: &Deconstructed, opt: &Options, coded: &mut [u64; NSTREAMS]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&VERSION.to_le_bytes());
    // Each stream on a thread of its own (zstd's workers aside).
    let zs: Vec<Option<Vec<u8>>> = std::thread::scope(|sc| {
        let hs: Vec<_> = d
            .streams
            .iter()
            .map(|s| {
                sc.spawn(move || {
                    (s.len() > 64).then(|| {
                        let mut cz = zstd::bulk::Compressor::new(opt.level).expect("zstd");
                        cz.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(opt.threads.max(1))).expect("zstd");
                        if opt.ldm && s.len() > 2 << 20 {
                            use zstd::zstd_safe::CParameter as P;
                            cz.set_parameter(P::EnableLongDistanceMatching(true)).expect("zstd");
                            cz.set_parameter(P::WindowLog(27)).expect("zstd");
                        }
                        cz.compress(s).expect("zstd")
                    })
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (i, (s, z)) in d.streams.iter().zip(zs).enumerate() {
        let at = out.len();
        match z.filter(|z| z.len() < s.len()) {
            Some(z) => {
                put_uv(&mut out, (z.len() as u64) << 1 | 1);
                put_uv(&mut out, s.len() as u64);
                out.extend_from_slice(&z);
            }
            None => {
                put_uv(&mut out, (s.len() as u64) << 1);
                out.extend_from_slice(s);
            }
        }
        coded[i] += (out.len() - at) as u64;
    }
    put_uv(&mut out, d.symlinks.len() as u64);
    for (p, t) in &d.symlinks {
        put_uv(&mut out, p.len() as u64);
        out.extend_from_slice(p.as_bytes());
        put_uv(&mut out, t.len() as u64);
        out.extend_from_slice(t);
    }
    for h in &d.hashes {
        out.extend_from_slice(&h.to_le_bytes());
    }
    out
}

/// The streams, symlinks and file hashes of a container.
pub struct Unpacked {
    pub streams: Streams,
    pub symlinks: Vec<(String, Vec<u8>)>,
    pub hashes: Vec<u64>,
}

pub fn unpack(src: &[u8]) -> Result<Unpacked> {
    let mut r = Rd::new(src);
    if r.bytes(6).ok() != Some(&MAGIC[..]) {
        return Err(Error("not a GITR file"));
    }
    if u16::from_le_bytes(r.bytes(2)?.try_into().unwrap()) != VERSION {
        return Err(Error("unknown version"));
    }
    let mut streams: Streams = Default::default();
    let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
    dz.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(27)).map_err(|_| corrupt())?;
    for s in &mut streams {
        let v = r.uv()?;
        let c = usize::try_from(v >> 1).map_err(|_| corrupt())?;
        if v & 1 != 0 {
            let n = r.usize()?;
            if n > c.saturating_mul(ZSTD_MAX_RATIO) {
                return Err(corrupt());
            }
            let z = r.bytes(c)?;
            s.reserve_exact(n);
            dz.decompress_to_buffer(z, s).map_err(|_| corrupt())?;
            if s.len() != n {
                return Err(corrupt());
            }
        } else {
            s.extend_from_slice(r.bytes(c)?);
        }
    }
    let mut symlinks = vec![];
    for _ in 0..r.uv()? {
        let n = r.usize()?;
        let p = String::from_utf8(r.bytes(n)?.to_vec()).map_err(|_| corrupt())?;
        let n = r.usize()?;
        symlinks.push((p, r.bytes(n)?.to_vec()));
    }
    let rest = &src[r.p..];
    if !rest.len().is_multiple_of(8) {
        return Err(corrupt());
    }
    let hashes = rest.as_chunks::<8>().0.iter().map(|&c| u64::from_le_bytes(c)).collect();
    Ok(Unpacked { streams, symlinks, hashes })
}

/// Every file of a container, checked against its hash.
pub fn for_each(u: &Unpacked, mut f: impl FnMut(&str, Vec<u8>) -> Result<()>) -> Result<()> {
    let a = Archive::open(&u.streams.each_ref().map(|s| &s[..]))?;
    if a.len() != u.hashes.len() {
        return Err(corrupt());
    }
    let mut i = 0;
    a.for_each(|p, b| {
        if xxh3_64(&b) != u.hashes[i] {
            return Err(Error("checksum mismatch"));
        }
        i += 1;
        f(p, b)
    })
}
