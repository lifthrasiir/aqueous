//! The code of the modules `bz2-recons` and `bz2-alz-recons` (see `formula`, and `FORMAT.md`),
//! which needs no aqr-formula, so that aqr-modules builds them into their Wasm modules.

use aqr_common::varint;

use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};
use aqr_common::msb::{BitReader, BitWriter};
use aqr_common::pool;

use crate::compress;
use crate::crc;
use crate::{ALZ_LEVEL, Error, Framing, Result, block_max, corrupt};

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

    /// [`Params::orig_ptr`] while `left` of the stream's blocks given a rank remain, else None.
    fn next_given(&mut self, left: &mut u64) -> Result<Option<(usize, u32)>> {
        if *left == 0 {
            return Ok(None);
        }
        *left -= 1;
        self.orig_ptr().map(Some)
    }
}

/// Pages of the windows of the module as a part module ([`decode_part`]): of the input and of
/// the output.
pub const WINDOW: u32 = 4;

/// Bytes of the pieces in which the data is taken, and of the buffer of the stream written
/// before it is handed on: a block's header, selectors and tables take 21 KB at most
/// (`entropy`), on top of the [`SPILL`] bytes that are held before they are handed on.
const PIECE: usize = 64 << 10;
const OUT: usize = 32 << 10;
const SPILL: usize = 4 << 10;

/// The memory plan of a module of `framing` as a part module ([`aqr_api::stack`]): what
/// [`decode_part`] takes beyond its initial memory (`initial` bytes) at most, of params
/// `params` and the data `data` (whose blocks' symbols it reads; `None` where they are not so):
/// the params copied, a piece of the data, the buffer of the stream, then a block compressed at
/// a time ([`compress::block_plan`] of the most symbols of a block). Nothing of the lengths is
/// held but the params and the largest block.
pub fn plan(framing: Framing, initial: u64, params: &[u8], data: &[u8]) -> Option<u64> {
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
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(params.len() as u64);
    m.take(PIECE as u64);
    m.take(OUT as u64);
    if most > 0 {
        m.peak(compress::block_plan(most));
    }
    Some(m.plan())
}

/// The runs of equal bytes (255 at most) of one stream's data, taken from `r` in order.
struct Runs<'a, 'b> {
    r: &'b mut Reader<'a>,
    /// The byte read past the last run, which starts the next.
    pending: Option<u8>,
    /// Bytes of the stream not read yet.
    left: u64,
}

impl Runs<'_, '_> {
    /// The next run: its byte and length.
    fn next(&mut self) -> Result<Option<(u8, usize)>> {
        let short = || Error("data of another length");
        let c = match self.pending.take() {
            Some(c) => c,
            None if self.left == 0 => return Ok(None),
            None => {
                self.left -= 1;
                self.r.byte().ok_or_else(short)?
            }
        };
        let mut n = 1;
        while n < 255 && self.left > 0 {
            self.left -= 1;
            match self.r.byte().ok_or_else(short)? {
                b if b == c => n += 1,
                b => {
                    self.pending = Some(b);
                    break;
                }
            }
        }
        Ok(Some((c, n)))
    }

    /// Bytes of the stream past the run just taken.
    fn rest(&self) -> u64 {
        self.left + self.pending.is_some() as u64
    }
}

/// Puts the bytes the writer holds to `out` where there are [`SPILL`] of them (or any, where
/// `all`).
fn spill(w: &mut BitWriter, out: &mut dyn Put, all: bool) -> Result<()> {
    if all || w.out.len() >= SPILL {
        out.put(&w.out).ok_or(Error("output of another length"))?;
        w.out.clear();
    }
    Ok(())
}

/// The module on the data pulled from `data`, put to `out`: [`decode`] as the data comes and the
/// stream goes, a block of RLE1 output at a time. The output may be of any length; the caller
/// checks it.
pub fn run(framing: Framing, params: &[u8], data: &mut dyn Pull, out: &mut dyn Put) -> Result<()> {
    let mut p = Params::new(params, framing)?;
    let mut rd = Reader::new(data, PIECE);
    let mut w = BitWriter::with_capacity(OUT);
    // (the block's RLE1 output: it grows on top of the stack, then is written twice onto itself)
    let mut rle: Vec<u8> = Vec::new();
    while let Some((level, n, k)) = p.stream()? {
        compress::put_start(&mut w, framing, level);
        let max = block_max(level);
        let mut left = k;
        let mut given = p.next_given(&mut left)?;
        let (mut combined, mut index) = (0u32, 0usize);
        let mut runs = Runs { r: &mut rd, pending: None, left: n };
        let (mut crc, mut syms) = (!0u32, 0usize);
        // the block in `rle` written, with the rank the params give it
        let mut flush = |rle: &mut Vec<u8>, crc: u32, w: &mut BitWriter, p: &mut Params| -> Result<()> {
            let rank = match given {
                Some((g, r)) if g == index => {
                    given = p.next_given(&mut left)?;
                    Some(r)
                }
                _ => None,
            };
            index += 1;
            let (m, crc) = (rle.len(), !crc);
            rle.reserve_exact(m);
            compress::write_rle1(rle, crc, framing, rank, w, &mut |w| spill(w, out, false))?;
            combined = combined.rotate_left(1) ^ crc;
            rle.clear();
            Ok(())
        };
        while let Some((c, r)) = runs.next()? {
            // a block full by the runs before this one ends here, unless this is the last byte
            if syms >= max && r as u64 + runs.rest() > 1 {
                flush(&mut rle, crc, &mut w, &mut p)?;
                (crc, syms) = (!0, 0);
            }
            if rle.capacity() - rle.len() < 5 {
                rle.reserve_exact(rle.len().max(16));
            }
            if r < 4 {
                rle.extend(core::iter::repeat_n(c, r));
            } else {
                rle.extend_from_slice(&[c, c, c, c, (r - 4) as u8]);
            }
            crc = crc::update_run(crc, c, r);
            syms += if r < 4 { r } else { 5 };
        }
        if !rle.is_empty() {
            flush(&mut rle, crc, &mut w, &mut p)?;
        }
        if given.is_some() {
            return Err(corrupt());
        }
        compress::put_end(&mut w, framing, combined);
        w.put(0, (8 - w.len() % 8) as u32 % 8);
        w.flush_aligned();
        spill(&mut w, out, true)?;
    }
    if rd.done() { Ok(()) } else { Err(Error("data of another length")) }
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its input taken and its output put in order through windows ([`WINDOW`]) in pieces, a block
/// of RLE1 output held at a time.
pub fn decode_part(params_len: u64, in_len: u64, size: u64) {
    aqr_api::finish(part(Framing::Bzip2, params_len, in_len, size));
}

/// [`decode_part`] of `bz2-alz-recons`.
pub fn decode_alz_part(params_len: u64, in_len: u64, size: u64) {
    aqr_api::finish(part(Framing::Alz, params_len, in_len, size));
}

fn part(framing: Framing, params_len: u64, in_len: u64, size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut params = vec![0; usize::try_from(params_len).map_err(|_| corrupt())?];
    aqr_api::Params::read(0, &mut params);
    let mut data = In::<0>::new(in_len);
    let mut out = Written::new(size);
    run(framing, &params, &mut data, &mut out)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}

/// [`decode_part`] (or [`decode_alz_part`], of `framing`) on a whole input, natively: what a part
/// module gives.
pub fn decode_streamed(framing: Framing, params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[data] = inputs else { return Err(Error("one input wanted")) };
    let mut out = Vec::new();
    run(framing, params, &mut Slice(data), &mut out)?;
    match out.len() as u64 == len {
        true => Ok(out),
        false => Err(Error("output of another length")),
    }
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
        compress::put_start(&mut w, framing, level);
        let mut left = k;
        let mut given = r.next_given(&mut left)?;
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
                    given = r.next_given(&mut left)?;
                }
                w.copy(&mut BitReader::new(&blk.bits, 0), blk.nbits);
                one(&w, blk.crc)?;
            }
        } else {
            let mut k = 0;
            compress::each_block(d, level, &mut |s, e, _| {
                let rank = match given {
                    Some((g, p)) if g == k => {
                        given = r.next_given(&mut left)?;
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
        compress::put_end(&mut w, framing, combined);
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
