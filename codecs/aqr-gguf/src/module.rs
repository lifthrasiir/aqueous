//! The code of the module `gguf-recons` (FORMAT.md §5), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module. It is a part module: its three inputs are read and
//! its output written in order through windows, a chunk of a run at a time, and nothing of their
//! lengths is held.

use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};

use crate::layout::{self, FLOAT_CHUNK, Float, FloatRun, MAX_CHUNK_RAW, MAX_CHUNK_SIDE, MAX_CHUNK_SYMS, Run};
use crate::rans::{Decoder, HEAD, Kernel, LANES};
use crate::{Error, Result, corrupt};

/// Pages of the windows of the module as a part module: of each input and of the output.
pub const WINDOW: u32 = 16;
/// Bytes of the pieces in which the raw and side inputs are read and the output written.
pub const PIECE: usize = 1 << 16;
/// Bytes of the buffer of the coded symbols: what is read of them ahead, and the zeros past
/// their end that a chunk of a damaged stream may read.
const WORDS: usize = 128 << 10;
/// Bytes a chunk reads at most of the coded symbols.
const CHUNK_WORDS: usize = 2 * MAX_CHUNK_SYMS;
/// Bytes of the buffer of the coded symbols that are not filled: a chunk's at most past the end.
const PAD: usize = CHUNK_WORDS + 64;

/// Params of more bytes are not taken.
pub const MAX_PARAMS: u64 = 1 << 20;

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes) at most, of `params` bytes of parameters,
/// whatever the lengths: the parameters, the buffers of the inputs and of the output, and the
/// chunk's symbols, raw and side bytes.
pub fn plan(initial: u64, params: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(params);
    for n in [WORDS, PIECE, PIECE, PIECE, MAX_CHUNK_SYMS, MAX_CHUNK_RAW, MAX_CHUNK_SIDE] {
        m.take(n as u64);
    }
    m.plan()
}

/// The coded symbols, read ahead in a buffer with zeros past their end.
struct Words<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    eof: bool,
    /// Whether words past the end were read (zeros).
    over: bool,
}

impl<'a> Words<'a> {
    fn new(src: &'a mut dyn Pull) -> Words<'a> {
        Words { src, buf: vec![0; WORDS], pos: 0, end: 0, eof: false, over: false }
    }

    /// Reads ahead to have the words of a chunk.
    #[inline]
    fn fill(&mut self) {
        // (past the end, which only the stream's end lets: zeros again from it)
        if self.pos > self.end {
            self.over = true;
            self.pos = self.end;
        }
        if self.eof || self.end - self.pos >= CHUNK_WORDS {
            return;
        }
        self.buf.copy_within(self.pos..self.end, 0);
        self.end -= self.pos;
        self.pos = 0;
        let cap = WORDS - PAD;
        while self.end < cap {
            let n = self.src.pull(&mut self.buf[self.end..cap]);
            if n == 0 {
                self.eof = true;
                self.buf[self.end..].fill(0);
                break;
            }
            self.end += n;
        }
    }

    /// Whether every word was read, and none past the end.
    fn done(&mut self) -> bool {
        self.fill();
        self.eof && self.pos == self.end && !self.over
    }
}

/// The output, put in pieces.
struct Out<'a> {
    sink: &'a mut dyn Put,
    buf: Vec<u8>,
}

impl Out<'_> {
    /// Room for the next `n` bytes (at most [`PIECE`]), zeroed.
    #[inline]
    fn chunk(&mut self, n: usize) -> Result<&mut [u8]> {
        if self.buf.len() + n > PIECE {
            self.flush()?;
        }
        let l = self.buf.len();
        self.buf.resize(l + n, 0);
        Ok(&mut self.buf[l..])
    }

    fn flush(&mut self) -> Result<()> {
        self.sink.put(&self.buf).ok_or(Error("output of another length"))?;
        self.buf.clear();
        Ok(())
    }
}

/// The working buffers of a chunk.
struct Scratch {
    syms: Vec<u8>,
    raw: Vec<u8>,
    side: Vec<u8>,
}

/// The module on its streams (the coded symbols of `syms_len` bytes, the raw and the side
/// bytes), its output of `size` bytes put to `out`.
fn run(
    params: &[u8],
    syms: &mut dyn Pull,
    syms_len: u64,
    raw: &mut dyn Pull,
    side: &mut dyn Pull,
    out: &mut dyn Put,
    size: u64,
) -> Result<()> {
    // (in the order of the plan)
    let mut words = Words::new(syms);
    let mut raw = Reader::new(raw, PIECE);
    let mut side = Reader::new(side, PIECE);
    let mut out = Out { sink: out, buf: Vec::with_capacity(PIECE) };
    let mut sc = Scratch { syms: vec![0; MAX_CHUNK_SYMS], raw: vec![0; MAX_CHUNK_RAW], side: vec![0; MAX_CHUNK_SIDE] };
    let mut dec = None;
    if syms_len > 0 {
        words.fill();
        if words.end - words.pos < HEAD {
            return Err(corrupt());
        }
        dec = Some(Decoder::new(&words.buf[words.pos..words.pos + HEAD], Kernel::best())?);
        words.pos += HEAD;
    }
    let mut at = 0;
    let mut total = 0u64;
    while at < params.len() {
        let r = Run::read(params, &mut at)?;
        total = total.checked_add(r.len()).filter(|&t| t <= size).ok_or_else(corrupt)?;
        match r {
            Run::Raw(n) => {
                let mut put = Ok(());
                let all = side.each(n, |p| {
                    put = out.chunk(p.len()).map(|o| o.copy_from_slice(p));
                    put.is_ok().then_some(())
                });
                put?;
                all.ok_or_else(corrupt)?;
            }
            Run::Float(f) => {
                let dec = dec.as_mut().ok_or_else(corrupt)?;
                float(&f, dec, &mut words, &mut raw, &mut side, &mut out, &mut sc)?;
            }
            Run::Quant { q, blocks, tables } => {
                let dec = dec.as_mut().ok_or_else(corrupt)?;
                let mut left = blocks;
                while left > 0 {
                    let b = left.min(q.chunk() as u64) as usize;
                    left -= b as u64;
                    words.fill();
                    let mut o = 0;
                    for (part, t) in q.coded().zip(&tables) {
                        let n = b * part.syms();
                        let full = n / LANES * LANES;
                        dec.groups(t, &words.buf, &mut words.pos, &mut sc.syms[o..o + full]);
                        if n > full {
                            dec.partial(t, &words.buf, &mut words.pos, &mut sc.syms[o + full..o + n]);
                        }
                        o += n;
                    }
                    let (nr, ns) = (layout::quant_raw(q, b), layout::quant_side(q, b));
                    raw.exact(&mut sc.raw[..nr]).ok_or_else(corrupt)?;
                    side.exact(&mut sc.side[..ns]).ok_or_else(corrupt)?;
                    let dst = out.chunk(b * q.block())?;
                    layout::join_quant(q, b, &sc.syms[..o], &sc.raw[..nr], &sc.side[..ns], dst);
                }
            }
        }
    }
    let written = total;
    out.flush()?;
    if written != size || dec.is_some_and(|d| !d.finish()) || !words.done() || !raw.done() || !side.done() {
        return Err(corrupt());
    }
    Ok(())
}

/// A run of floats.
fn float(
    r: &FloatRun,
    dec: &mut Decoder,
    words: &mut Words,
    raw: &mut Reader,
    side: &mut Reader,
    out: &mut Out,
    sc: &mut Scratch,
) -> Result<()> {
    let (f, kept) = (r.f, r.kept);
    let fb = f.bytes();
    let mut left = r.n;
    while left > 0 {
        let e = left.min(FLOAT_CHUNK as u64) as usize;
        left -= e as u64;
        words.fill();
        let full = e / LANES * LANES;
        let (a, b) = sc.syms.split_at_mut(FLOAT_CHUNK);
        dec.groups(&r.exp, &words.buf, &mut words.pos, &mut a[..full]);
        if e > full {
            dec.partial(&r.exp, &words.buf, &mut words.pos, &mut a[full..e]);
        }
        if let Some(t) = &r.top {
            dec.groups(t, &words.buf, &mut words.pos, &mut b[..full]);
            if e > full {
                dec.partial(t, &words.buf, &mut words.pos, &mut b[full..e]);
            }
        }
        let nr = e * kept.k;
        raw.exact(&mut sc.raw[..nr]).ok_or_else(corrupt)?;
        let m = r.m;
        let esc = a[..e].iter().filter(|&&s| s >= m).count();
        let eb = if r.whole { fb } else { 1 };
        side.exact(&mut sc.side[..esc * eb]).ok_or_else(corrupt)?;
        if !r.whole && sc.side[..esc].iter().any(|&x| x as u32 >= 1 << f.exp_bits()) {
            return Err(corrupt());
        }
        let dst = out.chunk(e * fb)?;
        let (a, b, rw) = (&a[..e], &b[..e], &sc.raw[..nr]);
        let map = &r.map;
        match (f, kept.k, kept.b) {
            // (the usual: BF16, or F16 of BF16's precision)
            (Float::Bf16 | Float::F16, 1, 0) => {
                let ms = f.man_bits() - kept.shift;
                for i in 0..e {
                    let k = rw[i] as u32;
                    let v = (k >> ms) << 15
                        | (map[a[i] as usize] as u32) << f.man_bits()
                        | (k & ((1 << ms) - 1)) << kept.shift;
                    dst[2 * i..2 * i + 2].copy_from_slice(&(v as u16).to_le_bytes());
                }
            }
            _ => {
                for i in 0..e {
                    let mut k = 0u32;
                    for (t, &x) in rw[i * kept.k..(i + 1) * kept.k].iter().enumerate() {
                        k |= (x as u32) << (8 * t);
                    }
                    if kept.b > 0 {
                        k |= (b[i] as u32) << (8 * kept.k);
                    }
                    let v = kept.float(f, map[a[i] as usize] as u32, k);
                    dst[fb * i..fb * (i + 1)].copy_from_slice(&v.to_le_bytes()[..fb]);
                }
            }
        }
        // (the escapes over what was put for them: whole, or their exponents)
        if esc > 0 {
            let mut j = 0;
            // (a counter of its own: of `enumerate`, gguf-recons is 5 bytes larger)
            #[allow(clippy::explicit_counter_loop)]
            for i in (0..e).filter(|&i| a[i] >= m) {
                if r.whole {
                    dst[fb * i..fb * (i + 1)].copy_from_slice(&sc.side[fb * j..fb * (j + 1)]);
                } else {
                    let mut v = [0u8; 4];
                    v[..fb].copy_from_slice(&dst[fb * i..fb * (i + 1)]);
                    let x = u32::from_le_bytes(v) & !(((1 << f.exp_bits()) - 1) << f.man_bits())
                        | (sc.side[j] as u32) << f.man_bits();
                    dst[fb * i..fb * (i + 1)].copy_from_slice(&x.to_le_bytes()[..fb]);
                }
                j += 1;
            }
        }
    }
    Ok(())
}

/// The module: the output of `len` bytes from `params` and `inputs` (the coded symbols, the raw
/// and the side bytes), whole, natively.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[syms, raw, side] = inputs else { return Err(corrupt()) };
    let mut out = Vec::with_capacity(len.min(1 << 30) as usize);
    let (mut s, mut r, mut d) = (Slice(syms), Slice(raw), Slice(side));
    run(params, &mut s, syms.len() as u64, &mut r, &mut d, &mut out, len)?;
    Ok(out)
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]).
pub fn decode_part(params_len: u64, lens: [u64; 3], size: u64) {
    aqr_api::finish(part(params_len, lens, size));
}

fn part(params_len: u64, lens: [u64; 3], size: u64) -> Result<()> {
    use aqr_api::{Memory, Params};
    if params_len > MAX_PARAMS {
        return Err(corrupt());
    }
    let mut params = vec![0; params_len as usize];
    Params::read(0, &mut params);
    let (mut i0, mut i1, mut i2) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]));
    let mut out = Written::new(size);
    run(&params, &mut i0, lens[0], &mut i1, &mut i2, &mut out, size)
}
