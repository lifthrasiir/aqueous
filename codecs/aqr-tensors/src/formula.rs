//! aqr-tensors as a module of entry formulas (see aqr-formula): a file of tensors' header as it is
//! (GGUF's, `torch.save`'s, safetensors'), its tensor data in segments, each an application of
//! `tensors-recons` (FORMAT.md §2), and the long runs of bytes this crate does not code as they are.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::grid;
use crate::layout::{self, FLOAT_CHUNK, Float, FloatRun, LANES_CHUNK, LanesRun, Part, Quant, Run};
use crate::rans::{Encoder, Table};
use crate::tensors::{self, Data, Tensor, Tensors};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("tensors-recons", 3).with_plan(plan);

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

aqr_common::zopt_namespace!("tensors", "files of tensors: GGUF, PyTorch, safetensors (aqr-tensors)");

/// Bytes of a segment, about: of the output of an application (the default of [`SEGMENT_BYTES`]).
/// The decoder makes the applications of a file at once, 8 of them ahead and 256 MiB at most, so
/// 16 MiB keeps 8 at work; the encoder holds the symbols of as many as it has threads.
pub const SEGMENT: usize = 16 << 20;

aqr_common::zopt! {
    /// Bytes of a segment, about: of the output of an application ([`SEGMENT`]).
    pub static SEGMENT_BYTES: Opt<usize> =
        Opt::size("tensors:segment", SEGMENT, "bytes of a segment of tensor data, about");
    /// Bytes not coded of at least this many, between the tensors coded, are left out of the
    /// applications, as they are.
    static LONG_RAW: Opt<usize> =
        Opt::size("tensors:long-raw", 1 << 20, "bytes between coded tensors left out of the applications");
    /// A tensor (or a part of one in a segment) of fewer bytes is not coded.
    static MIN_CODED: Opt<usize> = Opt::size("tensors:min-coded", 4096, "tensors of fewer bytes are not coded");
    /// A tensor of at least this many bytes (or of a segment's, where that is less) starts a
    /// segment and ends its last one, so that the same tensor makes the same applications wherever
    /// it is ([`pieces`]): of another file, the archive stores their streams once.
    static ALIGN: Opt<usize> =
        Opt::size("tensors:align", SEGMENT, "tensors of at least this many bytes are segments of their own");
    /// A tensor of at least this many bytes of the same bytes as one before it in the file is
    /// that one's applications again, not coded again ([`copies`]).
    static COPY_MIN: Opt<usize> =
        Opt::size("tensors:copy-min", 64 << 10, "tensors of the same bytes as one before, of at least this, are not coded again");
    /// Whether coding comes to less than this share of the bytes as they are (of their 8 bits
    /// each), for a run to be coded.
    static PAYS: Opt<f64> = Opt::value("tensors:pays", 0.985, "a run is coded where under this share of its raw bits");
    /// The least gain of the mantissa's top bits, as a share of a float's bits, for them to be symbols.
    pub(crate) static MAN_GAIN: Opt<f64> =
        Opt::value("tensors:man-gain", 0.015, "least gain, as a share of a float's bits, to code mantissas");
    /// The sample of floats a model is chosen on (also of grids): windows of this many floats.
    pub(crate) static SAMPLE_WINDOW: Opt<usize> =
        Opt::value("tensors:man-window", 1024, "floats of each window of a sample for models and grids");
    /// The windows of the sample, spread over the tensor (at least 2).
    pub(crate) static SAMPLE_WINDOWS: Opt<usize> =
        Opt::value("tensors:man-windows", 8, "windows of a sample for models and grids, at least 2");
    /// The least gain of codes (zeros and subnormals apart), as a share of the run's bits, for them
    /// to be taken.
    static FINE_GAIN: Opt<f64> = Opt::value("tensors:fine-gain", 0.01, "least gain of codes, as a share of a run's bits");
    /// A run of floats of rows of fewer elements is not of row classes.
    static ROW_MIN: Opt<u64> = Opt::value("tensors:row-min", 64, "rows of fewer floats are not in classes");
    /// The least gain of row classes, as a share of the run's bits, for them to be taken.
    static ROW_GAIN: Opt<f64> = Opt::value("tensors:row-gain", 0.02, "least gain of row classes, as a share of a run's bits");
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

/// A piece of the root: a long run as it is, a segment, or a tensor of the same bytes as one before
/// it, of its bytes: the pieces of that one (an index range of them) again.
enum Piece {
    Long(usize, usize),
    Segment(Vec<Span>),
    Copy(std::ops::Range<usize>, usize, usize),
}

/// 64 bytes of `b` (not empty) spread from its first to its last: what tells tensors apart that
/// are not the same, mostly.
fn probe(b: &[u8]) -> [u8; 64] {
    std::array::from_fn(|k| b[(b.len() - 1) / 63 * k])
}

/// Keys of the tensors of file `d` that are segments of their own ([`ALIGN`], [`pieces`]): of
/// their type and a few of their bytes, the same for the same tensor in another file. Files of
/// a key in common are for one batch of the archive, which stores the streams of their
/// applications made alike once. Empty where `d` is not taken.
pub fn shared_keys(d: &[u8]) -> Vec<u64> {
    use std::hash::{Hash, Hasher};
    let Some(g) = tensors::parse(d) else { return Vec::new() };
    let align = ALIGN.get().min(SEGMENT_BYTES.get()) as u64;
    let mut keys: Vec<u64> = (g.tensors.iter().filter(|t| t.data.len() >= align))
        .map(|t| {
            let b = &d[t.start..t.start + t.data.len() as usize];
            let mut h = std::hash::DefaultHasher::new();
            (t.data, probe(b), &b[..4096.min(b.len())]).hash(&mut h);
            h.finish()
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Of each tensor of `g` (of file `d`), the one before it of the same bytes, if any, of at least
/// [`COPY_MIN`] bytes (a model's embeddings, tied to its output's weights, stored as both).
fn copies(d: &[u8], g: &Tensors) -> Vec<Option<usize>> {
    let min = COPY_MIN.get() as u64;
    let bytes = |t: &Tensor| &d[t.start..t.start + t.data.len() as usize];
    // (of each tensor, its type and length and a few of its bytes, those before it alike)
    let mut alike: std::collections::HashMap<(Data, [u8; 64]), Vec<usize>> = Default::default();
    let mut out = vec![None; g.tensors.len()];
    for (i, t) in g.tensors.iter().enumerate().filter(|(_, t)| t.data.len() >= min) {
        let b = bytes(t);
        let earlier = alike.entry((t.data, probe(b))).or_default();
        match earlier.iter().find(|&&j| bytes(&g.tensors[j]) == b) {
            Some(&j) => out[i] = Some(j),
            None => earlier.push(i),
        }
    }
    out
}

/// The pieces of the file after the header: the tensors coded cut into segments of about
/// `segment` bytes (at chunks), with the bytes between them. A tensor of [`ALIGN`] bytes or more,
/// or of copies, starts a segment and ends its last one; a copy ([`copies`]) is a [`Piece::Copy`].
fn pieces(d: &[u8], g: &Tensors, segment: usize) -> Vec<Piece> {
    let len = d.len();
    let copies = copies(d, g);
    // (or of a segment, where that is less: of the tests' short ones)
    let align = ALIGN.get().min(segment) as u64;
    let mut copied = vec![false; g.tensors.len()];
    copies.iter().flatten().for_each(|&j| copied[j] = true);
    // (of each tensor that is a segment of its own, its pieces)
    let mut own: Vec<Option<std::ops::Range<usize>>> = vec![None; g.tensors.len()];
    let mut out = Vec::new();
    let mut seg: Vec<Span> = Vec::new();
    let mut seg_len = 0usize;
    let close = |seg: &mut Vec<Span>, seg_len: &mut usize, out: &mut Vec<Piece>| {
        if !seg.is_empty() {
            out.push(Piece::Segment(std::mem::take(seg)));
        }
        *seg_len = 0;
    };
    let long_raw = LONG_RAW.get();
    let mut at = g.data;
    let raw = |a: usize, b: usize, seg: &mut Vec<Span>, seg_len: &mut usize, out: &mut Vec<Piece>| {
        if a == b {
            return;
        }
        if b - a >= long_raw {
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
        if let Some(j) = copies[ti] {
            close(&mut seg, &mut seg_len, &mut out);
            let of = own[j].clone().expect("a segment of its own");
            at = start + data.len() as usize;
            out.push(Piece::Copy(of, start, at));
            continue;
        }
        let alone = copied[ti] || data.len() >= align;
        if alone {
            close(&mut seg, &mut seg_len, &mut out);
        }
        let first = out.len();
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
        if alone {
            close(&mut seg, &mut seg_len, &mut out);
            own[ti] = Some(first..out.len());
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
    let (win, wins) = (SAMPLE_WINDOW.get().min(n), SAMPLE_WINDOWS.get());
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
    gain >= MAN_GAIN.get() * float_bits
}

/// Rounds of the classes of rows made better ([`cluster_rows`]) at most.
const ROW_ROUNDS: usize = 8;

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
    let (min_coded, pays) = (MIN_CODED.get(), PAYS.get());
    for &s in spans {
        let r = s.range();
        let bytes = &d[r.clone()];
        let run = match s {
            Span::Coded(_, _, _) if bytes.len() < min_coded => None,
            Span::Coded(at, Data::Float(f, n), ti) => {
                let t = &g.tensors[ti];
                let model = *models[ti].get_or_init(|| model(d, t));
                let gr = match model {
                    Model::Grid(cfg) => {
                        let tensor = &d[t.start..t.start + t.data.len() as usize];
                        let from = ((at - t.start) / f.bytes()) as u64;
                        let limit = pays * 8.0 * bytes.len() as f64;
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
                let rows = (t.outer > 0).then(|| (t.outer, ((at - t.start) / f.bytes()) as u64 % t.outer));
                gr.or_else(|| float(f, n as usize, bytes, man, rows, &mut enc, &mut tables, &mut c.raw, &mut c.side))
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
    bits < PAYS.get() * 8.0 * bytes as f64
}

/// A run of `n` floats `d` coded (FORMAT.md §4.2), the top bits of their mantissas symbols where
/// `man` and they come to enough less, its rows in classes where `rows` (the elements of a row and
/// the column of the first float) and that comes to enough less; None where it would not come to
/// less.
#[allow(clippy::too_many_arguments)]
fn float(
    f: Float,
    n: usize,
    d: &[u8],
    man: bool,
    rows: Option<(u64, u64)>,
    enc: &mut Encoder,
    tables: &mut Vec<Table>,
    raw: &mut Vec<u8>,
    side: &mut Vec<u8>,
) -> Option<Run> {
    match f.bytes() {
        1 => float_of::<1>(f, n, d, man, rows, enc, tables, raw, side),
        2 => float_of::<2>(f, n, d, man, rows, enc, tables, raw, side),
        4 => float_of::<4>(f, n, d, man, rows, enc, tables, raw, side),
        _ => float_of::<8>(f, n, d, man, rows, enc, tables, raw, side),
    }
}

/// The float `i` of `d`, of `FB` bytes.
#[inline(always)]
fn get<const FB: usize>(d: &[u8], i: usize) -> u64 {
    let mut b = [0u8; 8];
    b[..FB].copy_from_slice(&d[FB * i..FB * i + FB]);
    u64::from_le_bytes(b)
}

/// How a run of floats takes them apart (FORMAT.md §4.2), and what that comes to.
struct FloatPlan {
    bits: f64,
    shift: u32,
    whole: bool,
    fine: bool,
    map: [u16; 16],
    m: usize,
    /// Of each exponent (or code, where `fine`), its symbol.
    sym: Vec<u8>,
    escapes: u64,
    exp: Table,
    /// Of each count of bits kept, the floats.
    by_kept: Vec<u64>,
}

/// [`float`] of floats of `FB` bytes (the loops over every float specialised: they are most of
/// the encoder's time).
#[allow(clippy::too_many_arguments)]
fn float_of<const FB: usize>(
    f: Float,
    n: usize,
    d: &[u8],
    man: bool,
    rows: Option<(u64, u64)>,
    enc: &mut Encoder,
    tables: &mut Vec<Table>,
    raw: &mut Vec<u8>,
    side: &mut Vec<u8>,
) -> Option<Run> {
    let (m_bits, e_bits) = (f.man_bits(), f.exp_bits());
    let ne = 1usize << e_bits;
    let nc = f.codes() as usize;
    let mmask = (1u64 << m_bits) - 1;
    // (of each count of trailing zero bits of the mantissa, M for a mantissa of zero, the codes
    // counted, whose exponents are the codes below 2^E, 0 of the others; in parts, the counts of
    // a part in 32 bits)
    let tz = |v: u64| ((v & mmask) | 1 << m_bits).trailing_zeros() as usize;
    let mut hist = vec![0u64; (m_bits as usize + 1) * nc];
    let mut part_hist = vec![0u32; (m_bits as usize + 1) * nc];
    for part in d.chunks(FB * (1 << 20)) {
        for i in 0..part.len() / FB {
            let v = get::<FB>(part, i);
            part_hist[tz(v) * nc + layout::code(f, v) as usize] += 1;
        }
        for (h, p) in hist.iter_mut().zip(part_hist.iter_mut()) {
            *h += *p as u64;
            *p = 0;
        }
    }
    let by_tz: Vec<u64> = hist.chunks(nc).map(|h| h.iter().sum()).collect();
    // (the floats with fewer zeros than a shift: escapes whole, as many as there are; else the
    // escapes are of the exponents, the shift that of all floats)
    let short = |s: u32| by_tz[..s as usize].iter().sum::<u64>();
    let all_zero = (0..=m_bits).rev().find(|&s| short(s) == 0)?;
    // (the shift that saves the most with escapes whole: of `shift` bits of each float, less a
    // float's bytes and more for each short one)
    let best = (0..=m_bits)
        .max_by_key(|&s| (n as i64 * s as i64 - short(s) as i64 * (8 * FB as i64 + 8), std::cmp::Reverse(s)))?;
    // the cost of each way: its table, escapes and bits kept
    let plan = |shift: u32, whole: bool, fine: bool| {
        let keys = if fine { nc } else { ne };
        let key = |c: usize| if fine || c < ne { c } else { 0 };
        let kept = |c: usize| match fine {
            true => 1 + layout::code_man(f, shift, c as u32).unwrap_or(0),
            false => 1 + m_bits - shift,
        };
        let mut ex = vec![0u64; keys];
        for h in hist.chunks(nc).skip(shift as usize) {
            for c in 0..nc {
                ex[key(c)] += h[c];
            }
        }
        let short = short(shift);
        let mut order: Vec<usize> = (0..keys).filter(|&e| ex[e] > 0).collect();
        order.sort_by_key(|&e| (std::cmp::Reverse(ex[e]), e));
        let m = if order.len() <= 16 && short == 0 { order.len() } else { order.len().min(15) };
        let mut map = [0u16; 16];
        let mut sym = vec![15u8; keys];
        for (s, &e) in order.iter().take(m).enumerate() {
            map[s] = e as u16;
            sym[e] = s as u8;
        }
        let mut a = [0u64; 16];
        let mut by_kept = vec![0u64; m_bits as usize + 2];
        for e in 0..keys {
            a[sym[e] as usize] += ex[e];
            by_kept[kept(e) as usize] += ex[e];
        }
        a[15] += short;
        by_kept[(1 + m_bits - shift) as usize] += short;
        let escapes = if m < 16 { a[15] } else { 0 };
        let exp = Table::of_counts(&a);
        let eb = if whole { FB } else { f.exp_bytes(fine) } as u64;
        let kept_bits: u64 = by_kept.iter().enumerate().map(|(k, &c)| k as u64 * c).sum();
        let bits = exp.cost(&a) + 8.0 * (escapes * eb) as f64 + kept_bits as f64 + 400.0;
        FloatPlan { bits, shift, whole, fine, map, m, sym, escapes, exp, by_kept }
    };
    // (of codes only where that saves enough: their decoding is slower)
    let (a, b, c) = (plan(all_zero, false, false), plan(best, true, false), plan(all_zero, false, true));
    let p = if b.bits < a.bits { b } else { a };
    let p = if c.bits < (1.0 - FINE_GAIN.get()) * p.bits { c } else { p };
    let FloatPlan { mut bits, shift, whole, fine, map, m, ref sym, escapes, mut exp, ref by_kept } = p;
    let kept = 1 + m_bits - shift;
    let low = (1u64 << shift) - 1;
    // (a float's symbol: of its exponent or code, or the escape where it is short)
    let symbol = |v: u64| match fine {
        true => sym[layout::code(f, v) as usize],
        false if v & low != 0 => 15,
        false => sym[f.exp(v) as usize],
    };
    // (the bits a float keeps, and how many)
    let kept_of = |v: u64| match fine {
        true => {
            let c = layout::code(f, v);
            let k = 1 + layout::code_man(f, shift, c).unwrap();
            (layout::code_kept(f, shift, c, v), k)
        }
        false => (layout::kept_bits(f, shift, v), kept),
    };
    // (the top `t` bits of bits kept `k` of `k` bits, as a symbol: the bits shifted up where fewer)
    let top = |k: u64, kb: u32, t: u32| if kb >= t { k >> (kb - t) } else { k << (t - kb) };
    // the top bits of the mantissa: of each exponent symbol, the top 4 bits (or all) counted
    let (mut t, mut class, mut man_tables) = (0u32, [0u8; 16], Vec::new());
    if man {
        let t4 = kept.min(4);
        let mut counts = [[0u64; 16]; 16];
        for i in 0..n {
            let v = get::<FB>(d, i);
            let (k, kb) = kept_of(v);
            counts[symbol(v) as usize][top(k, kb, t4) as usize] += 1;
        }
        for tt in 1..=t4 {
            let mut c = [[0u64; 16]; 16];
            for s in 0..16 {
                for v in 0..16 {
                    c[s][v >> (t4 - tt)] += counts[s][v];
                }
            }
            let (_, cl, ts, b) = grid::class_tables(&c, 1 << tt);
            let saved: u64 = by_kept.iter().enumerate().map(|(k, &c)| (k as u64).min(tt as u64) * c).sum();
            let b = bits - saved as f64 + b;
            if b < bits.min(if t == 0 { 0.99 * bits } else { f64::INFINITY }) {
                (t, class, man_tables, bits) = (tt, cl, ts, b);
            }
        }
    }
    // the rows in classes, each its own table of the symbols
    let mut row_classes: Option<(u64, u64, Vec<u8>, Vec<Table>)> = None;
    if let Some((row, col)) = rows
        && row >= ROW_MIN.get().max(layout::MIN_CLASS_ROW)
        && (n as u64 + col) / row >= 4
    {
        let count = (col + n as u64).div_ceil(row) as usize;
        let mut h = vec![[0u64; 16]; count];
        for i in 0..n {
            h[((col + i as u64) / row) as usize][symbol(get::<FB>(d, i)) as usize] += 1;
        }
        let mag: Vec<f64> = (0..16)
            .map(|s| {
                let c = map[s] as i64;
                match fine {
                    _ if s >= m => 0.0,
                    true if c as usize >= ne => (c - ne as i64 - m_bits as i64) as f64,
                    true if c == 0 => -(m_bits as f64) - 2.0,
                    _ => c as f64,
                }
            })
            .collect();
        let whole_cost = exp.cost(&h.iter().fold([0u64; 16], |mut a, r| {
            (0..16).for_each(|s| a[s] += r[s]);
            a
        }));
        if let Some((cls, ts, b)) = cluster_rows(&h, &mag)
            && b < whole_cost - ROW_GAIN.get() * bits
        {
            bits += b - whole_cost;
            exp = ts[0];
            row_classes = Some((row, col, cls, ts[1..].to_vec()));
        }
    }
    if !pays(bits, d.len()) {
        return None;
    }
    let te = tables.len() as u32;
    tables.push(exp);
    let k_rows = match &row_classes {
        Some((_, _, _, ts)) => {
            tables.extend_from_slice(ts);
            ts.len() + 1
        }
        None => 1,
    };
    let tm = tables.len() as u32;
    tables.extend_from_slice(&man_tables);
    let rb = kept - t;
    let mut sa = [0u8; FLOAT_CHUNK];
    let mut sb: Vec<Vec<u8>> = vec![Vec::with_capacity(FLOAT_CHUNK); man_tables.len()];
    let mut by_row: Vec<Vec<u8>> = vec![Vec::with_capacity(FLOAT_CHUNK); k_rows];
    let mut bits_buf = vec![0u8; (FLOAT_CHUNK * kept as usize).div_ceil(8) + 16];
    raw.reserve((n * rb as usize).div_ceil(8));
    for c in (0..n).step_by(FLOAT_CHUNK) {
        let e = (n - c).min(FLOAT_CHUNK);
        let chunk = &d[FB * c..FB * (c + e)];
        if fine {
            bits_buf.fill(0);
            sb.iter_mut().for_each(|b| b.clear());
            let mut at = 0;
            for (i, sym) in sa[..e].iter_mut().enumerate() {
                let v = get::<FB>(chunk, i);
                let s = symbol(v);
                *sym = s;
                let (k, kb) = kept_of(v);
                if t > 0 {
                    sb[class[s as usize] as usize].push(top(k, kb, t) as u8);
                }
                let r = kb - kb.min(t);
                layout::put_bits(&mut bits_buf, at, r, k & ((1u64 << r) - 1));
                at += r as usize;
            }
            raw.extend_from_slice(&bits_buf[..at.div_ceil(8)]);
        } else if t == 0 && rb == 8 {
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
            let nr = (e * rb as usize).div_ceil(8);
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
        // (the classes of the rows starting in the chunk, and of the run's first, then the escapes)
        if let Some((row, col, cls, _)) = &row_classes {
            by_row.iter_mut().for_each(|b| b.clear());
            for (i, &s) in sa[..e].iter().enumerate() {
                let at = col + (c + i) as u64;
                let r = (at / row) as usize;
                if c + i == 0 || at.is_multiple_of(*row) {
                    side.push(cls[r]);
                }
                by_row[cls[r] as usize].push(s);
            }
        }
        if escapes > 0 {
            for i in (0..e).filter(|&i| sa[i] as usize >= m) {
                let v = get::<FB>(chunk, i);
                match (whole, fine) {
                    (true, _) => side.extend_from_slice(&chunk[FB * i..FB * i + FB]),
                    (false, true) => side.extend_from_slice(&layout::code(f, v).to_le_bytes()[..f.exp_bytes(true)]),
                    (false, false) => side.extend_from_slice(&f.exp(v).to_le_bytes()[..f.exp_bytes(false)]),
                }
            }
        }
        match &row_classes {
            Some(_) => by_row.iter().enumerate().for_each(|(k, b)| grid::put(enc, te + k as u32, b)),
            None => grid::put(enc, te, &sa[..e]),
        }
        for (k, b) in sb.iter().enumerate() {
            grid::put(enc, tm + k as u32, b);
        }
    }
    let rows = row_classes.map(|(row, col, _, tables)| layout::RowClasses { row, col, tables });
    let run = FloatRun { f, shift, n: n as u64, map, m: m as u8, whole, exp, t, class, man: man_tables, fine, rows };
    Some(Run::Float(run))
}

/// The rows of histograms `h` of symbols in classes (2, 4 or 8, the best), `mag` the magnitude of
/// each symbol (the classes started by it): of each row its class, the tables of the classes,
/// and the bits they come to (the tables' and the rows' classes counted); None where there are
/// too few rows.
fn cluster_rows(h: &[[u64; 16]], mag: &[f64]) -> Option<(Vec<u8>, Vec<Table>, f64)> {
    let rows = h.len();
    if rows < 4 {
        return None;
    }
    // (each row's mean magnitude, the rows in order of it)
    let mean = |r: &[u64; 16]| {
        let t: u64 = r.iter().sum();
        (0..16).map(|s| r[s] as f64 * mag[s]).sum::<f64>() / t.max(1) as f64
    };
    let mut order: Vec<usize> = (0..rows).collect();
    let means: Vec<f64> = h.iter().map(mean).collect();
    order.sort_by(|&a, &b| means[a].total_cmp(&means[b]).then(a.cmp(&b)));
    let mut best: Option<(Vec<u8>, Vec<Table>, f64)> = None;
    for k in [2usize, 4, 8] {
        if rows < 2 * k {
            break;
        }
        let mut cls = vec![0u8; rows];
        for (j, &r) in order.iter().enumerate() {
            cls[r] = (j * k / rows) as u8;
        }
        for _ in 0..ROW_ROUNDS {
            // (each class's costs of a symbol, of its counts made a little smoother; each row to
            // the class it costs least in)
            let mut sum = vec![[0u64; 16]; k];
            for (r, &c) in h.iter().zip(&cls) {
                (0..16).for_each(|s| sum[c as usize][s] += r[s]);
            }
            let cost: Vec<[f64; 16]> = sum
                .iter()
                .map(|c| {
                    let t = c.iter().sum::<u64>() as f64 + 16.0 * 0.5;
                    std::array::from_fn(|s| (t / (c[s] as f64 + 0.5)).log2())
                })
                .collect();
            let mut moved = false;
            for (r, c) in h.iter().zip(cls.iter_mut()) {
                let of = |j: usize| (0..16).filter(|&s| r[s] > 0).map(|s| r[s] as f64 * cost[j][s]).sum::<f64>();
                let mut pick = *c as usize;
                let mut low = of(pick);
                for j in 0..k {
                    let x = of(j);
                    if x < low {
                        (pick, low) = (j, x);
                    }
                }
                moved |= pick != *c as usize;
                *c = pick as u8;
            }
            if !moved {
                break;
            }
        }
        // (the classes of no rows left out, the others numbered in turn)
        let mut sum = vec![[0u64; 16]; k];
        for (r, &c) in h.iter().zip(&cls) {
            (0..16).for_each(|s| sum[c as usize][s] += r[s]);
        }
        let mut renum = [u8::MAX; 8];
        let mut used = 0u8;
        for j in 0..k {
            if sum[j].iter().any(|&x| x > 0) {
                renum[j] = used;
                used += 1;
            }
        }
        if used < 2 {
            continue;
        }
        cls.iter_mut().for_each(|c| *c = renum[*c as usize]);
        let tables: Vec<Table> = sum.iter().filter(|c| c.iter().any(|&x| x > 0)).map(Table::of_counts).collect();
        let bits = sum.iter().filter(|c| c.iter().any(|&x| x > 0)).zip(&tables).map(|(c, t)| t.cost(c)).sum::<f64>()
            + 300.0 * tables.len() as f64
            + rows as f64 * (1.0 + (used as f64).log2());
        if best.as_ref().is_none_or(|b| bits < b.2) {
            best = Some((cls, tables, bits));
        }
    }
    best
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
    let block = aqr_archive::BLOCK;
    let over = |len: usize| len / block + 2;
    let n = 1024 + over(g.data) * SLICE + ITEM + over(input.len()) * 4;
    // (of each piece, its bound: a copy's those of the pieces it is again)
    let mut bounds: Vec<usize> = Vec::new();
    for p in pieces(input, &g, SEGMENT_BYTES.get()) {
        let b = ITEM
            + match p {
                Piece::Long(a, b) => over(b - a) * SLICE + 1 + NUM,
                Piece::Segment(spans) => APPLY + spans.len() * SPAN + 2,
                Piece::Copy(of, _, _) => bounds[of].iter().sum(),
            };
        bounds.push(b);
    }
    Some(n + bounds.iter().sum::<usize>())
}

/// A segment coded ([`encode_sealing`]): its formula and what was made of it, None where none of
/// it was coded; and its bytes.
type Made<T> = (Option<(Formula, T)>, std::ops::Range<usize>);

/// The formula of file of tensors `input` (GGUF, `torch.save`'s, safetensors), or None where it is not one this crate takes. The segments
/// are coded `threads` at a time; the formula is the same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_in(input, threads, SEGMENT_BYTES.get())
}

/// [`encode`] in segments of about `segment` bytes (the tests' of segments, at lengths shorter
/// than [`SEGMENT_BYTES`]).
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
    seal: &mut (dyn FnMut(&mut Formula, &[u8], T) + Send),
) -> Option<Formula> {
    let g = tensors::parse(input)?;
    if g.tensors.is_empty() {
        return None;
    }
    let pieces = pieces(input, &g, segment_bytes.max(1 << 16));
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
            // (each of a segment's streams a block of its own, encoded with the segment, on its
            // worker: the side streams of all of them a stream of the archive, whose blocks of
            // 128 MiB were compressed one at a time as they filled, held up the segments; at
            // 0.25-1.25 s each of the order the blocks are written in, most of gemma-4-31B's time)
            part.streams.iter_mut().for_each(|s| s.apart = true);
            let t = made(&mut part, &input[a..b]);
            (part, t)
        });
        aqr_common::progress::done(&input[a..b]);
        (part, a..b)
    };
    // (of each piece put, its node in `nodes`)
    let mut node_of: Vec<usize> = Vec::with_capacity(pieces.len());
    let put = |piece: &Piece, c: Option<Made<T>>| {
        let (c, r) = match (piece, c) {
            (Piece::Long(a, b), _) => {
                aqr_common::progress::done(&input[*a..*b]);
                (None, *a..*b)
            }
            (Piece::Copy(of, a, b), _) => {
                // (the nodes of the pieces it is again, their streams the same)
                let again: Vec<Node> = node_of[of.clone()].iter().map(|&k| nodes[k].clone()).collect();
                node_of.push(nodes.len());
                nodes.push(Node::concat(again));
                aqr_common::progress::done(&input[*a..*b]);
                aqr_formula::store::let_go(&input[*a..*b]);
                return;
            }
            (Piece::Segment(_), Some(c)) => c,
            (Piece::Segment(_), None) => unreachable!("coded first"),
        };
        node_of.push(nodes.len());
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
    // (the segments coded on the pool's threads, those given back as others finish taken in too
    // (`par_map`: a scope reserves only the workers free as it starts, none where another file of
    // the batch took them, a shard of 3.4 GB then coded on one thread), each put by the thread
    // that coded the last of those before it, and at most `threads` ahead of those put, so many
    // segments' symbols held at once: no wait for the slowest of a batch)
    let ahead = threads.max(1);
    // (the next piece to put, those coded waiting for it, whether one failed, and whether one is
    // putting: one at a time, outside the lock, as the others go on)
    let order = std::sync::Mutex::new((0usize, std::collections::BTreeMap::new(), false, false));
    let put = std::sync::Mutex::new(put);
    let turn = std::sync::Condvar::new();
    // (a piece that panics fails those waiting for it: they would wait for ever)
    struct Failed<'a, P>(&'a std::sync::Mutex<P>, &'a std::sync::Condvar, fn(&mut P));
    impl<P> Drop for Failed<'_, P> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                (self.2)(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
                self.1.notify_all();
            }
        }
    }
    aqr_common::pool::par_map(pieces.len(), ahead, |i| {
        let _failed = Failed(&order, &turn, |o| o.2 = true);
        let mut o = order.lock().unwrap();
        while i >= o.0 + ahead && !o.2 {
            // (waking to give up with the others where the attempt is over its heap: the piece
            // waited for may be one its check stopped before it began)
            o = turn.wait_timeout(o, std::time::Duration::from_millis(20)).unwrap().0;
            aqr_common::allowance::check();
        }
        if o.2 {
            return;
        }
        drop(o);
        let c = match &pieces[i] {
            Piece::Segment(spans) => Some(code(spans)),
            Piece::Long(..) | Piece::Copy(..) => None,
        };
        let mut o = order.lock().unwrap();
        o.1.insert(i, c);
        if o.3 {
            return;
        }
        o.3 = true;
        while let Some(c) = {
            let n = o.0;
            o.1.remove(&n)
        } {
            let n = o.0;
            drop(o);
            (put.lock().unwrap())(&pieces[n], c);
            o = order.lock().unwrap();
            o.0 += 1;
            turn.notify_all();
        }
        o.3 = false;
    });
    drop(order);
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
