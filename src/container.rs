//! File format, compression and decompression.
//!
//! ```text
//! "PDFR" version(u8) mode(u8)
//! mode 0: the file as it is
//! mode 1: varint(length) u64-LE(xxh3 of the file) | section × NSECTIONS
//!   section: varint(raw length) u8(0 stored | 1 zstd) [varint(coded length)] data
//! ```
//!
//! The file is cut into the skeleton and *items*, ranges of stream data coded on their own.
//! The table lists the items in file order, each as the gap of skeleton before it, its mode
//! and the mode's fields:
//! - `RAW`: varint(length); the bytes are in the raw section.
//! - `DUP`: varint(items back); a copy of an earlier item's bytes.
//! - `TOK`, `FULL`: a FlateDecode stream, as its tokens ([`crate::tok`]) or as
//!   flate-recons' fast representation: u8(flags: bit 0 zlib header, bit 1 Adler-32),
//!   [2 header bytes], u8(class), varint(inflated length), and for `FULL` varint(metadata
//!   length). `FULL` streams' inflated data is in their class's pool, the metadata in the
//!   meta section; `TOK` streams are in the token section, in order.
//! - `JPEG`: varint(length); a jpeg-recons container in the jpeg section.
//!
//! With more than one thread ([`Options::threads`]), items are coded and restored ahead on a
//! pool, and joined in order on the calling thread (with the token streams, which share one
//! coder); the output is the same whatever the number of threads.

use crate::image::{self, Geometry};
use crate::kind::{self, CLASSES, Class, Filter};
use crate::lex::Obj;
use crate::objects::Objects;
use crate::scan;
use crate::tok;
use crate::varint::{self, Reader};
use crate::{Error, Result, flate};

const MAGIC: &[u8; 4] = b"PDFR";
const VERSION: u8 = 1;

pub const RAW: u8 = 0;
pub const DUP: u8 = 1;
pub const FULL: u8 = 2;
pub const JPEG: u8 = 3;
pub const TOK: u8 = 4;
pub const PNG: u8 = 5;
pub const NMODES: usize = 6;
pub const MODES: [&str; NMODES] = ["raw", "dup", "full", "jpeg", "tok", "png"];

const S_TABLE: usize = 0;
const S_SKEL: usize = 1;
const S_RAW: usize = 2;
const S_META: usize = 3;
const S_JPEG: usize = 4;
const S_TOK: usize = 5;
const S_PNG: usize = 6;
const S_POOL: usize = 7;
const NSECTIONS: usize = S_POOL + CLASSES.len();

/// Streams shorter than this stay in the skeleton.
const MIN_ITEM: usize = 64;

#[derive(Debug, Clone)]
pub struct Options {
    /// Target speed in MB/s of PDF, both ways, on one thread: coding that would not fit the
    /// time this gives is skipped (by estimates, so the output does not depend on timing).
    /// Infinite: no limit.
    pub speed: f64,
    /// Decompress the result and compare; on a mismatch, store the streams as they are.
    pub verify: bool,
    /// zstd level of the skeleton and the pools.
    pub level: i32,
    /// Modes allowed for FlateDecode streams (for experiments): tokens, full.
    pub tok: bool,
    pub full: bool,
    /// Images with PNG predictors through png-recons.
    pub png: bool,
    /// Threads (1: the items one after the other on the calling thread, jpeg-recons on
    /// rayon's global pool as before). The output does not depend on it.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { speed: 100.0, verify: false, level: 9, tok: true, full: true, png: true, threads: 1 }
    }
}

/// What the compressor did.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Per mode: items, input bytes, output bytes (payload only; pools count separately).
    pub modes: [(usize, usize, usize); NMODES],
    pub skeleton: (usize, usize),
    pub pools: usize,
    pub meta: usize,
    pub tok: usize,
    /// Items that a coder failed on (stored raw instead).
    pub failed: usize,
    /// Seconds spent scanning and planning, parsing FlateDecode streams, on flate-recons,
    /// on jpeg-recons, and writing the sections.
    pub times: [f64; 5],
    /// Whether verification failed (the file was coded again without models).
    pub verify_failed: bool,
    /// Per mode and class (`CLASSES`, then one for streams of no class): what was coded
    /// and how long it took, for calibrating the cost model.
    pub cost: [[Measure; 7]; NMODES],
    /// Seconds spent finishing the token coder (tables and rANS).
    pub tok_finish: f64,
    /// Output bytes per mode and class (`TOK`: estimated; `FULL`: metadata plus the pool).
    pub class_out: [[f64; CLASSES.len() + 1]; NMODES],
    /// Estimated ns of the choice of modes (the larger of the two ways per item).
    pub predicted: f64,
    /// Every item: mode, class, raw and inflated bytes, seconds.
    pub items: Vec<(u8, u8, usize, usize, f64)>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Measure {
    pub n: usize,
    pub raw: usize,
    pub plain: usize,
    pub secs: f64,
}

impl Measure {
    pub fn add(&mut self, o: &Measure) {
        self.n += o.n;
        self.raw += o.raw;
        self.plain += o.plain;
        self.secs += o.secs;
    }
}

/// What decompression did, per mode and class (as [`Stats::cost`]).
#[derive(Debug, Clone, Default)]
pub struct DStats {
    pub cost: [[Measure; 7]; NMODES],
    /// Seconds reading the sections (zstd) and setting up the token decoder.
    pub setup: f64,
    pub items: Vec<(u8, u8, usize, usize, f64)>,
}

pub fn compress(file: &[u8]) -> Vec<u8> {
    compress_with(file, &Options::default(), &mut Stats::default())
}

/// Compresses `file`; never fails: whatever cannot be coded is stored.
pub fn compress_with(file: &[u8], opts: &Options, st: &mut Stats) -> Vec<u8> {
    let attempt = |o: &Options| {
        std::panic::catch_unwind(|| {
            let mut s = Stats::default();
            let r = try_compress(file, o, &mut s);
            (r, s)
        })
        .ok()
        .and_then(|(r, s)| r.map(|r| (r, s)))
    };
    let mut res = attempt(opts);
    if opts.verify
        && let Some((out, _)) = &res
        && decompress_threads(out, opts.threads).as_deref() != Ok(file)
    {
        let o = Options { tok: false, full: false, png: false, ..opts.clone() };
        res = attempt(&o).map(|(r, mut s)| {
            s.verify_failed = true;
            (r, s)
        });
    }
    match res {
        Some((out, s)) if out.len() < file.len() + 6 => {
            *st = s;
            out
        }
        _ => stored(file),
    }
}

/// Like [`compress_with`] without the safety nets (panics propagate, nothing is verified),
/// for testing.
pub fn compress_unchecked(file: &[u8], opts: &Options, st: &mut Stats) -> Vec<u8> {
    try_compress(file, opts, st).unwrap_or_else(|| stored(file))
}

fn stored(file: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file.len() + 6);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, 0]);
    out.extend_from_slice(file);
    out
}

/// A range of the file to code on its own.
struct Item {
    start: usize,
    len: usize,
    plan: Plan,
}

#[derive(Clone, Copy, PartialEq)]
enum Plan {
    Raw,
    Dup(usize),
    /// A FlateDecode stream: its class, estimated inflated size, the mode chosen, and for
    /// images their geometry.
    Flate(Class, f64, u8, Option<Geometry>),
    /// A JPEG: whether it is progressive, its number of 8×8 blocks, whether to code it.
    Jpeg(bool, u64, bool),
}

/// Costs on an Apple M4 for the time budget, fitted to the per-item times of the corpus
/// (`scripts/fit-costs.py`): ns per item, per byte of stream and per inflated byte, each
/// way; and expected sizes.
mod cost {
    use crate::kind::Class;

    /// The plan's time is this much under the budget: estimates of a file's time scatter by
    /// about 10% (content the features do not see), and the target is per file.
    pub const MARGIN: f64 = 1.1;
    /// ns per byte of file: scanning; ns per byte of skeleton: zstd at the default level.
    pub const SCAN: f64 = 0.2;
    pub const ZSTD_C: f64 = 10.0;
    /// ns per inflated byte of the pools: zstd compression, decompression.
    pub const POOL: (f64, f64) = (7.0, 1.0);

    /// (per item, per byte, per inflated byte or JPEG block), compressing and decompressing.
    pub type Time = ([f64; 3], [f64; 3]);

    pub const JPEG: Time = ([375e3, 10.5, 15.0], [111e3, 10.9, 19.6]);
    /// Baseline JPEGs: output / input by size (small ones gain little).
    pub fn jpeg_ratio(n: f64) -> f64 {
        match n {
            ..1024.0 => 0.996,
            ..4096.0 => 0.954,
            ..16384.0 => 0.856,
            ..65536.0 => 0.81,
            _ => 0.79,
        }
    }
    pub const JPEG_PROG: Time = ([301e3, 21.0, 42.3], [27e3, 19.7, 37.1]);
    pub const JPEG_PROG_RATIO: f64 = 0.89;
    pub const PNG: Time = ([213e3, 13.6, 0.28], [199e3, 13.5, 0.25]);
    pub const PNG_RATIO: f64 = 0.80;

    /// Token mode. Per byte, with the coder's final pass (1.9 ns) and the decoder's setup
    /// (0.7 ns), which are per file.
    pub fn tok(c: Class) -> (Time, f64) {
        match c {
            Class::Content => (([4.8e3, 13.1, 0.0], [5.7e3, 13.7, 0.0]), 0.95),
            Class::Font => (([7.1e3, 12.1, 0.05], [7.7e3, 10.3, 0.22]), 0.997),
            Class::Image => (([7.7e3, 10.3, 0.11], [7.1e3, 11.8, 0.07]), 0.945),
            Class::ObjStm => (([5.7e3, 13.8, 0.04], [6.9e3, 13.3, 0.07]), 0.915),
            Class::XRef => (([7.9e3, 17.6, 0.2], [5.2e3, 18.7, 0.22]), 0.955),
            Class::Data => (([5.1e3, 12.8, 0.63], [8.6e3, 8.7, 1.27]), 1.01),
        }
    }

    /// Token mode for images coded as pixels.
    pub const TOK_PIXELS: (Time, f64) = (([11.3e3, 12.6, 0.11], [12.5e3, 12.7, 0.07]), 0.846);

    /// flate-recons' fast representation; the size is the pool's share and the metadata.
    pub fn full(c: Class) -> (Time, f64) {
        match c {
            Class::Content => (([11.7e3, 52.2, 2.35], [7.6e3, 40.2, 1.47]), 0.774),
            Class::Font => (([8.4e3, 22.6, 7.56], [8.3e3, 12.7, 7.0]), 0.805),
            Class::Image => (([70e3, 32.9, 1.72], [17.4e3, 24.7, 1.63]), 0.86),
            Class::ObjStm => (([13.4e3, 84.5, 1.83], [10.9e3, 35.0, 1.54]), 0.833),
            Class::XRef => (([9.6e3, 84.7, 3.8], [9.7e3, 29.7, 3.3]), 0.878),
            Class::Data => (([26.1e3, 17.7, 8.0], [9.9e3, 12.9, 5.35]), 0.973),
        }
    }

    /// The larger of the two times of an item.
    pub fn time(t: &Time, n: f64, plain: f64) -> f64 {
        let f = |c: &[f64; 3]| c[0] + c[1] * n + c[2] * plain;
        f(&t.0).max(f(&t.1))
    }

    /// Inflated bytes per DEFLATE byte, when the dictionary does not tell.
    pub fn expansion(c: Class) -> f64 {
        match c {
            Class::Content => 4.5,
            Class::Font => 2.2,
            Class::Image => 11.8,
            Class::ObjStm => 4.6,
            Class::XRef => 4.5,
            Class::Data => 1.4,
        }
    }
}

/// The pixel layout of an image without a predictor, for the token coder's pixel model.
fn pixels(g: &Geometry) -> Option<tok::Pixels> {
    (g.predictor == 1 && g.bpc == 8 && g.colors <= 4).then(|| tok::Pixels { bpp: g.colors as usize, stride: g.row() })
}

/// Whether the file has an `/Encrypt` entry (a trailer's, normally).
fn encrypted(file: &[u8]) -> bool {
    memchr::memmem::find_iter(file, b"/Encrypt").any(|i| {
        let rest = &file[i + 8..];
        let k = rest.iter().position(|&b| !crate::lex::is_white(b)).unwrap_or(rest.len());
        k > 0 && rest.get(k).is_some_and(|b| b.is_ascii_digit()) || rest[k..].starts_with(b"<<")
    })
}

/// The inflated size of a FlateDecode stream, from its dictionary or a prior.
fn plain_estimate(d: &Obj, c: Class, data: &[u8]) -> f64 {
    let len = data.len();
    let int = |k: &[u8]| d.get(k).and_then(Obj::as_int).filter(|&v| v > 0);
    let parms = d.get(b"DecodeParms").map(|p| match p {
        Obj::Array(a) => a.first().cloned().unwrap_or(Obj::Null),
        p => p.clone(),
    });
    let pint = |k: &[u8]| parms.as_ref().and_then(|p| p.get(k)).and_then(Obj::as_int).filter(|&v| v > 0);
    let est = match c {
        Class::Image => (|| {
            let (w, h) = (int(b"Width")?, int(b"Height")?);
            let bpc = int(b"BitsPerComponent").unwrap_or(1);
            let colors = pint(b"Colors").unwrap_or_else(|| match d.get(b"ColorSpace").and_then(Obj::as_name) {
                Some(b"DeviceRGB" | b"CalRGB") => 3,
                Some(b"DeviceCMYK") => 4,
                Some(_) => 1,
                None if d.get(b"ColorSpace").is_some() => 3,
                None => 1,
            });
            let pred = pint(b"Predictor").unwrap_or(1) >= 10;
            let row = (w * colors * bpc + 7) / 8 + pred as i64;
            Some((row * h) as f64)
        })(),
        Class::Font => {
            int(b"Length1").map(|l| (l + int(b"Length2").unwrap_or(0) + int(b"Length3").unwrap_or(0)) as f64)
        }
        _ => None,
    };
    // Else the first block's codes tell, except for fonts and other data (where the class
    // is closer: `examples/expansion.rs`).
    let est = est.or_else(|| match c {
        Class::Font | Class::Data => None,
        _ => flate::expansion(data).map(|e| e * len as f64),
    });
    est.filter(|&e| e < len as f64 * 1032.0).unwrap_or(len as f64 * cost::expansion(c))
}

fn try_compress(file: &[u8], opts: &Options, st: &mut Stats) -> Option<Vec<u8>> {
    let t0 = std::time::Instant::now();
    let streams = scan::streams(file);
    let mut items: Vec<Item> = vec![];
    let mut seen: std::collections::HashMap<u64, Vec<usize>> = Default::default();
    let mut covered = 0usize;
    // Encrypted files' streams look random: nothing to code but repeats.
    let encrypted = encrypted(file);
    let mut objs = Objects::new(file);
    for s in &streams {
        let data = &file[s.data.clone()];
        if data.len() < MIN_ITEM {
            continue;
        }
        let f = kind::filter(&s.dict);
        let keep = match f {
            Filter::None => true,
            Filter::Other => s
                .dict
                .get(b"Filter")
                .map(Obj::names)
                .is_some_and(|n| matches!(n[0], b"ASCIIHexDecode" | b"AHx" | b"ASCII85Decode" | b"A85")),
            _ => false,
        };
        if keep {
            continue;
        }
        let h = xxhash_rust::xxh3::xxh3_64(data);
        let same =
            seen.get(&h).and_then(|v| v.iter().copied().find(|&i| file[items[i].start..][..items[i].len] == *data));
        seen.entry(h).or_default().push(items.len());
        let plan = match (same, f) {
            (Some(i), _) => Plan::Dup(i),
            _ if encrypted => Plan::Raw,
            (None, Filter::Flate) => {
                let c = kind::class(&s.dict);
                let g = (c == Class::Image).then(|| image::geometry(&s.dict, &mut objs)).flatten();
                let plain = match g {
                    Some(g) => ((g.row() + (g.predictor >= 10) as usize) * g.height as usize) as f64,
                    None => plain_estimate(&s.dict, c, data),
                };
                Plan::Flate(c, plain, RAW, g)
            }
            (None, Filter::Dct) => {
                let (prog, blocks) = image::jpeg_frame(data).unwrap_or((false, 0));
                Plan::Jpeg(prog, blocks, false)
            }
            _ => Plan::Raw,
        };
        covered += data.len();
        items.push(Item { start: s.data.start, len: data.len(), plan });
    }
    let skel_len = file.len() - covered;
    let budget =
        if opts.speed.is_finite() { file.len() as f64 * 1e3 / (opts.speed * cost::MARGIN) } else { f64::INFINITY };
    let fixed = file.len() as f64 * cost::SCAN + skel_len as f64 * cost::ZSTD_C;
    st.predicted = fixed + choose(&mut items, budget - fixed, opts);
    st.times[0] += t0.elapsed().as_secs_f64();

    // Code the items: each on its own (on the pool, with more than one thread), then into
    // the sections, in order.
    let pool = pool(opts.threads);
    let fr_opts = flate_recons::fast::Options { threads: 1, ..Default::default() };
    let mut b = Sections::new(skel_len, items.len());
    match &pool {
        None => {
            for (i, it) in items.iter().enumerate() {
                let t_item = std::time::Instant::now();
                let d = code_item(file, it, &fr_opts);
                b.add(file, i, it, d, st, t_item, 0.0);
            }
        }
        Some(pool) => {
            // Memory an item takes while coded (the parsed or decoded form), roughly.
            let est = |i: usize| -> Option<usize> {
                let it = &items[i];
                match it.plan {
                    Plan::Flate(_, plain, TOK | FULL | PNG, _) => Some(it.len + (3.0 * plain) as usize),
                    Plan::Jpeg(_, blocks, true) => Some(it.len + 160 * blocks as usize),
                    _ => None,
                }
            };
            let job = |i: usize| {
                let t = std::time::Instant::now();
                let d = code_item(file, &items[i], &fr_opts);
                (d, t.elapsed().as_secs_f64())
            };
            pipeline(pool, items.len(), est, job, |i, d| {
                let t = std::time::Instant::now();
                let (d, secs) = d.unwrap_or_else(|| (code_item(file, &items[i], &fr_opts), 0.0));
                b.add(file, i, &items[i], d, st, t, secs);
                Ok::<(), ()>(())
            })
            .ok()?;
        }
    }
    let Sections { table, mut skel, raw, meta, jpeg, png, tokens, pools, at, .. } = b;
    skel.extend_from_slice(&file[at..]);

    let t = std::time::Instant::now();
    let level = opts.level;
    // The token coder's final pass; with a pool, zstd of the table, the skeleton and the
    // pools beside it.
    let finish = |tokens: tok::Encoder| {
        let t = std::time::Instant::now();
        let r = tokens.finish();
        (r, t.elapsed().as_secs_f64())
    };
    let ((tok, tok_sizes), tok_secs, mut z) = match &pool {
        None => {
            let (r, s) = finish(tokens);
            (r, s, None)
        }
        Some(pool) => {
            let mut v: Vec<&[u8]> = vec![&table, &skel];
            v.extend(pools.iter().map(|p| &p[..]));
            let ((r, s), z) = pool.install(|| {
                use rayon::prelude::*;
                rayon::join(|| finish(tokens), || v.par_iter().map(|d| zstd_of(d, level)).collect::<Vec<_>>())
            });
            (r, s, Some(z.into_iter()))
        }
    };
    // The zstd form of the next of those sections, made above or now.
    let mut zs = |d: &[u8]| match &mut z {
        Some(z) => z.next().expect("a section"),
        None => zstd_of(d, level),
    };
    st.class_out[TOK as usize] = tok_sizes;
    st.tok_finish = tok_secs;
    // The token items' share of it, by bytes, for the cost fit.
    let tok_bytes: usize = st.items.iter().filter(|i| i.0 == TOK).map(|i| i.2).sum();
    if tok_bytes > 0 {
        let per = st.tok_finish / tok_bytes as f64;
        for i in st.items.iter_mut().filter(|i| i.0 == TOK) {
            i.4 += per * i.2 as f64;
            st.cost[TOK as usize][i.1 as usize].secs += per * i.2 as f64;
        }
    }
    let mut out = Vec::with_capacity(file.len() / 2);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, 1]);
    varint::put(&mut out, file.len() as u64);
    out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(file).to_le_bytes());
    put_section(&mut out, &table, zs(&table));
    let k = out.len();
    put_section(&mut out, &skel, zs(&skel));
    st.skeleton = (skel.len(), out.len() - k);
    put_section(&mut out, &raw, None);
    put_section(&mut out, &meta, None);
    st.meta = meta.len();
    put_section(&mut out, &jpeg, None);
    put_section(&mut out, &tok, None);
    put_section(&mut out, &png, None);
    st.tok = tok.len();
    let k = out.len();
    for (c, pool) in pools.iter().enumerate() {
        let k = out.len();
        put_section(&mut out, pool, zs(pool));
        st.class_out[FULL as usize][c] += (out.len() - k) as f64;
    }
    st.pools = out.len() - k;
    st.times[4] += t.elapsed().as_secs_f64();
    Some(out)
}

/// A pool of `threads` threads, if more than one.
fn pool(threads: usize) -> Option<rayon::ThreadPool> {
    if threads <= 1 {
        return None;
    }
    rayon::ThreadPoolBuilder::new().num_threads(threads).build().ok()
}

/// Memory in flight in [`pipeline`] (estimated); one item at least.
const IN_FLIGHT: usize = 256 << 20;

/// Hands `consume` the indices `0..n` in order, on the calling thread, each with `job`'s
/// result for it if `est` gives it an estimate of memory: those jobs run on `pool`, ahead in
/// order, up to a bound in number and memory. A job's panic is resumed here; once this
/// returns, jobs not started are skipped.
fn pipeline<T: Send, E>(
    pool: &rayon::ThreadPool,
    n: usize,
    est: impl Fn(usize) -> Option<usize>,
    job: impl Fn(usize) -> T + Sync,
    mut consume: impl FnMut(usize, Option<T>) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Relaxed);
        }
    }
    let cap = 4 * pool.current_num_threads() + 4;
    let stop = AtomicBool::new(false);
    let (job, stop) = (&job, &stop);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, std::thread::Result<T>)>();
    pool.in_place_scope_fifo(|s| {
        let _stop = Stop(stop);
        // Spawned and not consumed: (index, estimate), in order.
        let mut flight = std::collections::VecDeque::new();
        let mut bytes = 0usize;
        let mut done = std::collections::HashMap::new();
        let mut next = 0;
        for i in 0..n {
            while next < n && flight.len() < cap {
                let Some(e) = est(next) else {
                    next += 1;
                    continue;
                };
                if !flight.is_empty() && bytes.saturating_add(e) > IN_FLIGHT {
                    break;
                }
                bytes = bytes.saturating_add(e);
                flight.push_back((next, e));
                let (k, tx) = (next, tx.clone());
                s.spawn_fifo(move |_| {
                    if !stop.load(Relaxed) {
                        let _ = tx.send((k, std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(k)))));
                    }
                });
                next += 1;
            }
            let r = match flight.front() {
                Some(&(k, e)) if k == i => {
                    flight.pop_front();
                    bytes -= e;
                    Some(loop {
                        if let Some(r) = done.remove(&i) {
                            break r;
                        }
                        match rx.recv().expect("a job's result") {
                            (k, Ok(r)) => done.insert(k, r),
                            (_, Err(p)) => std::panic::resume_unwind(p),
                        };
                    })
                }
                _ => None,
            };
            consume(i, r)?;
        }
        Ok(())
    })
}

/// An item coded on its own, before it goes in the sections.
enum Coded {
    /// Stored as it is; whether a coder failed on it.
    Raw(bool),
    Dup(usize),
    /// A FlateDecode stream: its table fields, the parsed stream for the token coder, its
    /// class, pixel layout and the bytes it covers.
    Tok(Vec<u8>, Box<flate_recons::fast::inflate::Flat>, Class, Option<tok::Pixels>, usize),
    /// A FlateDecode stream: its table fields, inflated data and metadata, class and the
    /// bytes it covers.
    Full(Vec<u8>, Vec<u8>, Vec<u8>, Class, usize),
    /// Table fields, png-recons container, class.
    Png(Vec<u8>, Vec<u8>, Class),
    Jpeg(Vec<u8>),
}

/// An item's coded form, its inflated bytes (or JPEG blocks) and the seconds of each phase
/// spent on it (as [`Stats::times`]).
struct Done {
    coded: Coded,
    plain_n: usize,
    times: [f64; 5],
}

fn code_item(file: &[u8], it: &Item, fr_opts: &flate_recons::fast::Options) -> Done {
    let data = &file[it.start..it.start + it.len];
    let mut times = [0f64; 5];
    let mut plain_n = 0usize;
    let coded = match it.plan {
        Plan::Dup(j) => Coded::Dup(j),
        Plan::Flate(c, _, m @ (TOK | FULL), g) => {
            let px = if m == TOK { g.as_ref().and_then(pixels) } else { None };
            let t = std::time::Instant::now();
            let hdr = 2 * flate::zlib_header(data) as usize;
            let parsed = flate_recons::fast::inflate::parse(&data[hdr..]);
            times[1] += t.elapsed().as_secs_f64();
            let coded = parsed.ok().and_then(|mut f| {
                let end = data.len() - f.stream.trailing.len();
                let adler = f.stream.trailing.len() >= 4 && data[end..end + 4] == flate::adler32(&f.data).to_be_bytes();
                f.stream.trailing.clear();
                plain_n = f.data.len();
                let flags = (hdr as u8 / 2) | ((adler as u8) << 1) | ((px.is_some() as u8) << 2);
                let mut fields = vec![m, flags];
                fields.extend_from_slice(&data[..hdr]);
                fields.push(c as u8);
                varint::put(&mut fields, f.data.len() as u64);
                if let Some(px) = px {
                    varint::put(&mut fields, px.bpp as u64);
                    varint::put(&mut fields, px.stride as u64);
                }
                let len = end + 4 * adler as usize;
                if m == TOK {
                    return Some(Coded::Tok(fields, Box::new(f), c, px, len));
                }
                let t = std::time::Instant::now();
                let r = flate_recons::fast::deconstruct_with(&data[hdr..end], fr_opts);
                times[2] += t.elapsed().as_secs_f64();
                let (plain, meta) = r.ok()?;
                varint::put(&mut fields, meta.len() as u64);
                Some(Coded::Full(fields, plain, meta, c, len))
            });
            coded.unwrap_or(Coded::Raw(true))
        }
        Plan::Flate(c, _, PNG, Some(g)) => {
            let (ct, depth) = image::png_format(&g).unwrap();
            let t = std::time::Instant::now();
            let p = png_recons::compress(&image::wrap(g.width, g.height, ct, depth, data));
            times[3] += t.elapsed().as_secs_f64();
            if p.len() < data.len() {
                plain_n = (g.row() + 1) * g.height as usize;
                let mut fields = vec![PNG, ct, depth];
                varint::put(&mut fields, g.width as u64);
                varint::put(&mut fields, g.height as u64);
                varint::put(&mut fields, p.len() as u64);
                Coded::Png(fields, p, c)
            } else {
                Coded::Raw(false)
            }
        }
        Plan::Jpeg(_, blocks, true) => {
            let t = std::time::Instant::now();
            let p = jpeg_recons::container::compress(data);
            times[3] += t.elapsed().as_secs_f64();
            if p.len() < data.len() {
                plain_n = blocks as usize;
                Coded::Jpeg(p)
            } else {
                Coded::Raw(false)
            }
        }
        _ => Coded::Raw(false),
    };
    Done { coded, plain_n, times }
}

/// The sections as the items go in, in order.
struct Sections {
    table: Vec<u8>,
    skel: Vec<u8>,
    raw: Vec<u8>,
    meta: Vec<u8>,
    jpeg: Vec<u8>,
    png: Vec<u8>,
    tokens: tok::Encoder,
    pools: Vec<Vec<u8>>,
    /// The end of the last item.
    at: usize,
    /// Bytes each item covers once coded.
    cov: Vec<usize>,
}

impl Sections {
    fn new(skel_len: usize, items: usize) -> Self {
        Sections {
            table: vec![],
            skel: Vec::with_capacity(skel_len),
            raw: vec![],
            meta: vec![],
            jpeg: vec![],
            png: vec![],
            tokens: tok::Encoder::default(),
            pools: vec![vec![]; CLASSES.len()],
            at: 0,
            cov: Vec::with_capacity(items),
        }
    }

    /// Adds item `i`, coded as `d`; its time is `secs` and since `t_item`.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        file: &[u8],
        i: usize,
        it: &Item,
        d: Done,
        st: &mut Stats,
        t_item: std::time::Instant,
        secs: f64,
    ) {
        let data = &file[it.start..it.start + it.len];
        varint::put(&mut self.table, (it.start - self.at) as u64);
        self.skel.extend_from_slice(&file[self.at..it.start]);
        for (a, b) in st.times.iter_mut().zip(&d.times) {
            *a += b;
        }
        let raw = |s: &mut Self| {
            s.table.push(RAW);
            varint::put(&mut s.table, it.len as u64);
            s.raw.extend_from_slice(data);
            (RAW, it.len, it.len)
        };
        let (mode, len, out_len) = match d.coded {
            Coded::Dup(j) => {
                self.table.push(DUP);
                varint::put(&mut self.table, (i - j) as u64);
                (DUP, self.cov[j], 0)
            }
            Coded::Tok(fields, f, c, px, len) => match self.tokens.add(c, &f, px) {
                Ok(()) => {
                    self.table.extend_from_slice(&fields);
                    (TOK, len, 0)
                }
                Err(_) => {
                    st.failed += 1;
                    raw(self)
                }
            },
            Coded::Full(fields, plain, m, c, len) => {
                self.table.extend_from_slice(&fields);
                self.pools[c as usize].extend_from_slice(&plain);
                self.meta.extend_from_slice(&m);
                st.class_out[FULL as usize][c as usize] += m.len() as f64;
                (FULL, len, m.len())
            }
            Coded::Png(fields, p, c) => {
                self.table.extend_from_slice(&fields);
                self.png.extend_from_slice(&p);
                st.class_out[PNG as usize][c as usize] += p.len() as f64;
                (PNG, it.len, p.len())
            }
            Coded::Jpeg(p) => {
                self.table.push(JPEG);
                varint::put(&mut self.table, p.len() as u64);
                self.jpeg.extend_from_slice(&p);
                (JPEG, it.len, p.len())
            }
            Coded::Raw(failed) => {
                st.failed += failed as usize;
                raw(self)
            }
        };
        let e = &mut st.modes[mode as usize];
        e.0 += 1;
        e.1 += len;
        e.2 += out_len;
        // Pixel streams count as of no class, progressive JPEGs as images.
        let ci = match it.plan {
            Plan::Jpeg(true, ..) => Class::Image as usize,
            Plan::Flate(_, _, TOK, Some(g)) if mode == TOK && pixels(&g).is_some() => 6,
            Plan::Flate(c, ..) => c as usize,
            _ => 6,
        };
        let plain_n = d.plain_n;
        let secs = secs + t_item.elapsed().as_secs_f64();
        st.cost[mode as usize][ci].add(&Measure { n: 1, raw: len, plain: plain_n, secs });
        st.items.push((mode, ci as u8, len, plain_n, secs));
        self.at = it.start + len;
        self.cov.push(len);
    }
}

/// Picks a mode per item: the smallest estimated size plus `λ` times the estimated time
/// (the larger of compression and decompression), with the smallest `λ` that keeps the
/// total time within `budget` ns. Returns the estimated time of the choice.
fn choose(items: &mut [Item], budget: f64, opts: &Options) -> f64 {
    // Per item: (mode, size, time) of each option.
    type Choices = Vec<(u8, f64, f64)>;
    let mut opts_of: Vec<(usize, Choices)> = vec![];
    for (i, it) in items.iter().enumerate() {
        let len = it.len as f64;
        let mut v = vec![(RAW, len, 0.0)];
        match it.plan {
            Plan::Flate(c, plain, _, g) => {
                if opts.tok {
                    let (t, r) = if g.as_ref().and_then(pixels).is_some() { cost::TOK_PIXELS } else { cost::tok(c) };
                    v.push((TOK, len * r, cost::time(&t, len, plain)));
                }
                if opts.full {
                    let (mut t, r) = cost::full(c);
                    t.0[2] += cost::POOL.0;
                    t.1[2] += cost::POOL.1;
                    v.push((FULL, len * r, cost::time(&t, len, plain)));
                }
                if opts.png && g.as_ref().and_then(image::png_format).is_some() {
                    v.push((PNG, len * cost::PNG_RATIO, cost::time(&cost::PNG, len, plain)));
                }
            }
            Plan::Jpeg(prog, blocks, _) => {
                let (t, r) =
                    if prog { (cost::JPEG_PROG, cost::JPEG_PROG_RATIO) } else { (cost::JPEG, cost::jpeg_ratio(len)) };
                v.push((JPEG, len * r, cost::time(&t, len, blocks as f64)))
            }
            _ => continue,
        }
        opts_of.push((i, v));
    }
    let pick = |lambda: f64, v: &[(u8, f64, f64)]| -> (u8, f64) {
        let b = v.iter().min_by(|a, b| (a.1 + lambda * a.2).total_cmp(&(b.1 + lambda * b.2))).unwrap();
        (b.0, b.2)
    };
    let total = |lambda: f64| opts_of.iter().map(|(_, v)| pick(lambda, v).1).sum::<f64>();
    let lambda = if total(0.0) <= budget {
        0.0
    } else if budget <= 0.0 {
        f64::INFINITY
    } else {
        // Bisection over log λ (bytes per ns).
        let (mut lo, mut hi) = (1e-9f64, 1e9f64);
        for _ in 0..60 {
            let mid = (lo * hi).sqrt();
            if total(mid) > budget { lo = mid } else { hi = mid }
        }
        hi
    };
    let mut spent = 0.0;
    for (i, v) in &opts_of {
        let m = if lambda.is_finite() { pick(lambda, v).0 } else { RAW };
        spent += v.iter().find(|o| o.0 == m).map_or(0.0, |o| o.2);
        items[*i].plan = match (items[*i].plan, m) {
            (Plan::Flate(c, p, _, g), m) => Plan::Flate(c, p, m, g),
            (Plan::Jpeg(prog, b, _), m) => Plan::Jpeg(prog, b, m == JPEG),
            (p, _) => p,
        };
    }
    spent
}

/// zstd of a section, if smaller.
fn zstd_of(data: &[u8], level: i32) -> Option<Vec<u8>> {
    if data.len() < 32 {
        return None;
    }
    zstd::bulk::compress(data, level).ok().filter(|z| z.len() < data.len())
}

/// A section, stored or as `z` (its zstd form).
fn put_section(out: &mut Vec<u8>, data: &[u8], z: Option<Vec<u8>>) {
    varint::put(out, data.len() as u64);
    match z {
        Some(z) => {
            out.push(1);
            varint::put(out, z.len() as u64);
            out.extend_from_slice(&z);
        }
        None => {
            out.push(0);
            out.extend_from_slice(data);
        }
    }
}

/// A section's length and its data: stored, or zstd (`true`).
fn read_section<'a>(r: &mut Reader<'a>, max: usize) -> Result<(usize, &'a [u8], bool)> {
    let len = r.usize(max)?;
    match r.u8()? {
        0 => Ok((len, r.bytes(len)?, false)),
        1 => {
            let n = r.usize(r.s.len())?;
            Ok((len, r.bytes(n)?, true))
        }
        _ => Err(Error::Corrupt),
    }
}

fn get_section((len, data, z): (usize, &[u8], bool)) -> Result<Vec<u8>> {
    if !z {
        return Ok(data.to_vec());
    }
    let d = zstd::bulk::decompress(data, len).map_err(|_| Error::Corrupt)?;
    if d.len() != len {
        return Err(Error::Corrupt);
    }
    Ok(d)
}

/// Bound on any length read, against corrupt sizes.
const MAX_LEN: usize = 1 << 40;

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    decompress_with(src, &mut DStats::default())
}

/// [`decompress`] with `threads` threads (the result does not depend on it).
pub fn decompress_threads(src: &[u8], threads: usize) -> Result<Vec<u8>> {
    decompress_with_threads(src, &mut DStats::default(), threads)
}

pub fn decompress_with(src: &[u8], ds: &mut DStats) -> Result<Vec<u8>> {
    decompress_with_threads(src, ds, 1)
}

/// An item of the table, with its data in the sections.
struct Entry<'a> {
    /// The skeleton before it.
    gap: std::ops::Range<usize>,
    mode: u8,
    /// Class (as [`Stats::cost`]) and inflated bytes, for the statistics.
    ci: usize,
    plain_n: usize,
    op: Op<'a>,
}

enum Op<'a> {
    Raw(&'a [u8]),
    /// Items back.
    Dup(usize),
    /// A FlateDecode stream: zlib header, class, inflated length, pixel layout, whether the
    /// Adler-32 follows, and for `FULL` the inflated data and metadata.
    Flate(&'a [u8], usize, usize, Option<tok::Pixels>, bool, Option<(&'a [u8], &'a [u8])>),
    Png(&'a [u8]),
    Jpeg(&'a [u8]),
}

/// Where the items' data is next in each section.
#[derive(Default)]
struct Cursors {
    sk: usize,
    rw: usize,
    mt: usize,
    jp: usize,
    pn: usize,
    pool: [usize; CLASSES.len()],
}

/// The next `n` bytes of `v` from `at`.
fn take<'a>(v: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = at.checked_add(n).filter(|&e| e <= v.len()).ok_or(Error::Corrupt)?;
    let r = &v[*at..end];
    *at = end;
    Ok(r)
}

/// The next item of the table, the `index`th; `len` is the file's length.
fn entry<'a>(t: &mut Reader<'a>, sec: &'a [Vec<u8>], cur: &mut Cursors, len: usize, index: usize) -> Result<Entry<'a>> {
    let gap = t.usize(len)?;
    let skel = &sec[S_SKEL];
    let s0 = cur.sk;
    take(skel, &mut cur.sk, gap)?;
    let mode = t.u8()?;
    let (mut ci, mut plain_n) = (6, 0);
    let op = match mode {
        RAW => {
            let n = t.usize(len)?;
            Op::Raw(take(&sec[S_RAW], &mut cur.rw, n)?)
        }
        DUP => {
            let back = t.usize(index)?;
            if back == 0 {
                return Err(Error::Corrupt);
            }
            Op::Dup(back)
        }
        m @ (FULL | TOK) => {
            let flags = t.u8()?;
            if flags & !7 != 0 || (m == FULL && flags & 4 != 0) {
                return Err(Error::Corrupt);
            }
            let hdr = if flags & 1 != 0 { t.bytes(2)? } else { &[] };
            let c = t.u8()? as usize;
            if c >= CLASSES.len() {
                return Err(Error::Corrupt);
            }
            let n = t.usize(len.saturating_mul(1032))?;
            (ci, plain_n) = (if flags & 4 != 0 { 6 } else { c }, n);
            let px =
                if flags & 4 != 0 { Some(tok::Pixels { bpp: t.usize(64)?, stride: t.usize(1 << 34)? }) } else { None };
            let full = if m == FULL {
                let mlen = t.usize(MAX_LEN)?;
                let plain = take(&sec[S_POOL + c], &mut cur.pool[c], n)?;
                Some((plain, take(&sec[S_META], &mut cur.mt, mlen)?))
            } else {
                None
            };
            Op::Flate(hdr, c, n, px, flags & 2 != 0, full)
        }
        PNG => {
            ci = Class::Image as usize;
            let _format = t.bytes(2)?;
            let (_w, _h) = (t.varint()?, t.varint()?);
            let n = t.usize(len)?;
            Op::Png(take(&sec[S_PNG], &mut cur.pn, n)?)
        }
        JPEG => {
            let n = t.usize(len)?;
            Op::Jpeg(take(&sec[S_JPEG], &mut cur.jp, n)?)
        }
        _ => return Err(Error::Corrupt),
    };
    Ok(Entry { gap: s0..cur.sk, mode, ci, plain_n, op })
}

/// An item restored on its own: the bytes are `buf[range]`, then the Adler-32 if any.
struct Restored {
    buf: Vec<u8>,
    range: std::ops::Range<usize>,
    adler: Option<u32>,
}

/// Restores a `FULL`, `PNG` or `JPEG` item on its own.
fn restore(e: &Entry) -> Result<Restored> {
    let whole = |buf: Vec<u8>| Restored { range: 0..buf.len(), buf, adler: None };
    match e.op {
        Op::Flate(_, _, _, _, adler, Some((plain, meta))) => {
            let d = flate_recons::fast::reconstruct_with_threads(plain, meta, 1).map_err(|_| Error::Corrupt)?;
            Ok(Restored { adler: adler.then(|| flate::adler32(plain)), ..whole(d) })
        }
        Op::Png(p) => {
            let file = png_recons::decompress(p).map_err(|_| Error::Corrupt)?;
            let u = image::unwrap(&file).ok_or(Error::Corrupt)?;
            let at = u.as_ptr() as usize - file.as_ptr() as usize;
            Ok(Restored { range: at..at + u.len(), buf: file, adler: None })
        }
        Op::Jpeg(p) => Ok(whole(jpeg_recons::container::decompress(p).map_err(|_| Error::Corrupt)?)),
        _ => unreachable!("not restored on its own"),
    }
}

/// Memory an entry takes while restored, roughly, if restored on its own.
fn restore_estimate(e: &Entry) -> Option<usize> {
    match e.op {
        Op::Flate(.., Some((plain, meta))) => Some(plain.len().saturating_mul(2).saturating_add(meta.len())),
        Op::Png(p) | Op::Jpeg(p) => Some(p.len().saturating_mul(16)),
        _ => None,
    }
}

/// The output so far, and what restoring the items in order needs.
struct Out<'a> {
    out: Vec<u8>,
    skel: &'a [u8],
    tokens: Option<tok::DecoderState<'a>>,
    /// Where each item went in the output.
    placed: Vec<(usize, usize)>,
    len: usize,
}

impl Out<'_> {
    /// Appends item `e`, restored as `r` if it was on its own; its time is `secs` and
    /// since `t_item`.
    fn add(
        &mut self,
        e: &Entry,
        r: Option<Result<Restored>>,
        ds: &mut DStats,
        t_item: std::time::Instant,
        secs: f64,
    ) -> Result<()> {
        let out = &mut self.out;
        out.extend_from_slice(&self.skel[e.gap.clone()]);
        let start = out.len();
        let restored = || r.unwrap_or_else(|| restore(e));
        match e.op {
            Op::Raw(d) => out.extend_from_slice(d),
            Op::Dup(back) => {
                let (s, n) = self.placed[self.placed.len() - back];
                out.extend_from_within(s..s + n);
            }
            Op::Flate(hdr, c, n, px, adler, full) => {
                out.extend_from_slice(hdr);
                let a = if full.is_some() {
                    let r = restored()?;
                    out.extend_from_slice(&r.buf[r.range]);
                    r.adler
                } else {
                    let dec = self.tokens.as_mut().ok_or(Error::Corrupt)?;
                    let (d, plain) = dec.next(CLASSES[c], n, px)?;
                    out.extend_from_slice(&d);
                    adler.then(|| flate::adler32(&plain))
                };
                if let Some(a) = a {
                    out.extend_from_slice(&a.to_be_bytes());
                }
            }
            Op::Png(_) | Op::Jpeg(_) => {
                let r = restored()?;
                out.extend_from_slice(&r.buf[r.range]);
            }
        }
        self.placed.push((start, out.len() - start));
        let secs = secs + t_item.elapsed().as_secs_f64();
        ds.cost[e.mode as usize][e.ci].add(&Measure { n: 1, raw: out.len() - start, plain: e.plain_n, secs });
        ds.items.push((e.mode, e.ci as u8, out.len() - start, e.plain_n, secs));
        if out.len() > self.len {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}

/// [`decompress_with`] with `threads` threads: the sections are decoded, and the items but
/// the token streams (one coded stream) restored, on a pool; the rest is in order here.
pub fn decompress_with_threads(src: &[u8], ds: &mut DStats, threads: usize) -> Result<Vec<u8>> {
    let t_setup = std::time::Instant::now();
    let mut r = Reader::new(src);
    if r.bytes(4)? != MAGIC || r.u8()? != VERSION {
        return Err(Error::Corrupt);
    }
    match r.u8()? {
        0 => return Ok(r.rest().to_vec()),
        1 => {}
        _ => return Err(Error::Corrupt),
    }
    let len = r.usize(MAX_LEN)?;
    let sum = r.u64le()?;
    let pool = pool(threads);
    // Sections are at most the file, but the pools (inflated data, at most 1032 times
    // the DEFLATE data).
    let max = |k: usize| if k >= S_POOL { len.saturating_mul(1032) } else { len.saturating_add(1 << 20) };
    let sec: Vec<Vec<u8>> = match &pool {
        None => {
            let mut sec = Vec::with_capacity(NSECTIONS);
            for k in 0..NSECTIONS {
                sec.push(get_section(read_section(&mut r, max(k))?)?);
            }
            sec
        }
        Some(pool) => {
            let mut v = Vec::with_capacity(NSECTIONS);
            for k in 0..NSECTIONS {
                v.push(read_section(&mut r, max(k))?);
            }
            use rayon::prelude::*;
            pool.install(|| v.into_par_iter().map(get_section).collect::<Result<_>>())?
        }
    };
    if !r.at_end() {
        return Err(Error::Corrupt);
    }
    let mut t = Reader::new(&sec[S_TABLE]);
    let mut cur = Cursors::default();
    let tokens = if sec[S_TOK].is_empty() { None } else { Some(tok::DecoderState::new(&sec[S_TOK])?) };
    let mut o = Out { out: Vec::with_capacity(len), skel: &sec[S_SKEL], tokens, placed: vec![], len };
    ds.setup += t_setup.elapsed().as_secs_f64();
    match &pool {
        None => {
            while !t.at_end() {
                let t_item = std::time::Instant::now();
                let e = entry(&mut t, &sec, &mut cur, len, o.placed.len())?;
                o.add(&e, None, ds, t_item, 0.0)?;
            }
        }
        Some(pool) => {
            let mut entries = vec![];
            while !t.at_end() {
                entries.push(entry(&mut t, &sec, &mut cur, len, entries.len())?);
            }
            let job = |i: usize| {
                let t = std::time::Instant::now();
                let r = restore(&entries[i]);
                (r, t.elapsed().as_secs_f64())
            };
            let est = |i: usize| restore_estimate(&entries[i]);
            pipeline(pool, entries.len(), est, job, |i, r| {
                let t = std::time::Instant::now();
                let (r, secs) = r.map_or((None, 0.0), |(r, s)| (Some(r), s));
                o.add(&entries[i], r, ds, t, secs)
            })?;
        }
    }
    let mut out = o.out;
    out.extend_from_slice(&o.skel[cur.sk..]);
    if o.tokens.is_some_and(|d| !d.finished()) || out.len() != len || xxhash_rust::xxh3::xxh3_64(&out) != sum {
        return Err(Error::Corrupt);
    }
    Ok(out)
}
