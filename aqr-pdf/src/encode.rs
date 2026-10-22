//! The encoder of the formula module (`FORMAT.md` specifies the module).
//!
//! The file is cut into the skeleton and *items*, ranges of stream data coded on their own,
//! each with a mode ([`crate::formula`]): `RAW`, `DUP`, `TOK`, `FULL`, `PNG` or `JPEG`. The
//! planner picks the modes by a cost model within a time budget; the sections of the module
//! (the table, the skeleton, the raw bytes, the pools of inflated data and the token coder's
//! parts) and the formulas of the other modules (aqr-flate, aqr-png, aqr-jpeg) make the
//! formula.
//!
//! With more than one thread ([`Options::threads`]), items are coded ahead on a pool, and
//! joined in order on the calling thread (with the token streams, which share one coder); the
//! formula is the same whatever the number of threads.

use crate::flate;
use crate::formula::{DUP, FULL, JPEG, NMODES, PNG, RAW, TOK};
use crate::image::{self, Geometry};
use crate::kind::{self, CLASSES, Class, Filter};
use crate::lex::Obj;
use crate::objects::Objects;
use crate::scan;
use crate::tok;
use crate::varint;
use aqr_formula::{Formula, Node};

/// Streams shorter than this stay in the skeleton.
const MIN_ITEM: usize = 64;

#[derive(Debug, Clone)]
pub struct Options {
    /// Target speed in MB/s of PDF, both ways, on one thread: coding that would not fit the
    /// time this gives is skipped (by estimates, so the output does not depend on timing).
    /// Infinite: no limit.
    pub speed: f64,
    /// Evaluate the formula and compare; on a mismatch, code the file again without the
    /// models (tokens, full, PNG).
    pub verify: bool,
    /// zstd level the table, the skeleton and the pools are measured at, for judging whether
    /// the formula is smaller than the file.
    pub level: i32,
    /// Modes allowed for FlateDecode streams (for experiments): tokens, full.
    pub tok: bool,
    pub full: bool,
    /// Images with PNG predictors through aqr-png.
    pub png: bool,
    /// Threads (1: the items one after the other on the calling thread, aqr-jpeg on
    /// rayon's global pool as before). The output does not depend on it.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { speed: 100.0, verify: false, level: 9, tok: true, full: true, png: true, threads: 1 }
    }
}

/// What the encoder did.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Per mode: items, input bytes, output bytes (payload only; pools count separately).
    pub modes: [(usize, usize, usize); NMODES],
    /// Items that a coder failed on (stored raw instead).
    pub failed: usize,
    /// Seconds spent scanning and planning, parsing FlateDecode streams, on aqr-flate and on
    /// aqr-jpeg or aqr-png.
    pub times: [f64; 4],
    /// Estimated ns of the choice of modes (the larger of the two ways per item).
    pub predicted: f64,
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

/// Costs on an Apple M4 for the time budget, fitted to the per-item times of the corpus:
/// ns per item, per byte of stream and per inflated byte, each way; and expected sizes.
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

    /// aqr-flate's fast representation; the size is the pool's share and the metadata.
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

/// Cuts the file into items and picks their modes; returns them and the length of the skeleton.
fn plan(file: &[u8], opts: &Options, st: &mut Stats) -> (Vec<Item>, usize) {
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
    (items, skel_len)
}

/// Codes the items: each on its own (on the pool, with more than one thread), then into
/// the sections `b`, in order.
fn code_items(
    file: &[u8],
    items: &[Item],
    st: &mut Stats,
    pool: &Option<rayon::ThreadPool>,
    b: &mut Sections,
) -> Option<()> {
    let fr_opts = aqr_flate::Options { threads: 1, ..Default::default() };
    match pool {
        None => {
            for (i, it) in items.iter().enumerate() {
                let d = code_item(file, it, &fr_opts);
                b.add(file, i, it, d, st);
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
            pipeline(
                pool,
                items.len(),
                est,
                |i| code_item(file, &items[i], &fr_opts),
                |i, d| {
                    let d = d.unwrap_or_else(|| code_item(file, &items[i], &fr_opts));
                    b.add(file, i, &items[i], d, st);
                    Ok::<(), ()>(())
                },
            )
            .ok()?;
        }
    }
    Some(())
}

/// The formula of `file` (FORMAT.md, *Formula module*): the sections are streams or slots as
/// they are, and the FlateDecode, PNG and JPEG items are applications of the other modules.
/// `None` if no item is coded (only raw items and repeats: the formula would be the file cut
/// up), or if the streams would not be smaller than the file, with the table, the skeleton and
/// the pools measured by their zstd at `opts.level` (the pools hold inflated data, so they are
/// larger than the file).
pub fn try_formula(file: &[u8], opts: &Options, st: &mut Stats) -> Option<Formula> {
    let (items, skel_len) = plan(file, opts, st);
    let pool = pool(opts.threads);
    let mut b = Sections::new(skel_len, items.len());
    code_items(file, &items, st, &pool, &mut b)?;
    let Sections { table, mut skel, raw, meta, tokens, pools, at, fm, .. } = b;
    skel.extend_from_slice(&file[at..]);
    let (toks, _) = tokens.finish();
    let Parts { mut f, deflate, png, jpeg } = fm;
    if [FULL, JPEG, TOK, PNG].iter().all(|&m| st.modes[m as usize].0 == 0) {
        return None;
    }
    let mut zipped: Vec<&[u8]> = vec![&table, &skel];
    zipped.extend(pools.iter().map(|p| &p[..]));
    let zlen = |d: &&[u8]| zstd_of(d, opts.level).map_or(d.len(), |z| z.len());
    let saved: usize = zipped.iter().map(|d| d.len()).sum::<usize>()
        - match &pool {
            Some(pool) => pool.install(|| {
                use rayon::prelude::*;
                zipped.par_iter().map(zlen).sum::<usize>()
            }),
            None => zipped.iter().map(zlen).sum(),
        };
    for (c, pool) in pools.into_iter().enumerate() {
        f.streams[F_POOL + c].data = pool;
    }
    f.streams[F_META].data = meta;
    let mut inputs = vec![f.push("pdf/table", table), f.push("pdf/skeleton", skel), f.push("pdf/raw", raw)];
    inputs.extend([Node::concat(deflate), Node::concat(png), Node::concat(jpeg)]);
    for (part, name) in toks.into_iter().zip(TOK_CLASSES) {
        inputs.push(f.push(name, part));
    }
    f.root = Node::apply(crate::formula::MODULE, Vec::new(), inputs, file.len() as u64);
    (streams_len(&f) - saved < file.len()).then_some(f)
}

/// The classes of the token coder's streams in a formula.
const TOK_CLASSES: [&str; tok::PARTS] =
    ["pdf/tok-layout", "pdf/tok-tables", "pdf/tok-symbols", "pdf/tok-bits", "pdf/tok-headers"];

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

/// What the table says of a FlateDecode item, besides its mode.
struct FlateHead {
    zlib: Option<[u8; 2]>,
    /// The Adler-32 after the DEFLATE data, as stored.
    adler: Option<[u8; 4]>,
    class: Class,
    /// Inflated bytes.
    plain: usize,
    px: Option<tok::Pixels>,
    /// Bytes of DEFLATE data.
    deflate: usize,
}

impl FlateHead {
    /// The table fields of a `TOK` item: flags, zlib header, class, inflated length, pixel
    /// layout.
    fn tok_fields(&self) -> Vec<u8> {
        let flags = self.zlib.is_some() as u8 | (self.adler.is_some() as u8) << 1 | (self.px.is_some() as u8) << 2;
        let mut fields = vec![TOK, flags];
        fields.extend(self.zlib.iter().flatten());
        fields.push(self.class as u8);
        varint::put(&mut fields, self.plain as u64);
        if let Some(px) = self.px {
            varint::put(&mut fields, px.bpp as u64);
            varint::put(&mut fields, px.stride as u64);
        }
        fields
    }

    /// The table fields of a `FULL` item: flags, zlib header, DEFLATE length,
    /// Adler-32 (the inflated data is not there to compute it from).
    fn full_fields(&self) -> Vec<u8> {
        let mut fields = vec![FULL, self.zlib.is_some() as u8 | (self.adler.is_some() as u8) << 1];
        fields.extend(self.zlib.iter().flatten());
        varint::put(&mut fields, self.deflate as u64);
        fields.extend(self.adler.iter().flatten());
        fields
    }
}

/// An item coded on its own, before it goes in the sections.
enum Coded {
    /// Stored as it is; whether a coder failed on it.
    Raw(bool),
    Dup(usize),
    /// A FlateDecode stream: its table fields, the parsed stream for the token coder and the
    /// bytes it covers.
    Tok(FlateHead, Box<aqr_flate::inflate::Flat>, usize),
    /// A FlateDecode stream: its table fields, inflated data and metadata and the bytes it
    /// covers.
    Full(FlateHead, Vec<u8>, Vec<u8>, usize),
    /// The formula of the PNG file that wraps the data.
    Png(Formula),
    /// The formula of the JPEG.
    Jpeg(Formula),
}

/// Bytes of all streams of `f`.
fn streams_len(f: &Formula) -> usize {
    f.streams.iter().map(|s| s.data.len()).sum()
}

/// An item's coded form and the seconds of each phase spent on it (as [`Stats::times`]).
struct Done {
    coded: Coded,
    times: [f64; 4],
}

/// The images are coded as formulas of their own (aqr-png, aqr-jpeg).
fn code_item(file: &[u8], it: &Item, fr_opts: &aqr_flate::Options) -> Done {
    let data = &file[it.start..it.start + it.len];
    let mut times = [0f64; 4];
    let coded = match it.plan {
        Plan::Dup(j) => Coded::Dup(j),
        Plan::Flate(c, _, m @ (TOK | FULL), g) => {
            let px = if m == TOK { g.as_ref().and_then(pixels) } else { None };
            let t = std::time::Instant::now();
            let hdr = 2 * flate::zlib_header(data) as usize;
            let parsed = aqr_flate::inflate::parse(&data[hdr..]);
            times[1] += t.elapsed().as_secs_f64();
            let coded = parsed.ok().and_then(|mut f| {
                let end = data.len() - f.stream.trailing.len();
                let adler = f.stream.trailing.len() >= 4 && data[end..end + 4] == flate::adler32(&f.data).to_be_bytes();
                f.stream.trailing.clear();
                let head = FlateHead {
                    zlib: (hdr == 2).then(|| [data[0], data[1]]),
                    adler: adler.then(|| data[end..end + 4].try_into().unwrap()),
                    class: c,
                    plain: f.data.len(),
                    px,
                    deflate: end - hdr,
                };
                let len = end + 4 * adler as usize;
                if m == TOK {
                    return Some(Coded::Tok(head, Box::new(f), len));
                }
                let t = std::time::Instant::now();
                // The zlib header tells zlib emulation what to look for.
                let hints = if hdr == 2 {
                    aqr_flate::Hints::zlib_header([data[0], data[1]])
                } else {
                    aqr_flate::Hints::default()
                };
                let r = aqr_flate::deconstruct_hinted(&data[hdr..end], fr_opts, &hints).map(|(p, m, _)| (p, m));
                times[2] += t.elapsed().as_secs_f64();
                let (plain, meta) = r.ok()?;
                Some(Coded::Full(head, plain, meta, len))
            });
            coded.unwrap_or(Coded::Raw(true))
        }
        Plan::Flate(_, _, PNG, Some(g)) => {
            let (ct, depth) = image::png_format(&g).unwrap();
            let t = std::time::Instant::now();
            let wrapped = image::wrap(g.width, g.height, ct, depth, data);
            let f = aqr_png::formula::encode(&wrapped, 1).filter(|f| streams_len(f) < data.len());
            times[3] += t.elapsed().as_secs_f64();
            f.map_or(Coded::Raw(false), Coded::Png)
        }
        Plan::Jpeg(_, _, true) => {
            let t = std::time::Instant::now();
            let f = aqr_jpeg::formula::encode(data, 0);
            times[3] += t.elapsed().as_secs_f64();
            f.map_or(Coded::Raw(false), Coded::Jpeg)
        }
        _ => Coded::Raw(false),
    };
    Done { coded, times }
}

/// The sections as the items go in, in order.
struct Sections {
    table: Vec<u8>,
    skel: Vec<u8>,
    raw: Vec<u8>,
    meta: Vec<u8>,
    tokens: tok::Encoder,
    pools: Vec<Vec<u8>>,
    /// The end of the last item.
    at: usize,
    /// Bytes each item covers once coded.
    cov: Vec<usize>,
    /// What goes into the formula's input slots instead.
    fm: Parts,
}

/// The formula being built: the pools and metadata are its first streams (`F_POOL`, `F_META`,
/// filled at the end), then the images' formulas; and the nodes of the slots that hold the
/// items' rebuilt DEFLATE streams, PNG files and JPEG files.
pub(crate) struct Parts {
    pub f: Formula,
    pub deflate: Vec<Node>,
    pub png: Vec<Node>,
    pub jpeg: Vec<Node>,
}

pub(crate) const F_POOL: usize = 0;
pub(crate) const F_META: usize = F_POOL + CLASSES.len();

impl Parts {
    fn new() -> Self {
        let mut f = Formula::default();
        for c in CLASSES {
            f.push(format!("pdf/pool-{}", c.name()), vec![]);
        }
        f.push("pdf/meta", vec![]);
        Parts { f, deflate: vec![], png: vec![], jpeg: vec![] }
    }
}

impl Sections {
    fn new(skel_len: usize, items: usize) -> Self {
        Sections {
            fm: Parts::new(),
            table: vec![],
            skel: Vec::with_capacity(skel_len),
            raw: vec![],
            meta: vec![],
            tokens: tok::Encoder::default(),
            pools: vec![vec![]; CLASSES.len()],
            at: 0,
            cov: Vec::with_capacity(items),
        }
    }

    /// Adds item `i`, coded as `d`.
    fn add(&mut self, file: &[u8], i: usize, it: &Item, d: Done, st: &mut Stats) {
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
            Coded::Tok(head, f, len) => match self.tokens.add(head.class, &f, head.px) {
                Ok(()) => {
                    self.table.extend_from_slice(&head.tok_fields());
                    (TOK, len, 0)
                }
                Err(_) => {
                    st.failed += 1;
                    raw(self)
                }
            },
            Coded::Full(head, plain, m, len) => {
                let c = head.class as usize;
                self.table.extend_from_slice(&head.full_fields());
                let pool = Node::slice(F_POOL + c, self.pools[c].len() as u64, plain.len() as u64);
                let record = Node::slice(F_META, self.meta.len() as u64, m.len() as u64);
                let profile = aqr_flate::record_profile(&m).expect("a record");
                let deflate = aqr_flate::formula::apply(profile, pool, record, head.deflate as u64);
                self.fm.deflate.push(deflate.expect("a record of a known profile"));
                self.pools[c].extend_from_slice(&plain);
                self.meta.extend_from_slice(&m);
                (FULL, len, m.len())
            }
            Coded::Png(p) => {
                self.table.push(PNG);
                varint::put(&mut self.table, p.len());
                let n = streams_len(&p);
                self.fm.png.push(self.fm.f.embed(p));
                (PNG, it.len, n)
            }
            Coded::Jpeg(p) => {
                self.table.push(JPEG);
                varint::put(&mut self.table, p.len());
                let n = streams_len(&p);
                self.fm.jpeg.push(self.fm.f.embed(p));
                (JPEG, it.len, n)
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
