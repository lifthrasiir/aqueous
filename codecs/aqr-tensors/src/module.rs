//! The code of the module `tensors-recons` (FORMAT.md §5), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module. It is a part module: its three inputs are read and
//! its output written in order through windows, a chunk of a run at a time, and nothing of their
//! lengths is held.

use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};

use crate::layout::{
    self, ESCAPED, FLOAT_CHUNK, FloatRun, GridRun, GridValues, LanesRun, MAX_CHUNK_RAW, MAX_CHUNK_SIDE, MAX_CHUNK_SYMS,
    MAX_GRID, MAX_TILES, Run,
};
use crate::rans::{Decoder, HEAD, Kernel, LANES, Table};
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
    let tiles = 4 * MAX_TILES as usize;
    for n in [
        WORDS,
        PIECE,
        PIECE,
        PIECE,
        MAX_CHUNK_SYMS,
        MAX_CHUNK_RAW,
        MAX_CHUNK_SIDE,
        tiles,
        tiles,
        2 * tiles,
        4 * MAX_GRID,
    ] {
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

/// The working buffers of a chunk, and of a grid: the scales and zero points of a row of tiles,
/// and its values.
struct Scratch {
    syms: Vec<u8>,
    raw: Vec<u8>,
    side: Vec<u8>,
    scales: Vec<u32>,
    zeros: Vec<f32>,
    seen: Vec<u64>,
    grid: Vec<f32>,
}

/// The symbols of `n` of table `t` into `out` (groups of 32, the last perhaps fewer).
#[inline]
fn symbols(dec: &mut Decoder, t: &Table, words: &mut Words, out: &mut [u8]) {
    let n = out.len();
    let full = n / LANES * LANES;
    dec.groups(t, &words.buf, &mut words.pos, &mut out[..full]);
    if n > full {
        dec.partial(t, &words.buf, &mut words.pos, &mut out[full..]);
    }
}

/// Of the symbols `a` (each of 16), the symbols of their classes' tables, each class's in turn
/// into `out` (FORMAT.md §4.2): the start of each class's in it.
fn by_class(
    dec: &mut Decoder,
    tables: &[Table],
    class: &[u8; 16],
    a: &[u8],
    words: &mut Words,
    out: &mut [u8],
) -> [usize; 4] {
    let mut count = [0usize; 4];
    if tables.len() == 1 {
        count[0] = a.len();
    } else {
        a.iter().for_each(|&s| count[class[s as usize] as usize] += 1);
    }
    let mut start = [0usize; 4];
    let mut o = 0;
    for (k, t) in tables.iter().enumerate() {
        start[k] = o;
        symbols(dec, t, words, &mut out[o..o + count[k]]);
        o += count[k];
    }
    start
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
    let mut sc = Scratch {
        syms: vec![0; MAX_CHUNK_SYMS],
        raw: vec![0; MAX_CHUNK_RAW],
        side: vec![0; MAX_CHUNK_SIDE],
        scales: vec![0; MAX_TILES as usize],
        zeros: vec![0.0; MAX_TILES as usize],
        seen: vec![0; MAX_TILES as usize],
        grid: Vec::with_capacity(MAX_GRID),
    };
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
        let r = Run::read(params, &mut at, sc.grid.len())?;
        total = total.checked_add(r.len()).filter(|&t| t <= size).ok_or_else(corrupt)?;
        if let Run::Raw(n) = r {
            let mut put = Ok(());
            let all = side.each(n, |p| {
                put = out.chunk(p.len()).map(|o| o.copy_from_slice(p));
                put.is_ok().then_some(())
            });
            put?;
            all.ok_or_else(corrupt)?;
            continue;
        }
        let dec = dec.as_mut().ok_or_else(corrupt)?;
        match r {
            Run::Raw(_) => unreachable!(),
            Run::Float(f) => float(&f, dec, &mut words, &mut raw, &mut side, &mut out, &mut sc)?,
            Run::Lanes(l) => lanes(&l, dec, &mut words, &mut raw, &mut out, &mut sc)?,
            Run::Grid(g) => grid(g, dec, &mut words, &mut side, &mut out, &mut sc)?,
            Run::Quant { q, blocks, tables } => {
                let mut left = blocks;
                while left > 0 {
                    let b = left.min(q.chunk() as u64) as usize;
                    left -= b as u64;
                    words.fill();
                    let mut o = 0;
                    for (part, t) in q.coded().zip(&tables) {
                        let n = b * part.syms();
                        symbols(dec, t, &mut words, &mut sc.syms[o..o + n]);
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

/// A run of floats (FORMAT.md §4.2).
fn float(
    r: &FloatRun,
    dec: &mut Decoder,
    words: &mut Words,
    raw: &mut Reader,
    side: &mut Reader,
    out: &mut Out,
    sc: &mut Scratch,
) -> Result<()> {
    let f = r.f;
    let fb = f.bytes();
    let (t, rb, shift) = (r.t, r.raw_bits(), r.shift);
    let eb = if r.whole { fb } else { f.exp_bytes() };
    let mut left = r.n;
    while left > 0 {
        let e = left.min(FLOAT_CHUNK as u64) as usize;
        left -= e as u64;
        words.fill();
        let (a, b) = sc.syms.split_at_mut(FLOAT_CHUNK);
        let a = &mut a[..e];
        symbols(dec, &r.exp, words, a);
        let mut start = [0; 4];
        if t > 0 {
            start = by_class(dec, &r.man, &r.class, a, words, b);
        }
        let nr = (e * rb as usize).div_ceil(8);
        raw.exact(&mut sc.raw[..nr]).ok_or_else(corrupt)?;
        sc.raw[nr..nr + 8].fill(0);
        let m = r.m;
        let esc = a.iter().filter(|&&s| s >= m).count();
        side.exact(&mut sc.side[..esc * eb]).ok_or_else(corrupt)?;
        let dst = out.chunk(e * fb)?;
        let (a, rw, map) = (&*a, &sc.raw[..nr + 8], &r.map);
        match (fb, t, rb) {
            // (the usual: BF16, or F16 of BF16's precision)
            (2, 0, 8) => {
                let ms = f.man_bits() - shift;
                for i in 0..e {
                    let k = rw[i] as u32;
                    let v =
                        (k >> ms) << 15 | (map[a[i] as usize] as u32) << f.man_bits() | (k & ((1 << ms) - 1)) << shift;
                    dst[2 * i..2 * i + 2].copy_from_slice(&(v as u16).to_le_bytes());
                }
            }
            // (F32 of every bit)
            (4, 0, 24) => {
                for i in 0..e {
                    let k = u32::from_le_bytes([rw[3 * i], rw[3 * i + 1], rw[3 * i + 2], 0]);
                    let v = (k >> 23) << 31 | (map[a[i] as usize] as u32) << 23 | k & 0x7f_ffff;
                    dst[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            _ => {
                let fl = Floats { f, shift, rb, t, a, b, start, class: &r.class, map, rw };
                match fb {
                    1 => fl.put::<1>(dst),
                    2 => fl.put::<2>(dst),
                    4 => fl.put::<4>(dst),
                    _ => fl.put::<8>(dst),
                }
            }
        }
        // (the escapes over what was put for them: whole, or their exponents)
        if esc > 0 {
            let mut j = 0;
            // (a counter of its own: of `enumerate`, tensors-recons is 5 bytes larger)
            #[allow(clippy::explicit_counter_loop)]
            for i in (0..e).filter(|&i| a[i] >= m) {
                let d = &mut dst[fb * i..fb * (i + 1)];
                if r.whole {
                    d.copy_from_slice(&sc.side[fb * j..fb * (j + 1)]);
                } else {
                    let mut x = [0u8; 8];
                    x[..eb].copy_from_slice(&sc.side[eb * j..eb * (j + 1)]);
                    let x = u64::from_le_bytes(x);
                    if x >= 1 << f.exp_bits() {
                        return Err(corrupt());
                    }
                    let mut v = [0u8; 8];
                    v[..fb].copy_from_slice(d);
                    let v = u64::from_le_bytes(v) & !(((1 << f.exp_bits()) - 1) << f.man_bits()) | x << f.man_bits();
                    d.copy_from_slice(&v.to_le_bytes()[..fb]);
                }
                j += 1;
            }
        }
    }
    Ok(())
}

/// A chunk of a run of floats decoded but put together: its exponents' symbols `a`, the top bits
/// `b` of each class from `start`, the raw bits `rw`.
struct Floats<'a> {
    f: crate::layout::Float,
    shift: u32,
    rb: u32,
    t: u32,
    a: &'a [u8],
    b: &'a [u8],
    start: [usize; 4],
    class: &'a [u8; 16],
    map: &'a [u16; 16],
    rw: &'a [u8],
}

impl Floats<'_> {
    /// The floats of `FB` bytes into `dst` (specialised: the loops over every float are most of
    /// the decoder's time).
    #[inline(never)]
    fn put<const FB: usize>(&self, dst: &mut [u8]) {
        let (f, shift, rb, rw, a, map) = (self.f, self.shift, self.rb, self.rw, self.a, self.map);
        let mut set = |i: usize, s: usize, k: u64| {
            let v = layout::kept_float(f, shift, map[s] as u64, k);
            dst[FB * i..FB * i + FB].copy_from_slice(&v.to_le_bytes()[..FB]);
        };
        if self.t == 0 && rb % 8 == 0 {
            let k = rb as usize / 8;
            for i in 0..a.len() {
                let mut b = [0u8; 8];
                b[..k].copy_from_slice(&rw[i * k..i * k + k]);
                set(i, a[i] as usize, u64::from_le_bytes(b));
            }
        } else if self.t == 0 {
            for (i, &s) in a.iter().enumerate() {
                set(i, s as usize, layout::get_bits(rw, i * rb as usize, rb));
            }
        } else {
            let mut cur = self.start;
            for (i, &s) in a.iter().enumerate() {
                let s = s as usize;
                let c = self.class[s] as usize;
                let top = self.b[cur[c]] as u64;
                cur[c] += 1;
                set(i, s, top << rb | layout::get_bits(rw, i * rb as usize, rb));
            }
        }
    }
}

/// A run of lanes (FORMAT.md §4.4).
fn lanes(
    r: &LanesRun,
    dec: &mut Decoder,
    words: &mut Words,
    raw: &mut Reader,
    out: &mut Out,
    sc: &mut Scratch,
) -> Result<()> {
    let w = r.w;
    let nibs = r.nib.count_ones() as usize;
    let rl = w - nibs;
    let mut left = r.n;
    while left > 0 {
        let e = left.min(r.chunk() as u64) as usize;
        left -= e as u64;
        words.fill();
        for (k, t) in r.tables.iter().enumerate() {
            symbols(dec, t, words, &mut sc.syms[k * e..(k + 1) * e]);
        }
        raw.exact(&mut sc.raw[..e * rl]).ok_or_else(corrupt)?;
        let dst = out.chunk(e * w)?;
        let (mut k, mut j) = (0, 0);
        for lane in 0..w {
            if r.nib >> lane & 1 == 1 {
                let (lo, hi) = (&sc.syms[2 * k * e..][..e], &sc.syms[(2 * k + 1) * e..][..e]);
                for i in 0..e {
                    dst[w * i + lane] = lo[i] | hi[i] << 4;
                }
                k += 1;
            } else {
                for i in 0..e {
                    dst[w * i + lane] = sc.raw[rl * i + j];
                }
                j += 1;
            }
        }
    }
    Ok(())
}

/// A run of a grid (FORMAT.md §4.5).
fn grid(
    r: GridRun,
    dec: &mut Decoder,
    words: &mut Words,
    side: &mut Reader,
    out: &mut Out,
    sc: &mut Scratch,
) -> Result<()> {
    if let GridValues::Own(g) = &r.grid {
        sc.grid.clear();
        sc.grid.extend_from_slice(g);
    }
    let (f, k) = (r.f, r.k);
    let fb = f.bytes();
    // (where the element is: its row, its column, its tile's column and its column in the tile,
    // and its tiles' row; of each column of tiles, the row of tiles of its scale)
    let (mut row, mut col) = (r.start / r.row, r.start % r.row);
    let (mut tc, mut cin, mut band) = ((col / r.bc) as usize, col % r.bc, row / r.br);
    sc.seen[..r.tiles() as usize].fill(u64::MAX);
    let mut left = r.n;
    let mut b4 = [0u8; 4];
    while left > 0 {
        let e = left.min(FLOAT_CHUNK as u64) as usize;
        left -= e as u64;
        words.fill();
        let (a, b) = sc.syms.split_at_mut(FLOAT_CHUNK);
        let a = &mut a[..e];
        symbols(dec, &r.tables[0], words, a);
        let mut cur = [0; 4];
        if k > 16 {
            cur = by_class(dec, &r.tables[1..], &r.class, a, words, b);
        }
        // (the indices, each below the grid's values)
        let q = &mut sc.side[..e];
        if k > 16 {
            for i in 0..e {
                let h = a[i] as usize;
                let c = r.class[h] as usize;
                q[i] = (h << 4) as u8 | b[cur[c]];
                cur[c] += 1;
            }
            if q.iter().fold(0u8, |m, &v| m.max(v)) as usize >= k {
                return Err(corrupt());
            }
        } else {
            q.copy_from_slice(a);
        }
        let dst = out.chunk(e * fb)?;
        // (in spans of the elements of a tile)
        let mut i = 0;
        while i < e {
            if sc.seen[tc] != band {
                sc.seen[tc] = band;
                side.exact(&mut b4).ok_or_else(corrupt)?;
                sc.scales[tc] = u32::from_le_bytes(b4);
                if r.zero && sc.scales[tc] != ESCAPED {
                    side.exact(&mut b4).ok_or_else(corrupt)?;
                    sc.zeros[tc] = f32::from_le_bytes(b4);
                }
            }
            let span = ((e - i) as u64).min(r.bc - cin).min(r.row - col) as usize;
            let d = &mut dst[fb * i..fb * (i + span)];
            let s = sc.scales[tc];
            if s == ESCAPED {
                side.exact(d).ok_or_else(corrupt)?;
            } else {
                let (g, qs, s, z) = (&sc.grid[..k], &q[i..i + span], f32::from_bits(s), sc.zeros[tc]);
                crate::gridval::values(f, g, qs, s, z, r.zero, d);
            }
            i += span;
            col += span as u64;
            cin += span as u64;
            if col == r.row {
                (row, col, tc, cin) = (row + 1, 0, 0, 0);
                band = row / r.br;
            } else if cin == r.bc {
                (tc, cin) = (tc + 1, 0);
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
