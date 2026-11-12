//! aqr-tensors as a module of entry formulas (see aqr-formula): a file of tensors' header as it is
//! (GGUF's, `torch.save`'s, safetensors'), its tensor data in segments, each an application of
//! `tensors-recons` (FORMAT.md §2), and the long runs of bytes this crate does not code as they are.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::grid;
use crate::layout::{self, FLOAT_CHUNK, Float, FloatRun, LANES_CHUNK, LanesRun, Part, Quant, Run};
use crate::rans::{self, Encoder, Table};
use crate::tensors::{self, Data, Tensor, Tensors};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("tensors-recons", 2).with_plan(plan);

/// The memory of the module: its plan ([`crate::module::plan`]).
fn plan(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(crate::module::plan(u.initial(), u.params.len() as u64))
}

/// The classes of the streams: of a GGUF file's header (the others' [`tensors::Container::header`]).
pub const HEADER: &str = "gguf/header";
pub const SYMS: &str = "tensors/syms";
pub const RAW: &str = "tensors/raw";
pub const SIDE: &str = "tensors/side";
pub const DATA: &str = "tensors/data";

/// Bytes of a segment, about: of the output of an application. The decoder makes the
/// applications of a file at once, 8 of them ahead and 256 MiB at most, so 16 MiB keeps 8 at
/// work; the encoder holds the symbols of as many as it has threads.
pub const SEGMENT: usize = 16 << 20;
/// Bytes not coded of at least this many, between the tensors coded, are left out of the
/// applications, as they are.
pub const LONG_RAW: usize = 1 << 20;
/// A tensor (or a part of one in a segment) of fewer bytes is not coded.
pub const MIN_CODED: usize = 4096;

/// Whether `d` starts as a file of tensors this crate takes: GGUF, `torch.save`'s or safetensors.
pub fn starts(d: &[u8]) -> bool {
    tensors::starts(d).is_some()
}

/// A span of the file in a segment: bytes as they are, or of a tensor coded (its index in the
/// file's).
#[derive(Clone, Copy, Debug)]
enum Span {
    Raw(usize, usize),
    Coded(usize, Data, usize),
}

impl Span {
    fn range(self) -> std::ops::Range<usize> {
        match self {
            Span::Raw(a, b) => a..b,
            Span::Coded(a, d, _) => a..a + d.len() as usize,
        }
    }
}

/// A piece of the root: the header, a long run as it is, or a segment.
enum Piece {
    Long(usize, usize),
    Segment(Vec<Span>),
}

/// The pieces of the file after the header: the tensors coded cut into segments of about
/// `segment` bytes (at chunks), with the bytes between them.
fn pieces(len: usize, g: &Tensors, segment: usize) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut seg: Vec<Span> = Vec::new();
    let mut seg_len = 0usize;
    let close = |seg: &mut Vec<Span>, seg_len: &mut usize, out: &mut Vec<Piece>| {
        if !seg.is_empty() {
            out.push(Piece::Segment(std::mem::take(seg)));
        }
        *seg_len = 0;
    };
    let mut at = g.data;
    let raw = |a: usize, b: usize, seg: &mut Vec<Span>, seg_len: &mut usize, out: &mut Vec<Piece>| {
        if a == b {
            return;
        }
        if b - a >= LONG_RAW {
            close(seg, seg_len, out);
            out.push(Piece::Long(a, b));
        } else {
            seg.push(Span::Raw(a, b));
            *seg_len += b - a;
        }
    };
    for (ti, t) in g.tensors.iter().enumerate() {
        let (start, data) = (t.start, t.data);
        raw(at, start, &mut seg, &mut seg_len, &mut out);
        // (cut at chunks: of floats, a chunk; of blocks, a chunk's blocks; of lanes, a chunk's
        // elements)
        let (unit, unit_bytes) = match data {
            Data::Float(f, _) => (FLOAT_CHUNK as u64, (FLOAT_CHUNK * f.bytes()) as u64),
            Data::Quant(q, _) => (q.chunk() as u64, (q.chunk() * q.block()) as u64),
            Data::Lanes(w, _) => ((LANES_CHUNK / w as usize) as u64, LANES_CHUNK as u64),
        };
        let count = data.count();
        let mut done = 0u64;
        let mut p = start;
        while done < count {
            let room = segment.saturating_sub(seg_len) as u64;
            let units = (room / unit_bytes).max(1);
            let n = (units * unit).min(count - done);
            let part = data.with_count(n);
            seg.push(Span::Coded(p, part, ti));
            seg_len += part.len() as usize;
            p += part.len() as usize;
            done += n;
            if seg_len >= segment - unit_bytes.min(segment as u64 / 2) as usize {
                close(&mut seg, &mut seg_len, &mut out);
            }
        }
        at = p;
    }
    raw(at, len, &mut seg, &mut seg_len, &mut out);
    close(&mut seg, &mut seg_len, &mut out);
    out
}

/// How a tensor is coded, as a sample of it tells ([`model`]).
#[derive(Clone, Copy, Debug)]
enum Model {
    /// Floats, the top of the mantissa symbols where `man` (it looked worth it).
    Float { man: bool },
    /// Floats that are a grid's values (where a run of the grid does not hold, as floats).
    Grid(grid::Cfg),
    /// Blocks, or lanes.
    Other,
}

/// The model of tensor `t` of file `d`.
fn model(d: &[u8], t: &Tensor) -> Model {
    let Data::Float(f, _) = t.data else { return Model::Other };
    let bytes = &d[t.start..t.start + t.data.len() as usize];
    if let Some(cfg) = grid::detect(f, bytes, t.row) {
        return Model::Grid(cfg);
    }
    Model::Float { man: mantissa_pays(f, bytes) }
}

/// Whether the top bits of the mantissa (with the sign) given the exponent look to come to
/// enough less than as they are, on a sample of floats `d` of `f`: 1% of the floats' bits.
fn mantissa_pays(f: Float, d: &[u8]) -> bool {
    let fb = f.bytes();
    let n = d.len() / fb;
    let get = |i: usize| {
        let mut b = [0u8; 8];
        b[..fb].copy_from_slice(&d[i * fb..(i + 1) * fb]);
        u64::from_le_bytes(b)
    };
    let (win, wins) = (1024usize.min(n), 8usize);
    let starts: Vec<usize> = (0..wins).map(|k| (n - win) / (wins - 1) * k).collect();
    let mmask = (1u64 << f.man_bits()) - 1;
    let tz =
        (starts.iter().flat_map(|&s| s..s + win)).map(|i| (get(i) & mmask | 1 << f.man_bits()).trailing_zeros()).min();
    let shift = tz.unwrap_or(0).min(f.man_bits());
    let r = 1 + f.man_bits() - shift;
    let t4 = r.min(4);
    // (of each exponent, the top bits counted; the exponents' own counts)
    let mut counts = vec![[0u64; 16]; 1 << f.exp_bits()];
    for i in starts.iter().flat_map(|&s| s..s + win) {
        let v = get(i);
        let top = (layout::kept_bits(f, shift, v) >> (r - t4)) as usize;
        counts[f.exp(v) as usize][top] += 1;
    }
    let all = (wins * win) as f64;
    let h = |c: &[u64]| {
        let t: u64 = c.iter().sum();
        c.iter().filter(|&&x| x > 0).map(|&x| x as f64 * (t as f64 / x as f64).log2()).sum::<f64>()
    };
    let exps: Vec<u64> = counts.iter().map(|c| c.iter().sum()).collect();
    let float_bits = h(&exps) / all + r as f64;
    // (the exponents as symbols, the 15 commonest and the rest one; of the sample's counts, the
    // classes the coder would make, without their tables' cost)
    let mut order: Vec<(usize, &[u64; 16])> =
        counts.iter().enumerate().filter(|(_, c)| c.iter().any(|&x| x > 0)).collect();
    order.sort_by_key(|&(e, c)| (std::cmp::Reverse(c.iter().sum::<u64>()), e));
    let mut by_sym = [[0u64; 16]; 16];
    for (k, (_, c)) in order.iter().enumerate() {
        for v in 0..16 {
            by_sym[k.min(15)][v] += c[v];
        }
    }
    // (the best number of top bits: what it saves a float)
    let gain = (1..=t4)
        .map(|t| {
            let mut c = [[0u64; 16]; 16];
            for s in 0..16 {
                for v in 0..16 {
                    c[s][v >> (t4 - t)] += by_sym[s][v];
                }
            }
            t as f64 - grid::classes(&c, 1 << t, 0.0).2 / all
        })
        .fold(0f64, f64::max);
    gain >= 0.015 * float_bits
}

/// What a segment comes to: the parameters, and its coded symbols, raw and side bytes.
#[derive(Default)]
struct Coded {
    params: Vec<u8>,
    syms: Vec<u8>,
    raw: Vec<u8>,
    side: Vec<u8>,
    /// Whether any run was coded.
    coded: bool,
}

/// The runs of a segment coded, those that would not come to less as they are; of file `d`, its
/// tensors `g` and their models (each made where first needed).
fn segment(d: &[u8], spans: &[Span], g: &Tensors, models: &[std::sync::OnceLock<Model>]) -> Coded {
    let mut c = Coded::default();
    // (about 2 symbols a byte at most: blocks of nibbles)
    let bytes: usize = spans.iter().map(|s| s.range().len()).sum();
    let mut enc = Encoder::with_capacity(bytes * 9 / 5);
    let mut tables: Vec<Table> = Vec::new();
    // (bytes as they are run together)
    let mut pending = 0u64;
    let flush = |pending: &mut u64, params: &mut Vec<u8>| {
        if *pending > 0 {
            Run::Raw(*pending).write(params);
            *pending = 0;
        }
    };
    // (the grid of the last run of a grid)
    let mut last_grid = None;
    for &s in spans {
        let r = s.range();
        let bytes = &d[r.clone()];
        let run = match s {
            Span::Coded(_, _, _) if bytes.len() < MIN_CODED => None,
            Span::Coded(at, Data::Float(f, n), ti) => {
                let t = &g.tensors[ti];
                let model = *models[ti].get_or_init(|| model(d, t));
                let gr = match model {
                    Model::Grid(cfg) => {
                        let tensor = &d[t.start..t.start + t.data.len() as usize];
                        let from = ((at - t.start) / f.bytes()) as u64;
                        let limit = 0.985 * 8.0 * bytes.len() as f64;
                        let same = last_grid == Some(cfg.grid);
                        grid::encode(cfg, f, tensor, from, n, same, limit, &mut enc, &mut tables, &mut c.side).map(
                            |r| {
                                last_grid = Some(cfg.grid);
                                Run::Grid(r)
                            },
                        )
                    }
                    _ => None,
                };
                let man = matches!(model, Model::Float { man: true });
                gr.or_else(|| float(f, n as usize, bytes, man, &mut enc, &mut tables, &mut c.raw, &mut c.side))
            }
            Span::Coded(_, Data::Quant(q, n), _) => {
                quant(q, n as usize, bytes, &mut enc, &mut tables, &mut c.raw, &mut c.side)
            }
            Span::Coded(_, Data::Lanes(w, n), _) => {
                lanes(w as usize, n as usize, bytes, &mut enc, &mut tables, &mut c.raw)
            }
            Span::Raw(..) => None,
        };
        match run {
            Some(run) => {
                flush(&mut pending, &mut c.params);
                run.write(&mut c.params);
                c.coded = true;
            }
            None => {
                c.side.extend_from_slice(bytes);
                pending += bytes.len() as u64;
            }
        }
    }
    flush(&mut pending, &mut c.params);
    c.syms = enc.finish(&tables);
    c
}

/// Whether coding at `bits` pays against `bytes` as they are.
fn pays(bits: f64, bytes: usize) -> bool {
    bits < 0.985 * 8.0 * bytes as f64
}

/// A run of `n` floats `d` coded (FORMAT.md §4.2), the top bits of their mantissas symbols where
/// `man` and they come to enough less; None where it would not come to less.
#[allow(clippy::too_many_arguments)]
fn float(
    f: Float,
    n: usize,
    d: &[u8],
    man: bool,
    enc: &mut Encoder,
    tables: &mut Vec<Table>,
    raw: &mut Vec<u8>,
    side: &mut Vec<u8>,
) -> Option<Run> {
    match f.bytes() {
        1 => float_of::<1>(f, n, d, man, enc, tables, raw, side),
        2 => float_of::<2>(f, n, d, man, enc, tables, raw, side),
        4 => float_of::<4>(f, n, d, man, enc, tables, raw, side),
        _ => float_of::<8>(f, n, d, man, enc, tables, raw, side),
    }
}

/// The float `i` of `d`, of `FB` bytes.
#[inline(always)]
fn get<const FB: usize>(d: &[u8], i: usize) -> u64 {
    let mut b = [0u8; 8];
    b[..FB].copy_from_slice(&d[FB * i..FB * i + FB]);
    u64::from_le_bytes(b)
}

/// [`float`] of floats of `FB` bytes (the loops over every float specialised: they are most of
/// the encoder's time).
#[allow(clippy::too_many_arguments)]
fn float_of<const FB: usize>(
    f: Float,
    n: usize,
    d: &[u8],
    man: bool,
    enc: &mut Encoder,
    tables: &mut Vec<Table>,
    raw: &mut Vec<u8>,
    side: &mut Vec<u8>,
) -> Option<Run> {
    let (m_bits, e_bits) = (f.man_bits(), f.exp_bits());
    let ne = 1usize << e_bits;
    let mmask = (1u64 << m_bits) - 1;
    // (of each count of trailing zero bits of the mantissa, M for a mantissa of zero, the
    // exponents counted; in parts, the counts of a part in 32 bits)
    let tz = |v: u64| ((v & mmask) | 1 << m_bits).trailing_zeros() as usize;
    let mut hist = vec![0u64; (m_bits as usize + 1) * ne];
    let mut part_hist = vec![0u32; (m_bits as usize + 1) * ne];
    for part in d.chunks(FB * (1 << 20)) {
        for i in 0..part.len() / FB {
            let v = get::<FB>(part, i);
            part_hist[tz(v) * ne + f.exp(v) as usize] += 1;
        }
        for (h, p) in hist.iter_mut().zip(part_hist.iter_mut()) {
            *h += *p as u64;
            *p = 0;
        }
    }
    let by_tz: Vec<u64> = hist.chunks(ne).map(|h| h.iter().sum()).collect();
    // (the floats with fewer zeros than a shift: escapes whole, as many as there are; else the
    // escapes are of the exponents, the shift that of all floats)
    let short = |s: u32| by_tz[..s as usize].iter().sum::<u64>();
    let all_zero = (0..=m_bits).rev().find(|&s| short(s) == 0)?;
    // (the shift that saves the most with escapes whole: of `shift` bits of each float, less a
    // float's bytes and more for each short one)
    let best = (0..=m_bits)
        .max_by_key(|&s| (n as i64 * s as i64 - short(s) as i64 * (8 * FB as i64 + 8), std::cmp::Reverse(s)))?;
    // the cost of each way: its table, escapes and bits kept
    let plan = |shift: u32, whole: bool| {
        let kept = 1 + m_bits - shift;
        let mut ex = vec![0u64; ne];
        for h in hist.chunks(ne).skip(shift as usize) {
            for e in 0..ne {
                ex[e] += h[e];
            }
        }
        let short = short(shift);
        let mut order: Vec<usize> = (0..ne).filter(|&e| ex[e] > 0).collect();
        order.sort_by_key(|&e| (std::cmp::Reverse(ex[e]), e));
        let m = if order.len() <= 16 && short == 0 { order.len() } else { order.len().min(15) };
        let mut map = [0u16; 16];
        let mut sym = vec![15u8; ne];
        for (s, &e) in order.iter().take(m).enumerate() {
            map[s] = e as u16;
            sym[e] = s as u8;
        }
        let mut a = [0u64; 16];
        for e in 0..ne {
            a[sym[e] as usize] += ex[e];
        }
        a[15] += short;
        let escapes = if m < 16 { a[15] } else { 0 };
        let exp = Table::of_counts(&a);
        let eb = if whole { FB } else { f.exp_bytes() } as u64;
        let bits = exp.cost(&a) + 8.0 * (escapes * eb) as f64 + (n as u64 * kept as u64) as f64 + 400.0;
        (bits, shift, map, m, sym, escapes, exp)
    };
    let (bits, shift, map, m, sym, escapes, exp, whole) = {
        let a = plan(all_zero, false);
        let b = plan(best, true);
        match b.0 < a.0 {
            true => (b.0, b.1, b.2, b.3, b.4, b.5, b.6, true),
            false => (a.0, a.1, a.2, a.3, a.4, a.5, a.6, false),
        }
    };
    let kept = 1 + m_bits - shift;
    let low = (1u64 << shift) - 1;
    // (a float's symbol: of its exponent, or the escape where it is short)
    let symbol = |v: u64| if v & low != 0 { 15 } else { sym[f.exp(v) as usize] };
    // the top bits of the mantissa: of each exponent symbol, the top 4 bits (or all) counted
    let (mut t, mut class, mut man_tables, mut bits) = (0u32, [0u8; 16], Vec::new(), bits);
    if man {
        let t4 = kept.min(4);
        let mut counts = [[0u64; 16]; 16];
        for i in 0..n {
            let v = get::<FB>(d, i);
            counts[symbol(v) as usize][(layout::kept_bits(f, shift, v) >> (kept - t4)) as usize] += 1;
        }
        for tt in 1..=t4 {
            let mut c = [[0u64; 16]; 16];
            for s in 0..16 {
                for v in 0..16 {
                    c[s][v >> (t4 - tt)] += counts[s][v];
                }
            }
            let (_, cl, ts, b) = grid::class_tables(&c, 1 << tt);
            let b = bits - (n as u64 * tt as u64) as f64 + b;
            if b < bits.min(if t == 0 { 0.99 * bits } else { f64::INFINITY }) {
                (t, class, man_tables, bits) = (tt, cl, ts, b);
            }
        }
    }
    if !pays(bits, d.len()) {
        return None;
    }
    let te = tables.len() as u32;
    tables.push(exp);
    let tm = tables.len() as u32;
    tables.extend_from_slice(&man_tables);
    let rb = kept - t;
    let mut sa = [0u8; FLOAT_CHUNK];
    let mut sb: Vec<Vec<u8>> = vec![Vec::with_capacity(FLOAT_CHUNK); man_tables.len()];
    let mut bits_buf = vec![0u8; (FLOAT_CHUNK * rb as usize).div_ceil(8) + 16];
    raw.reserve((n * rb as usize).div_ceil(8));
    for c in (0..n).step_by(FLOAT_CHUNK) {
        let e = (n - c).min(FLOAT_CHUNK);
        let chunk = &d[FB * c..FB * (c + e)];
        let nr = (e * rb as usize).div_ceil(8);
        if t == 0 && rb == 8 {
            let at = raw.len();
            raw.resize(at + e, 0);
            let r = &mut raw[at..];
            for i in 0..e {
                let v = get::<FB>(chunk, i);
                sa[i] = symbol(v);
                r[i] = layout::kept_bits(f, shift, v) as u8;
            }
        } else if t == 0 && rb % 8 == 0 {
            // (whole bytes: F32's 24 bits, say)
            let k = rb as usize / 8;
            let at = raw.len();
            raw.resize(at + e * k, 0);
            let r = &mut raw[at..];
            for i in 0..e {
                let v = get::<FB>(chunk, i);
                sa[i] = symbol(v);
                r[i * k..i * k + k].copy_from_slice(&layout::kept_bits(f, shift, v).to_le_bytes()[..k]);
            }
        } else {
            bits_buf[..nr + 8].fill(0);
            sb.iter_mut().for_each(|b| b.clear());
            for (i, sym) in sa[..e].iter_mut().enumerate() {
                let v = get::<FB>(chunk, i);
                let s = symbol(v);
                *sym = s;
                let k = layout::kept_bits(f, shift, v);
                if t > 0 {
                    sb[class[s as usize] as usize].push((k >> rb) as u8);
                }
                layout::put_bits(&mut bits_buf, i * rb as usize, rb, k & ((1u64 << rb) - 1));
            }
            raw.extend_from_slice(&bits_buf[..nr]);
        }
        if escapes > 0 {
            for i in (0..e).filter(|&i| sa[i] as usize >= m) {
                match whole {
                    true => side.extend_from_slice(&chunk[FB * i..FB * i + FB]),
                    false => side.extend_from_slice(&f.exp(get::<FB>(chunk, i)).to_le_bytes()[..f.exp_bytes()]),
                }
            }
        }
        grid::put(enc, te, &sa[..e]);
        for (k, b) in sb.iter().enumerate() {
            grid::put(enc, tm + k as u32, b);
        }
    }
    Some(Run::Float(FloatRun { f, shift, n: n as u64, map, m: m as u8, whole, exp, t, class, man: man_tables }))
}

/// A run of `n` elements `d` of lanes of `w` bytes (FORMAT.md §4.4): the lanes whose nibbles come
/// to enough less symbols; None where it would not come to less.
fn lanes(w: usize, n: usize, d: &[u8], enc: &mut Encoder, tables: &mut Vec<Table>, raw: &mut Vec<u8>) -> Option<Run> {
    let mut h = vec![[[0u64; 16]; 2]; w];
    for e in d.chunks_exact(w) {
        for (j, &b) in e.iter().enumerate() {
            h[j][0][(b & 15) as usize] += 1;
            h[j][1][(b >> 4) as usize] += 1;
        }
    }
    let (mut nib, mut bits, mut ts) = (0u8, 0f64, Vec::new());
    for (j, hj) in h.iter().enumerate() {
        let (lo, hi) = (Table::of_counts(&hj[0]), Table::of_counts(&hj[1]));
        let b = lo.cost(&hj[0]) + hi.cost(&hj[1]) + 600.0;
        if b < 0.97 * 8.0 * n as f64 {
            nib |= 1 << j;
            bits += b;
            ts.extend([lo, hi]);
        } else {
            bits += 8.0 * n as f64;
        }
    }
    if nib == 0 || !pays(bits, d.len()) {
        return None;
    }
    let first = tables.len() as u32;
    tables.extend_from_slice(&ts);
    let run = LanesRun { w, n: n as u64, nib, tables: ts };
    let chunk = run.chunk();
    let mut s = vec![0u8; chunk];
    for c in d.chunks(chunk * w) {
        let e = c.len() / w;
        let mut k = 0;
        for j in 0..w {
            if nib >> j & 1 == 1 {
                for (i, v) in s[..e].iter_mut().enumerate() {
                    *v = c[w * i + j] & 15;
                }
                grid::put(enc, first + 2 * k, &s[..e]);
                for (i, v) in s[..e].iter_mut().enumerate() {
                    *v = c[w * i + j] >> 4;
                }
                grid::put(enc, first + 2 * k + 1, &s[..e]);
                k += 1;
            }
        }
        for el in c.chunks_exact(w) {
            raw.extend((0..w).filter(|&j| nib >> j & 1 == 0).map(|j| el[j]));
        }
    }
    Some(Run::Lanes(run))
}

/// A run of `n` blocks of `q` coded (FORMAT.md §4.3), or None where it would not come to less.
fn quant(
    q: Quant,
    n: usize,
    d: &[u8],
    enc: &mut Encoder,
    tables: &mut Vec<Table>,
    raw: &mut Vec<u8>,
    side: &mut Vec<u8>,
) -> Option<Run> {
    let parts: Vec<Part> = q.coded().collect();
    let bs = q.block();
    let first = tables.len() as u32;
    // (all of the run taken apart into the encoder, chunk by chunk: the symbols of each part of
    // a chunk are groups of its table)
    let mark = enc.mark();
    let (r0, s0) = (raw.len(), side.len());
    raw.resize(r0 + layout::quant_raw(q, n), 0);
    side.resize(s0 + layout::quant_side(q, n), 0);
    let (mut ra, mut da) = (r0, s0);
    let mut hist = vec![[0u64; 16]; parts.len()];
    for c in d.chunks(q.chunk() * bs) {
        let b = c.len() / bs;
        let (ns, nr, nd) = (layout::quant_syms(q, b), layout::quant_raw(q, b), layout::quant_side(q, b));
        let syms = enc.append(ns);
        layout::split_quant(q, c, syms, &mut raw[ra..ra + nr], &mut side[da..da + nd]);
        // (of each part, its symbols counted: four counts, so that the same symbol in a row does
        // not wait on its own count)
        let mut at = 0;
        for (k, p) in parts.iter().enumerate() {
            let len = b * p.syms();
            let mut h = [[0u32; 16]; 4];
            let (fours, rest) = syms[at..at + len].as_chunks::<4>();
            for g in fours {
                for j in 0..4 {
                    h[j][g[j] as usize] += 1;
                }
            }
            for &s in rest {
                h[0][s as usize] += 1;
            }
            for s in 0..16 {
                hist[k][s] += h.iter().map(|h| h[s] as u64).sum::<u64>();
            }
            at += len;
        }
        for (k, p) in parts.iter().enumerate() {
            enc.told_syms(first + k as u32, b * p.syms());
        }
        (ra, da) = (ra + nr, da + nd);
    }
    let ts: Vec<Table> = hist.iter().map(Table::of_counts).collect();
    let coded_bits: f64 = ts.iter().zip(&hist).map(|(t, h)| t.cost(h)).sum();
    let plain_bits: f64 = parts.iter().map(|p| 8.0 * (p.bytes() - p.raw()) as f64 * n as f64).sum::<f64>();
    if !pays(coded_bits + 300.0 * parts.len() as f64, (plain_bits / 8.0) as usize) {
        enc.rewind(mark);
        raw.truncate(r0);
        side.truncate(s0);
        return None;
    }
    tables.extend_from_slice(&ts);
    Some(Run::Quant { q, blocks: n as u64, tables: ts })
}

/// Bytes that the formula data of the archive of [`encode`]'s formula of `input` comes to at
/// most, not compressed (spec "Formula data"), its streams of a sink ([`encode_sealing`]: those of
/// SYMS and RAW of each segment a stream of their own): by the pieces of the file, each span of a
/// segment's parameters at most as long as a run of floats with two tables, its application and
/// slices of each input over two blocks, a run kept as it is a slice of each block it is over,
/// and a byte listing each block. None where [`encode`] takes no formula of it.
pub fn formula_bound(input: &[u8]) -> Option<usize> {
    let g = tensors::parse(input)?;
    if g.tensors.is_empty() {
        return None;
    }
    // (a varint of 64 bits, a Slice instruction, a Concat folding items)
    const NUM: usize = 10;
    const SLICE: usize = 1 + 3 * NUM;
    const ITEM: usize = 2 * (1 + NUM);
    // (a run of floats: tag, shift, count, map of 127 at most, two tables of 16 varints of 15 bits)
    const SPAN: usize = 1 + 1 + NUM + 1 + 127 + 2 * 16 * 3;
    // (an application: opcode, module, length of the parameters, the output's length, the memory
    // limit and a task limit; its inputs)
    const APPLY: usize = 1 + 4 * NUM + 2 * NUM + 2 * SLICE + (2 * SLICE + 1 + NUM);
    let block = aqr_formula::aqr::BLOCK;
    let over = |len: usize| len / block + 2;
    let mut n = 1024 + over(g.data) * SLICE + ITEM + over(input.len()) * 4;
    for p in pieces(input.len(), &g, SEGMENT) {
        n += ITEM
            + match p {
                Piece::Long(a, b) => over(b - a) * SLICE + 1 + NUM,
                Piece::Segment(spans) => APPLY + spans.len() * SPAN + 2,
            };
    }
    Some(n)
}

/// A segment coded ([`encode_sealing`]): its formula and what was made of it, None where none of
/// it was coded; and its bytes.
type Made<T> = (Option<(Formula, T)>, std::ops::Range<usize>);

/// The formula of file of tensors `input` (GGUF, `torch.save`'s, safetensors), or None where it is not one this crate takes. The segments
/// are coded `threads` at a time; the formula is the same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_in(input, threads, SEGMENT)
}

/// [`encode`] in segments of about `segment` bytes (the tests' of segments, at lengths shorter
/// than [`SEGMENT`]).
pub fn encode_in(input: &[u8], threads: usize, segment_bytes: usize) -> Option<Formula> {
    let made = |part: &mut Formula, _: &[u8]| aqr_formula::store::spill_streams(part);
    encode_sealing(input, threads, segment_bytes, &made, &mut |_, _, ()| {})
}

/// [`encode_in`], each segment coded made a formula of its own (its application and streams),
/// given to `made` on the worker that coded it, with the bytes it stands for, then to `seal` with
/// what `made` made of it, in order, before it is taken into the file's ([`Formula::embed`]): to
/// be sealed into the archive's sink, say, its streams then of the sink's (or to have its streams
/// spilled, as [`encode_in`] has them). The input of each piece is let go of once it is done
/// ([`aqr_formula::store::let_go`]).
pub fn encode_sealing<T: Send>(
    input: &[u8],
    threads: usize,
    segment_bytes: usize,
    made: &(dyn Fn(&mut Formula, &[u8]) -> T + Sync),
    seal: &mut dyn FnMut(&mut Formula, &[u8], T),
) -> Option<Formula> {
    let g = tensors::parse(input)?;
    if g.tensors.is_empty() {
        return None;
    }
    let pieces = pieces(input.len(), &g, segment_bytes.max(1 << 16));
    // (each tensor's model, of a sample of it, made by the first segment of it that needs it)
    let models: Vec<std::sync::OnceLock<Model>> = g.tensors.iter().map(|_| std::sync::OnceLock::new()).collect();
    let mut f = Formula::default();
    let mut nodes = vec![f.push(g.container.header(), input[..g.data].to_vec())];
    // (each segment's streams pushed as they come, moved to the store at once: a stream of a
    // class made of all of them, of a length not told, would be copied as its region grew)
    let mut any = false;
    // (a segment's formula and what `made` made of it, or None where none of it was coded, and
    // its bytes)
    let code = |spans: &[Span]| {
        let c = segment(input, spans, &g, &models);
        let (a, b) = (spans[0].range().start, spans.last().unwrap().range().end);
        let part = c.coded.then(|| {
            let mut part = Formula::default();
            let inputs = vec![
                part.push_as(SYMS, Kind::Compressed, c.syms),
                part.push_as(RAW, Kind::Compressed, c.raw),
                part.push_as(SIDE, Kind::Unknown, c.side),
            ];
            part.root = Node::apply(MODULE, c.params, inputs, (b - a) as u64);
            let t = made(&mut part, &input[a..b]);
            (part, t)
        });
        aqr_common::progress::done(&input[a..b]);
        (part, a..b)
    };
    let mut put = |piece: &Piece, c: Option<Made<T>>| {
        let (c, r) = match (piece, c) {
            (Piece::Long(a, b), _) => {
                aqr_common::progress::done(&input[*a..*b]);
                (None, *a..*b)
            }
            (Piece::Segment(_), Some(c)) => c,
            (Piece::Segment(_), None) => unreachable!("coded first"),
        };
        match c {
            Some((mut part, t)) => {
                any = true;
                seal(&mut part, &input[r.clone()], t);
                nodes.push(f.embed(part));
            }
            None => {
                let d = &input[r.clone()];
                let bytes = aqr_formula::store::find(d).unwrap_or_else(|| d.to_vec().into());
                nodes.push(f.push(DATA, bytes));
            }
        }
        // (read again only where it is archived as it is, or compared)
        aqr_formula::store::let_go(&input[r]);
    };
    // (the segments coded on the workers in order, as many at once as there are, each written
    // once those before it are: no wait for the slowest of a batch)
    aqr_common::pool::scope(threads.max(1), |s| {
        let workers = s.workers();
        let mut ahead = std::collections::VecDeque::new();
        for p in &pieces {
            if workers == 0 {
                put(p, if let Piece::Segment(spans) = p { Some(code(spans)) } else { None });
                continue;
            }
            if ahead.len() > workers {
                let (p, h): (&Piece, Option<aqr_common::pool::Handle<_>>) = ahead.pop_front().unwrap();
                put(p, h.map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))));
            }
            let h = match p {
                Piece::Segment(spans) => Some(s.spawn(|| code(spans))),
                Piece::Long(..) => None,
            };
            ahead.push_back((p, h));
        }
        for (p, h) in ahead {
            put(p, h.map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))));
        }
    });
    if !any {
        return None;
    }
    f.root = Node::concat(nodes);
    Some(f)
}

/// Whether formula `f` is one of [`encode`]'s, or holds one.
pub fn is(f: &Formula) -> bool {
    f.streams.iter().any(|s| s.class == SYMS)
}

/// The order-0 entropy of the symbols of `counts`, in bits (for the bench's reports).
pub fn entropy(counts: &[u64; 16]) -> f64 {
    rans::entropy(counts)
}
