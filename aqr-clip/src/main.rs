//! Research tool for `.clip` files.
//!
//! - `aqr-clip stat FILE...`: the chunks, the block data, and the tiles by layout and class.
//! - `aqr-clip bench [-t THREADS] [-b TILES] [-c] FILE...`: the tiles of each layout and class
//!   coded by every model at hand, a batch of `-b` tiles (256) at a time, and what a file would
//!   come to with the best of them. `-c` decodes the dynamic model's streams and compares.
//! - `aqr-clip speed [-t THREADS] [-b TILES] [-m dynamic|static] [-u] FILE...` (`-u`: a tile equal to
//!   one before it in its file not coded, its reference counted apart): the model of whole pixels
//!   with runs (`dynamic`: adaptive bits, `static`: static tables) alone, as a
//!   module would run it: batches of `-b` tiles (256) of a layout in file order, each a stream
//!   of its own (the model anew), coded on `-t` threads; then decoded, the tiles deflated again
//!   ([`aqr_clip::zlib`]) and both compared with the file. Wall times, and thread times by step.
//! - `aqr-clip formula [-t THREADS] FILE...`: each file's formula (`clip-recons`), its streams,
//!   the archive at level mid, and the formula evaluated back and compared.
//!
//! Models, by the bytes they code a batch to:
//!
//! - `zlib`: the zlib streams as they are, and `zl/gen` those through the archive (level mid):
//!   what aqr does now;
//! - `px/gen`: the inflated tiles through the archive;
//! - `raw`: aqr-raw's raster coder, the tiles stacked into an image 256 pixels wide (the plane
//!   apart from the 4 interleaved samples);
//! - `psd`: aqr-psd's pixel model, a group of B, G, R and the alpha plane per tile and the fourth
//!   sample as a plane of its own;
//! - `dyn-l`: [`aqr_clip::dynamic`] with [`Escape::Left`], `dyn-m` with [`Escape::Med`], `dyn`
//!   with [`Escape::Med`], runs and tile kinds.
//!
//! The file's estimate: each class by its best model (zero tiles free, constant ones their
//! pixel), the SQLite database by aqr-sqlite, and everything else (block headers, tables,
//! external data of other kinds) through the archive.

use aqr_clip::dynamic::{self, Escape, Model};
use aqr_clip::tile::{self, AREA, Class, Layout, SIDE};
use aqr_clip::{clip, r#static};
use aqr_common::arith::{ArithDecoder, ArithEncoder};
use aqr_formula::Formula;
use aqr_formula::aqr::{self, Level};
use aqr_psd::model::{GroupCoder, Recorder};
use aqr_raw::image::raster;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage: aqr-clip stat FILE...\n       aqr-clip bench [-t THREADS] [-b TILES] [-c] FILE...\n       aqr-clip speed [-t THREADS] [-b TILES] [-m dynamic|static] [-u] FILE...\n       aqr-clip formula [-t THREADS] FILE..."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("stat") => args[1..].iter().for_each(|f| stat(f)),
        Some("bench") => bench(&args[1..]),
        Some("speed") => speed(&args[1..]),
        Some("formula") => formula(&args[1..]),
        _ => usage(),
    }
}

fn inflate(z: &[u8], len: usize) -> Option<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut out = Vec::with_capacity(len);
    let st = Decompress::new(true).decompress_vec(z, &mut out, FlushDecompress::Finish).ok()?;
    (st == Status::StreamEnd && out.len() == len).then_some(out)
}

/// Tiles by layout and class.
type Key = (Layout, Class);

/// A block with data, found in a file.
struct Found<'a> {
    layout: Layout,
    zlib: &'a [u8],
}

/// The parts of a file: its blocks with data (of a known layout), the database, and everything
/// else.
struct Parts<'a> {
    found: Vec<Found<'a>>,
    sqlite: &'a [u8],
    other: Vec<u8>,
    /// Blocks without data, and with data of an unknown layout (kept in `other`).
    empty: usize,
    unknown: usize,
}

fn parts(d: &[u8]) -> Option<Parts<'_>> {
    let at = |s: &[u8]| s.as_ptr() as usize - d.as_ptr() as usize;
    let (mut found, mut sqlite, mut cut, mut empty, mut unknown) = (Vec::new(), &d[..0], Vec::new(), 0, 0);
    for c in clip::chunks(d)? {
        match &c.kind {
            b"SQLi" => {
                sqlite = c.body;
                cut.push(at(c.body)..at(c.body) + c.body.len());
            }
            b"Exta" => {
                let Some(b) = clip::external(c.body).and_then(|(_, data)| clip::blocks(data)) else { continue };
                for bl in &b.blocks {
                    match (bl.zlib, Layout::of(bl.len as usize)) {
                        (None, _) => empty += 1,
                        (Some(_), None) => unknown += 1,
                        (Some(z), Some(layout)) => {
                            cut.push(at(z)..at(z) + z.len());
                            found.push(Found { layout, zlib: z });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    cut.sort_by_key(|r| r.start);
    let (mut other, mut p) = (Vec::new(), 0);
    for r in cut {
        other.extend_from_slice(&d[p..r.start]);
        p = r.end;
    }
    other.extend_from_slice(&d[p..]);
    Some(Parts { found, sqlite, other, empty, unknown })
}

fn stat(path: &str) {
    let d = std::fs::read(path).expect("read");
    let Some(chunks) = clip::chunks(&d) else {
        println!("{path}: not a .clip file");
        return;
    };
    let mut kinds: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut blocks, mut other) = (0, 0);
    for c in &chunks {
        let e = kinds.entry(String::from_utf8_lossy(&c.kind).into()).or_default();
        e.0 += 1;
        e.1 += c.body.len();
        if &c.kind == b"Exta" {
            match clip::external(c.body).map(|(_, data)| (clip::is_block_data(data), clip::blocks(data))) {
                Some((true, Some(_))) => blocks += 1,
                Some((true, None)) => println!("  block data at {} does not parse", c.start),
                _ => other += 1,
            }
        }
    }
    println!("{path}: {} bytes", d.len());
    for (k, (n, b)) in &kinds {
        println!("  {k}: {n} chunks, {b} bytes");
    }
    println!("  external data: {blocks} of blocks, {other} of other kinds");
    let p = parts(&d).expect("parts");
    let mut by: BTreeMap<Key, (usize, usize)> = BTreeMap::new();
    for f in &p.found {
        let t = inflate(f.zlib, f.layout.bytes()).expect("inflate");
        let e = by.entry((f.layout, tile::classify(f.layout, &t))).or_default();
        e.0 += 1;
        e.1 += f.zlib.len();
    }
    println!("  blocks: {} empty, {} of unknown layout", p.empty, p.unknown);
    for ((l, c), (n, z)) in by {
        println!("  {:5} {:5}: {n} tiles, {z} bytes of zlib", l.name(), c.name());
    }
}

/// Bytes of `data` through the archive.
fn generic(data: Vec<u8>) -> usize {
    if data.is_empty() {
        return 0;
    }
    aqr::encode(&Formula::raw("raw", data), None, Level::MID).expect("archive").len()
}

#[derive(Default, Clone, Copy)]
struct Sizes {
    tiles: usize,
    zlib: usize,
    zl_gen: usize,
    px_gen: usize,
    raw: usize,
    psd: usize,
    dyn_l: usize,
    dyn_m: usize,
    dynamic: usize,
    /// Seconds of the dynamic models' encoders.
    dyn_l_s: f64,
    dyn_m_s: f64,
    dynamic_s: f64,
    psd_s: f64,
}

impl std::ops::AddAssign for Sizes {
    fn add_assign(&mut self, o: Sizes) {
        self.tiles += o.tiles;
        self.zlib += o.zlib;
        self.zl_gen += o.zl_gen;
        self.px_gen += o.px_gen;
        self.raw += o.raw;
        self.psd += o.psd;
        self.dyn_l += o.dyn_l;
        self.dyn_m += o.dyn_m;
        self.dynamic += o.dynamic;
        self.dyn_l_s += o.dyn_l_s;
        self.dyn_m_s += o.dyn_m_s;
        self.dynamic_s += o.dynamic_s;
        self.psd_s += o.psd_s;
    }
}

impl Sizes {
    /// The best of the models, for tiles of class `c` (of `samples` bytes a pixel).
    fn best(&self, c: Class, samples: usize) -> usize {
        match c {
            Class::Zero => 0,
            Class::Const => self.tiles * samples,
            _ => self.raw.min(self.psd).min(self.dyn_l).min(self.dyn_m).min(self.dynamic).min(self.px_gen),
        }
    }
}

fn raster_size(d: &[u8], n: usize, channels: usize) -> usize {
    let f = raster::Format { width: SIDE, height: SIDE * n, channels, stride: SIDE * channels };
    let c = raster::encode(d, f).expect("raster");
    c.coded.len() + c.pad.len()
}

fn psd_size(l: Layout, tiles: &[Vec<u8>]) -> usize {
    let mut out = Vec::new();
    let mut rec = Recorder::new(&mut out);
    for t in tiles {
        match l {
            Layout::Color => {
                let mut g = GroupCoder::new(SIDE, 4, true, vec![false; 4], vec![None; 4]);
                let mut x = GroupCoder::new(SIDE, 1, false, vec![false], vec![None]);
                let (mut rows, mut xr) = (vec![vec![0u8; SIDE]; 4], vec![vec![0u8; SIDE]]);
                for y in 0..SIDE {
                    for i in 0..SIDE {
                        let o = AREA + (y * SIDE + i) * 4;
                        (rows[0][i], rows[1][i], rows[2][i], xr[0][i]) = (t[o], t[o + 1], t[o + 2], t[o + 3]);
                    }
                    rows[3].copy_from_slice(&t[y * SIDE..(y + 1) * SIDE]);
                    g.code_row(&mut rec, &mut rows, None);
                    x.code_row(&mut rec, &mut xr, None);
                }
            }
            Layout::One | Layout::Two => {
                for k in 0..l.samples() {
                    let mut g = GroupCoder::new(SIDE, 1, false, vec![false], vec![None]);
                    let mut rows = vec![vec![0u8; SIDE]];
                    for y in 0..SIDE {
                        let o = k * AREA + y * SIDE;
                        rows[0].copy_from_slice(&t[o..o + SIDE]);
                        g.code_row(&mut rec, &mut rows, None);
                    }
                }
            }
        }
    }
    rec.finish();
    out.len()
}

fn dynamic_size(l: Layout, tiles: &[Vec<u8>], escape: Escape, runs: bool, check: bool) -> usize {
    let mut m = Model::new(escape, runs);
    let mut e = dynamic::Enc(ArithEncoder::new(Vec::new()));
    let mut px = Vec::with_capacity(AREA);
    for t in tiles {
        tile::pixels(l, t, &mut px);
        m.tile(&mut e, l, &mut px);
    }
    let coded = e.0.finish();
    if check {
        let mut m = Model::new(escape, runs);
        let mut dec = dynamic::Dec(ArithDecoder::new(&coded));
        let (mut px, mut t2) = (vec![0u64; AREA], Vec::new());
        for t in tiles {
            px.iter_mut().for_each(|p| *p = 0);
            m.tile(&mut dec, l, &mut px);
            tile::unpixels(l, &px, &mut t2);
            assert!(t2 == *t, "dynamic model: a tile decoded otherwise");
        }
    }
    coded.len()
}

/// Codes a batch of tiles of layout `l` with every model.
fn batch(l: Layout, zs: &[&[u8]], check: bool) -> Sizes {
    let tiles: Vec<Vec<u8>> = zs.iter().map(|z| inflate(z, l.bytes()).expect("inflate")).collect();
    let n = tiles.len();
    let mut s = Sizes { tiles: n, zlib: zs.iter().map(|z| z.len()).sum(), ..Sizes::default() };
    s.zl_gen = generic(zs.concat());
    s.px_gen = generic(tiles.concat());
    // planes (and for colour the interleaved samples) stacked
    let plane =
        |k: usize| -> Vec<u8> { tiles.iter().flat_map(|t| t[k * AREA..(k + 1) * AREA].iter().copied()).collect() };
    s.raw = match l {
        Layout::Color => {
            let c: Vec<u8> = tiles.iter().flat_map(|t| t[AREA..].iter().copied()).collect();
            raster_size(&plane(0), n, 1) + raster_size(&c, n, 4)
        }
        Layout::One => raster_size(&plane(0), n, 1),
        Layout::Two => raster_size(&plane(0), n, 1) + raster_size(&plane(1), n, 1),
    };
    let t = Instant::now();
    s.psd = psd_size(l, &tiles);
    s.psd_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    s.dyn_l = dynamic_size(l, &tiles, Escape::Left, false, check);
    s.dyn_l_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    s.dyn_m = dynamic_size(l, &tiles, Escape::Med, false, check);
    s.dyn_m_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    s.dynamic = dynamic_size(l, &tiles, Escape::Med, true, check);
    s.dynamic_s = t.elapsed().as_secs_f64();
    s
}

/// Runs `f` over `0..n` on `threads` threads, the results in order.
fn par<T: Send>(n: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<T>>> = Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.min(n.max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = f(i);
                    out.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

fn bench(args: &[String]) {
    let (mut threads, mut per, mut check, mut files) = (1, 256, false, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => threads = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-b" => per = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-c" => check = true,
            _ => files.push(a.clone()),
        }
    }
    let mut all: BTreeMap<Key, Sizes> = BTreeMap::new();
    let (mut t_in, mut t_est, mut t_sql, mut t_other) = (0, 0, 0, 0);
    println!(
        "{:28} {:5} {:5} {:>7} {:>11} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "file", "lay", "class", "tiles", "zlib", "zl/gen", "px/gen", "raw", "psd", "dyn-l", "dyn-m", "dyn"
    );
    for path in &files {
        let d = std::fs::read(path).expect("read");
        let Some(p) = parts(&d) else {
            println!("{path}: not a .clip file");
            continue;
        };
        // classes first, then batches of each
        let classes = par(p.found.len(), threads, |i| {
            let f = &p.found[i];
            tile::classify(f.layout, &inflate(f.zlib, f.layout.bytes()).expect("inflate"))
        });
        let mut groups: BTreeMap<Key, Vec<&[u8]>> = BTreeMap::new();
        for (f, c) in p.found.iter().zip(classes) {
            groups.entry((f.layout, c)).or_default().push(f.zlib);
        }
        let jobs: Vec<(Key, &[&[u8]])> = groups.iter().flat_map(|(k, v)| v.chunks(per).map(move |c| (*k, c))).collect();
        let sizes = par(jobs.len(), threads, |i| batch(jobs[i].0.0, jobs[i].1, check));
        let mut by: BTreeMap<Key, Sizes> = BTreeMap::new();
        for (j, s) in jobs.iter().zip(sizes) {
            *by.entry(j.0).or_default() += s;
        }
        let name: String = std::path::Path::new(path).file_name().unwrap().to_string_lossy().chars().take(28).collect();
        let mut est = 0;
        for ((l, c), s) in &by {
            let z = s.zlib as f64;
            println!(
                "{name:28} {:5} {:5} {:7} {:11} {:7.3} {:7.3} {:7.3} {:7.3} {:7.3} {:7.3} {:7.3}",
                l.name(),
                c.name(),
                s.tiles,
                s.zlib,
                s.zl_gen as f64 / z,
                s.px_gen as f64 / z,
                s.raw as f64 / z,
                s.psd as f64 / z,
                s.dyn_l as f64 / z,
                s.dyn_m as f64 / z,
                s.dynamic as f64 / z
            );
            est += s.best(*c, l.samples());
            *all.entry((*l, *c)).or_default() += *s;
        }
        let sql = aqr_sqlite::formula::encode(p.sqlite, threads)
            .map(|f| aqr::encode(&f, None, Level::MID).expect("archive").len())
            .unwrap_or_else(|| generic(p.sqlite.to_vec()));
        let other = generic(p.other.clone());
        println!(
            "{name:28} file {} -> tiles {est} + sqlite {sql} (of {}) + other {other} (of {}) = {} ({:.3})",
            d.len(),
            p.sqlite.len(),
            p.other.len(),
            est + sql + other,
            (est + sql + other) as f64 / d.len() as f64
        );
        t_in += d.len();
        t_est += est;
        t_sql += sql;
        t_other += other;
    }
    println!("all:");
    let mut tot = Sizes::default();
    let mut best = 0;
    for ((l, c), s) in &all {
        let z = s.zlib as f64;
        println!(
            "  {:5} {:5} {:7} {:11} {:7.3} {:7.3} {:7.3} {:7.3} {:7.3} {:7.3} {:7.3}  psd {:.0}, dyn-l {:.0}, dyn-m {:.0}, dyn {:.0} MB/s (inflated, a thread)",
            l.name(),
            c.name(),
            s.tiles,
            s.zlib,
            s.zl_gen as f64 / z,
            s.px_gen as f64 / z,
            s.raw as f64 / z,
            s.psd as f64 / z,
            s.dyn_l as f64 / z,
            s.dyn_m as f64 / z,
            s.dynamic as f64 / z,
            (s.tiles * l.bytes()) as f64 / s.psd_s / 1e6,
            (s.tiles * l.bytes()) as f64 / s.dyn_l_s / 1e6,
            (s.tiles * l.bytes()) as f64 / s.dyn_m_s / 1e6,
            (s.tiles * l.bytes()) as f64 / s.dynamic_s / 1e6
        );
        tot += *s;
        best += s.best(*c, l.samples());
    }
    let z = tot.zlib as f64;
    println!(
        "  tiles: zlib {} -> zl/gen {} ({:.3}), px/gen {:.3}, raw {:.3}, psd {:.3}, dyn-l {:.3}, dyn-m {:.3}, dyn {:.3}, best by class {} ({:.3})",
        tot.zlib,
        tot.zl_gen,
        tot.zl_gen as f64 / z,
        tot.px_gen as f64 / z,
        tot.raw as f64 / z,
        tot.psd as f64 / z,
        tot.dyn_l as f64 / z,
        tot.dyn_m as f64 / z,
        tot.dynamic as f64 / z,
        best,
        best as f64 / z
    );
    println!(
        "  files {t_in} -> tiles {t_est} + sqlite {t_sql} + other {t_other} = {} ({:.3})",
        t_est + t_sql + t_other,
        (t_est + t_sql + t_other) as f64 / t_in as f64
    );
}

/// Thread seconds of a job's steps.
#[derive(Default, Clone, Copy)]
struct Steps {
    inflate: f64,
    model: f64,
    deflate: f64,
}

/// Seconds since `t`, which starts anew.
fn lap(t: &mut Instant) -> f64 {
    let e = t.elapsed().as_secs_f64();
    *t = Instant::now();
    e
}

fn speed(args: &[String]) {
    let (mut threads, mut per, mut stat, mut unique, mut files) = (1, 256, false, false, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => threads = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-b" => per = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-u" => unique = true,
            "-m" => match it.next().map(String::as_str) {
                Some("dynamic") => stat = false,
                Some("static") => stat = true,
                _ => usage(),
            },
            _ => files.push(a.clone()),
        }
    }
    let data: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).expect("read")).collect();
    let all: Vec<Parts> = data.iter().map(|d| parts(d).expect("not a .clip file")).collect();
    // batches of a layout, in file order
    let mut jobs: Vec<(Layout, Vec<&[u8]>)> = Vec::new();
    // with -u, a tile equal to one before it in its file is a reference to it: the varint of the
    // number of distinct tiles back, counted apart
    let (mut dups, mut dup_zlib, mut refs) = (0usize, 0usize, 0usize);
    for p in &all {
        let mut by: BTreeMap<Layout, Vec<&[u8]>> = BTreeMap::new();
        let mut seen: std::collections::HashMap<&[u8], usize> = std::collections::HashMap::new();
        for f in &p.found {
            if unique {
                let n = seen.len();
                if let Some(&k) = seen.get(f.zlib) {
                    dups += 1;
                    dup_zlib += f.zlib.len();
                    refs += 1 + (usize::BITS - (n - k).leading_zeros()) as usize / 7;
                    continue;
                }
                seen.insert(f.zlib, n);
            }
            by.entry(f.layout).or_default().push(f.zlib);
        }
        for (l, zs) in by {
            jobs.extend(zs.chunks(per).map(|c| (l, c.to_vec())));
        }
    }
    let input: usize = data.iter().map(|d| d.len()).sum();
    let zlib: usize = jobs.iter().flat_map(|j| j.1.iter()).map(|z| z.len()).sum();
    let inflated: usize = jobs.iter().map(|j| j.1.len() * j.0.bytes()).sum();

    let heads = AtomicUsize::new(0);
    let t = Instant::now();
    let coded = par(jobs.len(), threads, |i| {
        let (l, zs) = (jobs[i].0, &jobs[i].1);
        let mut c = Instant::now();
        let tiles: Vec<Vec<u8>> = zs.iter().map(|z| inflate(z, l.bytes()).expect("inflate")).collect();
        let inflate = lap(&mut c);
        let mut px = Vec::with_capacity(AREA);
        let out = if stat {
            let (mut m, mut r) = (r#static::Model::default(), r#static::Recorder::default());
            for t in &tiles {
                tile::pixels(l, t, &mut px);
                m.tile(&mut r, l, &mut px);
            }
            let (out, head) = r.finish();
            heads.fetch_add(head, Ordering::Relaxed);
            out
        } else {
            let mut m = Model::new(Escape::Med, true);
            let mut e = dynamic::Enc(ArithEncoder::new(Vec::new()));
            for t in &tiles {
                tile::pixels(l, t, &mut px);
                m.tile(&mut e, l, &mut px);
            }
            e.0.finish()
        };
        (out, Steps { inflate, model: lap(&mut c), deflate: 0.0 })
    });
    let enc_wall = t.elapsed().as_secs_f64();
    let size: usize = coded.iter().map(|c| c.0.len()).sum();

    let t = Instant::now();
    let dec = par(jobs.len(), threads, |i| {
        let (l, zs) = (jobs[i].0, &jobs[i].1);
        let mut c = Instant::now();
        let (mut px, mut tiles) = (vec![0u64; AREA], Vec::with_capacity(zs.len()));
        let mut push = |px: &[u64]| {
            let mut t = Vec::new();
            tile::unpixels(l, px, &mut t);
            tiles.push(t);
        };
        if stat {
            let (mut m, mut p) = (r#static::Model::default(), r#static::Player::new(&coded[i].0));
            for _ in zs {
                m.tile(&mut p, l, &mut px);
                push(&px);
            }
            assert!(p.finish(), "a stream not consumed exactly");
        } else {
            let mut m = Model::new(Escape::Med, true);
            let mut d = dynamic::Dec(ArithDecoder::new(&coded[i].0));
            for _ in zs {
                m.tile(&mut d, l, &mut px);
                push(&px);
            }
        }
        let model = lap(&mut c);
        let again: Vec<Vec<u8>> = tiles.iter().map(|t| aqr_clip::zlib::deflate(t).expect("deflate")).collect();
        let deflate = lap(&mut c);
        // (the check is not timed)
        for ((t, z), a) in tiles.iter().zip(zs).zip(&again) {
            assert!(*t == inflate(z, l.bytes()).unwrap(), "a tile decoded otherwise");
            assert!(a == z, "a zlib stream made otherwise");
        }
        Steps { inflate: 0.0, model, deflate }
    });
    let dec_wall = t.elapsed().as_secs_f64();

    let total = |v: &[Steps], f: fn(&Steps) -> f64| v.iter().map(f).sum::<f64>();
    let es: Vec<Steps> = coded.iter().map(|c| c.1).collect();
    let mb = |s: f64| input as f64 / s / 1e6;
    println!(
        "{} files, {input} bytes ({zlib} of zlib, {inflated} inflated), {} batches of up to {per} tiles, {threads} threads",
        files.len(),
        jobs.len()
    );
    println!(
        "tiles: {zlib} -> {size} ({:.3} of zlib), {} bytes of them tables",
        size as f64 / zlib as f64,
        heads.into_inner()
    );
    if unique {
        println!(
            "repeated tiles: {dups}, {dup_zlib} bytes of zlib, as references about {refs} bytes: all tiles {} -> {} ({:.3})",
            zlib + dup_zlib,
            size + refs,
            (size + refs) as f64 / (zlib + dup_zlib) as f64
        );
    }
    println!(
        "encode: {enc_wall:.2} s wall ({:.1} MB/s of input); thread times: inflate {:.1} s, model {:.1} s ({:.1} MB/s of input a thread)",
        mb(enc_wall),
        total(&es, |s| s.inflate),
        total(&es, |s| s.model),
        mb(total(&es, |s| s.model))
    );
    println!(
        "decode: {dec_wall:.2} s wall, the check included; thread times: model {:.1} s ({:.1} MB/s of input a thread), deflate {:.1} s",
        total(&dec, |s| s.model),
        mb(total(&dec, |s| s.model)),
        total(&dec, |s| s.deflate)
    );
}

fn formula(args: &[String]) {
    let (mut threads, mut files) = (1, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => threads = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            _ => files.push(a.clone()),
        }
    }
    let modules = |a: &aqr_formula::Apply, inputs: &[&[u8]]| {
        aqr_clip::formula::apply(a, inputs, threads).map_err(aqr_formula::Error::new)
    };
    let (mut tin, mut tout) = (0usize, 0usize);
    for path in &files {
        let d = std::fs::read(path).expect("read");
        let t = Instant::now();
        let Some(f) = aqr_clip::formula::encode(&d, threads) else {
            println!("{path}: no formula");
            continue;
        };
        let enc = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let archive = aqr::encode(&f, None, Level::MID).expect("archive").len();
        let arc = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let back = f.eval(&modules).expect("eval");
        let dec = t.elapsed().as_secs_f64();
        assert!(back == d, "{path}: evaluated otherwise");
        let streams: Vec<String> = f.streams.iter().map(|s| format!("{} {}", s.class, s.data.len())).collect();
        println!(
            "{path}: {} -> {archive} ({:.3}); encode {enc:.2} s, archive {arc:.2} s, decode {dec:.2} s; {}",
            d.len(),
            archive as f64 / d.len() as f64,
            streams.join(", ")
        );
        tin += d.len();
        tout += archive;
    }
    println!("all: {tin} -> {tout} ({:.3})", tout as f64 / tin as f64);
}
