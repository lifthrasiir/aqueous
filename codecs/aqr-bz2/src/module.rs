//! The code of the modules `bz2-recons` and `bz2-alz-recons` (see `formula`, and `FORMAT.md`),
//! which needs no aqr-formula, so that aqr-modules builds them into their Wasm modules.

use aqr_common::varint;

use aqr_common::msb::{BitReader, BitWriter};
use aqr_common::pool;

use crate::compress;
use crate::{ALZ_END_MAGIC, ALZ_LEVEL, END_MAGIC, Error, Framing, MAGIC, Result, corrupt};

/// The parameters of one stream: its level, bytes of data, and the blocks whose `origPtr`
/// differs from the one compressing gives (index, `origPtr`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamParams {
    pub level: u8,
    pub len: u64,
    pub orig_ptrs: Vec<(usize, u32)>,
}

/// The params of `bz2-recons`.
pub fn put_params(out: &mut Vec<u8>, streams: &[StreamParams]) {
    put_framed(out, Framing::Bzip2, streams)
}

/// The params of a module of `framing`: those of [`Framing::Alz`] (`bz2-alz-recons`) have no
/// level, which is [`ALZ_LEVEL`].
pub fn put_framed(out: &mut Vec<u8>, framing: Framing, streams: &[StreamParams]) {
    varint::put(out, streams.len() as u64);
    for s in streams {
        match framing {
            Framing::Bzip2 => out.push(s.level),
            Framing::Alz => debug_assert_eq!(s.level, ALZ_LEVEL),
        }
        varint::put(out, s.len);
        varint::put(out, s.orig_ptrs.len() as u64);
        let mut next = 0;
        for &(b, p) in &s.orig_ptrs {
            varint::put(out, (b - next) as u64);
            varint::put(out, p as u64);
            next = b + 1;
        }
    }
}

/// The params of `bz2-recons` read.
pub fn get_params(p: &[u8]) -> Result<Vec<StreamParams>> {
    get_framed(p, Framing::Bzip2)
}

/// The params of a module of `framing` read ([`put_framed`]).
pub fn get_framed(p: &[u8], framing: Framing) -> Result<Vec<StreamParams>> {
    let mut r = Params::new(p, framing)?;
    let mut streams = Vec::with_capacity(r.left as usize);
    while let Some((level, len, k)) = r.stream()? {
        let orig_ptrs = (0..k).map(|_| r.orig_ptr()).collect::<Result<_>>()?;
        streams.push(StreamParams { level, len, orig_ptrs });
    }
    Ok(streams)
}

/// A reader of the params ([`put_framed`]), taken as they are read: for each stream
/// [`Params::stream`], then [`Params::orig_ptr`] as many times as it tells.
pub struct Params<'a> {
    p: &'a [u8],
    at: usize,
    framing: Framing,
    /// Streams left to read.
    left: u64,
    /// The index the next block given may take, at least.
    next: u64,
}

impl<'a> Params<'a> {
    pub fn new(p: &'a [u8], framing: Framing) -> Result<Self> {
        let mut at = 0;
        let left = varint::get(p, &mut at).map_err(|_| corrupt())?;
        if left == 0 || left > p.len() as u64 {
            return Err(corrupt());
        }
        Ok(Params { p, at, framing, left, next: 0 })
    }

    fn get(&mut self) -> Result<u64> {
        varint::get(self.p, &mut self.at).map_err(|_| corrupt())
    }

    /// The next stream's level, bytes of data and blocks given a rank; None after the last
    /// (where nothing else follows).
    pub fn stream(&mut self) -> Result<Option<(u8, u64, u64)>> {
        if self.left == 0 {
            return if self.at == self.p.len() { Ok(None) } else { Err(corrupt()) };
        }
        self.left -= 1;
        let level = match self.framing {
            Framing::Bzip2 => {
                self.at += 1;
                *self.p.get(self.at - 1).ok_or_else(corrupt)?
            }
            Framing::Alz => ALZ_LEVEL,
        };
        if !(1..=9).contains(&level) {
            return Err(corrupt());
        }
        let len = self.get()?;
        let k = self.get()?;
        if k > self.p.len() as u64 {
            return Err(corrupt());
        }
        self.next = 0;
        Ok(Some((level, len, k)))
    }

    /// The next block of the stream given a rank: its index and the rank.
    pub fn orig_ptr(&mut self) -> Result<(usize, u32)> {
        let b = self.next.checked_add(self.get()?).filter(|&b| b < u32::MAX as u64).ok_or_else(corrupt)?;
        let o = self.get()?;
        if o >= 1 << 24 {
            return Err(corrupt());
        }
        self.next = b + 1;
        Ok((b as usize, o as u32))
    }
}

/// The memory plan of a module of `framing` ([`aqr_api::stack`]): what it takes beyond its
/// initial memory at most, of params `params`, the data `data` and an output of `len` bytes
/// (`None` where they are not so). What `aqr_api::whole` copies; the output, reserved first;
/// then a block compressed at a time onto it, of [`compress::block_plan`] of the most symbols
/// of a block.
pub fn plan(framing: Framing, params: &[u8], data: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let mut r = Params::new(params, framing).ok()?;
    let (mut at, mut most) = (0usize, 0);
    while let Some((level, n, k)) = r.stream().ok()? {
        (0..k).try_for_each(|_| r.orig_ptr().map(drop)).ok()?;
        let d = data.get(at..at.checked_add(usize::try_from(n).ok()?)?)?;
        at += d.len();
        compress::each_block(d, level, &mut |_, _, syms| {
            most = most.max(syms);
            Ok(())
        })
        .ok()?;
    }
    let own = block(len) + if most > 0 { compress::block_plan(most) } else { 0 };
    Some(whole(params.len() as u64, &[], &[data.len() as u64]) + own)
}

/// The streams of `data`, one after another, compressed as `streams` says.
pub fn compress(data: &[u8], streams: &[StreamParams], threads: usize) -> Result<Vec<u8>> {
    let mut params = Vec::new();
    put_params(&mut params, streams);
    compress_framed(data, Framing::Bzip2, &params, usize::MAX, threads)
}

/// The streams of `data` in `framing`, one after another, compressed as the params `p` say, onto
/// an output of `cap` bytes at most (reserved first, where it is not `usize::MAX`). Natively
/// on `threads` threads, each a block at a time; on one, each block compressed onto the output.
pub fn compress_framed(data: &[u8], framing: Framing, p: &[u8], cap: usize, threads: usize) -> Result<Vec<u8>> {
    let mut r = Params::new(p, framing)?;
    let mut w = BitWriter::with_capacity(if cap == usize::MAX { 0 } else { cap });
    let mut at = 0usize;
    while let Some((level, n, k)) = r.stream()? {
        let d = data.get(at..at.checked_add(usize::try_from(n).map_err(|_| corrupt())?).ok_or_else(corrupt)?);
        let d = d.ok_or(Error("data of another length"))?;
        at += d.len();
        if framing == Framing::Bzip2 {
            MAGIC.iter().for_each(|&c| w.put(c as u32, 8));
            w.put((b'0' + level) as u32, 8);
        }
        let mut left = k;
        let mut next_given = |r: &mut Params| -> Result<Option<(usize, u32)>> {
            if left == 0 {
                return Ok(None);
            }
            left -= 1;
            r.orig_ptr().map(Some)
        };
        let mut given = next_given(&mut r)?;
        let mut combined = 0u32;
        // (the output past its bytes at most by a block, before it fails)
        let mut one = |w: &BitWriter, crc: u32| {
            combined = combined.rotate_left(1) ^ crc;
            if w.out.len() > cap { Err(Error("output of another length")) } else { Ok(()) }
        };
        if threads > 1 {
            let mut cuts = compress::boundaries(d, level);
            let m = cuts.len();
            cuts.push(d.len());
            let mut blocks = pool::par_map(m, threads, |k| compress::block_framed(&d[cuts[k]..cuts[k + 1]], framing));
            for (k, blk) in blocks.iter_mut().enumerate() {
                if let Some((_, p)) = given.filter(|g| g.0 == k) {
                    if p as usize >= blk.syms {
                        return Err(corrupt());
                    }
                    blk.set_orig_ptr(p);
                    given = next_given(&mut r)?;
                }
                w.copy(&mut BitReader::new(&blk.bits, 0), blk.nbits);
                one(&w, blk.crc)?;
            }
        } else {
            let mut k = 0;
            compress::each_block(d, level, &mut |s, e, _| {
                let rank = match given {
                    Some((g, p)) if g == k => {
                        given = next_given(&mut r)?;
                        Some(p)
                    }
                    _ => None,
                };
                k += 1;
                let (crc, _, _) = compress::write_block(&d[s..e], framing, rank, &mut w)?;
                one(&w, crc)
            })?;
        }
        if given.is_some() {
            return Err(corrupt());
        }
        match framing {
            Framing::Bzip2 => {
                w.put((END_MAGIC >> 24) as u32, 24);
                w.put((END_MAGIC & 0xFF_FFFF) as u32, 24);
                w.put(combined, 32);
            }
            Framing::Alz => w.put(ALZ_END_MAGIC, 32),
        }
        w.put(0, (8 - w.len() % 8) as u32 % 8);
    }
    if at != data.len() {
        return Err(Error("data of another length"));
    }
    Ok(w.finish())
}

/// The module: the bzip2 streams of `len` bytes that the parameters make of the single input
/// (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_framed(Framing::Bzip2, params, inputs, len, threads)
}

/// The module `bz2-alz-recons`: [`decode`] of ALZip's streams.
pub fn decode_alz(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_framed(Framing::Alz, params, inputs, len, threads)
}

fn decode_framed(framing: Framing, params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data] = inputs else { return Err(Error("one input wanted")) };
    let cap = usize::try_from(len).map_err(|_| corrupt())?;
    let out = compress_framed(data, framing, params, cap, threads)?;
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
