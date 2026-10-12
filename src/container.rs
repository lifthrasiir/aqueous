//! The file format, and compression / decompression of whole files.
//!
//! The pixel data of a PSD/PSB comes in *spans*: a layer channel's data (past its
//! compression field) or the composite image's (all planes). 8-bit spans that are RLE
//! (PackBits) or raw are taken out of the file and their planes coded by the pixel model;
//! RLE is rebuilt by the encoder identified for the span ([`packbits`]), with the rows it
//! does not reproduce stored verbatim. A span with the same bytes as one coded before is a
//! copy. Files embedded in smart objects are spans too, coded by the recompressor of their
//! format (this one for PSD/PSB, png-recons, jpeg-recons) when that makes them smaller. What
//! remains of the file, the *skeleton*, is compressed with zstd.
//!
//! Both directions stream: pixel data is coded row by row straight from the input, and
//! decoded row by row straight into the output (allocated at its final size), so that the
//! memory used besides input and output does not grow with the file.
//!
//! ```text
//! "PSDR" version(u8) mode(u8)
//! mode 0 (stored): zstd(file)
//! mode 1 (coded):
//!   u64 LE xxh3 of the original (checked after decompression)
//!   varint length of the original
//!   zstd(skeleton)                  -- its length is in the layout
//!   embedded files, coded           -- likewise
//!   model chunks                    -- see model::Recorder
//!   layout                          -- spans, encoders, exceptions, groups
//!   u64 LE length of the layout
//! ```

use std::io::{Read, Write};

use crate::composite::{self, RowRenderer};
use crate::model::{self, GroupCoder};
use crate::packbits::{self, Encoder};
use crate::psd::{self, Compression, Error, Psd, Result};
use crate::rows::{self, PlaneRows};
use recons_common::varint;

const MAGIC: &[u8; 4] = b"PSDR";
const VERSION: u8 = 2;

#[track_caller]
fn corrupt() -> Error {
    if std::env::var_os("PSDR_DEBUG").is_some() {
        eprintln!("corrupt at {}", std::panic::Location::caller());
    }
    Error("corrupt input".into())
}

pub fn put_varint(out: &mut Vec<u8>, v: u64) {
    varint::put(out, v)
}

pub fn get_varint(src: &[u8], at: &mut usize) -> Result<u64> {
    varint::get(src, at).map_err(|_| corrupt())
}

fn get_usize(src: &[u8], at: &mut usize) -> Result<usize> {
    usize::try_from(get_varint(src, at)?).map_err(|_| corrupt())
}

fn get_bytes<'a>(src: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = src.get(*at..at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
    *at += n;
    Ok(s)
}

fn get_u8(src: &[u8], at: &mut usize) -> Result<u8> {
    Ok(get_bytes(src, at, 1)?[0])
}

/// `usize::MAX` as 0, anything else shifted by one.
fn put_opt(out: &mut Vec<u8>, v: usize) {
    put_varint(out, if v == usize::MAX { 0 } else { v as u64 + 1 });
}

fn get_opt(src: &[u8], at: &mut usize) -> Result<usize> {
    Ok(match get_varint(src, at)? {
        0 => usize::MAX,
        v => usize::try_from(v - 1).map_err(|_| corrupt())?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Psd,
    Png,
    Jpeg,
}

/// How deep embedded PSDs are recompressed.
const MAX_DEPTH: usize = 3;

/// Options of [`compress_with`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Threads (1: all on the calling thread, the serial code). The output is the same
    /// bytes whatever the number.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { threads: 1 }
    }
}

/// Thread budget of one call, and what is shared among its threads.
struct Par {
    threads: usize,
    /// The pool jpeg-recons runs in with more than one thread (it uses rayon, whose global
    /// pool would ignore the budget); with one thread it keeps the global pool, as before.
    jpeg: std::sync::OnceLock<Option<rayon::ThreadPool>>,
}

impl Par {
    fn new(threads: usize) -> Par {
        Par { threads: threads.max(1), jpeg: std::sync::OnceLock::new() }
    }

    /// Runs `f` (a jpeg-recons call) in the pool of this budget.
    fn jpeg<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        if self.threads > 1
            && let Some(pool) =
                self.jpeg.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(self.threads).build().ok())
        {
            return pool.install(f);
        }
        f()
    }
}

/// `f` on every item, `threads` at a time (the caller's thread among them); results in the
/// order of the items. One thread, or one item: in order, on the calling thread. A panic
/// of `f` propagates, as it would serially.
fn par_map<T: Send, R: Send>(threads: usize, items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    if threads <= 1 || n <= 1 {
        return items.into_iter().map(f).collect();
    }
    let queue = std::sync::Mutex::new(items.into_iter().enumerate());
    let results = std::sync::Mutex::new((0..n).map(|_| None).collect::<Vec<Option<R>>>());
    let work = || {
        loop {
            let Some((i, t)) = queue.lock().unwrap().next() else { break };
            let r = f(t);
            results.lock().unwrap()[i] = Some(r);
        }
    };
    std::thread::scope(|sc| {
        for _ in 1..threads.min(n) {
            sc.spawn(work);
        }
        work();
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("every item is done")).collect()
}

fn nested_compress(codec: Codec, data: &[u8], depth: usize, par: &Par) -> Option<Vec<u8>> {
    let t0 = std::time::Instant::now();
    let r = nested_compress_(codec, data, depth, par);
    if std::env::var_os("PSDR_STATS").is_some() {
        eprintln!(
            "nested {codec:?} {} -> {:?} in {:.3}s",
            data.len(),
            r.as_ref().map(|v| v.len()),
            t0.elapsed().as_secs_f64()
        );
    }
    r
}

fn nested_compress_(codec: Codec, data: &[u8], depth: usize, par: &Par) -> Option<Vec<u8>> {
    let packed = match codec {
        Codec::Psd => try_compress(data, depth + 1, par)?,
        Codec::Png => png_recons::container::compress_threads(data, par.threads),
        Codec::Jpeg => par.jpeg(|| jpeg_recons::container::compress(data)),
    };
    (packed.len() < data.len()).then_some(packed)
}

fn nested_decompress(codec: Codec, packed: &[u8], par: &Par) -> Result<Vec<u8>> {
    match codec {
        Codec::Psd => decompress_threads(packed, par.threads),
        Codec::Png => png_recons::container::decompress_threads(packed, par.threads).map_err(|_| corrupt()),
        Codec::Jpeg => par.jpeg(|| jpeg_recons::container::decompress(packed)).map_err(|_| corrupt()),
    }
}

#[derive(Debug, Clone)]
enum Coding<'a> {
    Raw,
    Rle {
        count_bytes: usize,
        encoder: Encoder,
        /// Bytes of each plane's rows (without the counts).
        plane_lens: Vec<usize>,
        /// Rows (plane * h + row) not reproduced by `encoder`, and their bytes.
        exceptions: Vec<(usize, &'a [u8])>,
    },
    /// An embedded file, coded by `codec` into `len` bytes of the stream.
    Nested {
        codec: Codec,
        len: usize,
    },
    /// The same bytes as span `of`, which is coded before it.
    Copy {
        of: usize,
    },
}

/// A span of pixel data taken out of the file: `planes` planes of `h` rows of `w` bytes.
/// `gap` is the number of skeleton bytes before it (after the previous span).
#[derive(Debug, Clone)]
struct Span<'a> {
    start: usize,
    end: usize,
    gap: usize,
    planes: usize,
    w: usize,
    h: usize,
    coding: Coding<'a>,
}

/// A plane of a span: (span index, plane index).
type PlaneRef = (usize, usize);

#[derive(Debug, Clone)]
struct GroupDef {
    planes: Vec<PlaneRef>,
    has_alpha: bool,
    /// Coded with the composite rendered from the layers as hint (all planes in one span,
    /// which is the composite's).
    hinted: bool,
    /// Per plane, the plane (index in the group) it equals, coded before it in a row.
    same_as: Vec<Option<usize>>,
}

/// Checks the row counts of RLE data against its length, and a sample of rows (every
/// `every`th) decoding to `w` bytes; other rows are checked as they are decoded. Returns the
/// bytes of each plane's rows and the token breaks of the sample, for encoder identification.
fn check_rle(
    src: &[u8],
    planes: usize,
    w: usize,
    h: usize,
    cb: usize,
    every: usize,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let rows = planes * h;
    let counts = src.get(..rows * cb)?;
    let mut pos = rows * cb;
    let mut lens = vec![0; planes];
    let mut splits = Vec::new();
    for r in 0..rows {
        let n = rows::count(counts, cb, r);
        if r % every == 0 {
            let row = src.get(pos..pos + n)?;
            if packbits::decoded_len(row)? != w {
                return None;
            }
            packbits::forced_splits(row, &mut splits);
        }
        lens[r / h] += n;
        pos += n;
    }
    (pos == src.len()).then_some((lens, splits))
}

/// The token breaks of all rows of RLE data (for a second look at the encoder).
fn all_splits(src: &[u8], rows: usize, cb: usize) -> Vec<usize> {
    let mut pos = rows * cb;
    let mut splits = Vec::new();
    for r in 0..rows {
        let n = rows::count(src, cb, r);
        packbits::forced_splits(&src[pos..pos + n], &mut splits);
        pos += n;
    }
    splits
}

/// Orders candidate encoders by how many of a sample of rows they reproduce (stable, so the
/// inferred tile cut stays first on ties).
fn rank_candidates(mut cands: Vec<Encoder>, src: &[u8], planes: usize, w: usize, h: usize, cb: usize) -> Vec<Encoder> {
    const SAMPLE: usize = 32;
    let rows = planes * h;
    let step = rows.div_ceil(SAMPLE).max(1);
    let mut starts = vec![0usize; rows + 1];
    starts[0] = rows * cb;
    for r in 0..rows {
        starts[r + 1] = starts[r] + rows::count(src, cb, r);
    }
    let mut row = vec![0u8; w];
    let mut hits = vec![0usize; cands.len()];
    for r in (0..rows).step_by(step) {
        let enc = &src[starts[r]..starts[r + 1]];
        if !rows::unpackbits_into(enc, &mut row) {
            continue;
        }
        for (k, &c) in cands.iter().enumerate() {
            hits[k] += packbits::reproduces(c, &row, enc) as usize;
        }
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by_key(|&k| std::cmp::Reverse(hits[k]));
    let ranked = order.iter().map(|&k| cands[k]).collect();
    cands.clear();
    ranked
}

/// A planned span with what is known before coding.
struct Planned<'a> {
    span: Span<'a>,
    /// For RLE: candidate encoders, most likely first.
    candidates: Vec<Encoder>,
    /// An earlier span with the same bytes and size.
    same: Option<usize>,
}

/// A span of `planes` planes in `d[start..end]`, if its pixel data can be coded.
#[allow(clippy::too_many_arguments)]
fn plan_span<'a>(
    d: &[u8],
    start: usize,
    end: usize,
    planes: usize,
    w: usize,
    h: usize,
    comp: Compression,
    psb: bool,
) -> Option<Planned<'a>> {
    let src = &d[start..end];
    let (coding, candidates) = match comp {
        Compression::Raw => {
            if src.len() != planes.checked_mul(w)?.checked_mul(h)? {
                return None;
            }
            (Coding::Raw, Vec::new())
        }
        Compression::Rle => {
            let cb = if psb { 4 } else { 2 };
            let t0 = std::time::Instant::now();
            let (plane_lens, splits) = check_rle(src, planes, w, h, cb, 8)?;
            let t1 = std::time::Instant::now();
            let candidates = packbits::candidates_after(packbits::infer_tiles_from(splits));
            let t2 = std::time::Instant::now();
            let candidates = rank_candidates(candidates, src, planes, w, h, cb);
            PLAN_TIMES.add([(t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), t2.elapsed().as_secs_f64(), 0.0]);
            (Coding::Rle { count_bytes: cb, encoder: candidates[0], plane_lens, exceptions: Vec::new() }, candidates)
        }
        _ => return None,
    };
    Some(Planned { span: Span { start, end, gap: 0, planes, w, h, coding }, candidates, same: None })
}

/// Whether rendering the layers predicts the composite (span `sp`, first `planes` planes) well
/// enough to be worth its time: on a sample of rows, the share of pixels predicted exactly.
fn hint_pays(psd: &Psd, d: &[u8], sp: &Span, planes: usize) -> bool {
    const EVERY: usize = 16;
    const SHARE: f64 = 0.5;
    let (w, h) = (sp.w, sp.h);
    let comp = if matches!(sp.coding, Coding::Raw) { Compression::Raw } else { Compression::Rle };
    let src = &d[sp.start..sp.end];
    let Ok(mut r) = RowRenderer::new(psd, d, &mut composite::Report::default()) else { return false };
    let Ok(mut stored) =
        (0..planes).map(|p| PlaneRows::new(psd.psb, comp, src, w, h, p, sp.planes)).collect::<Result<Vec<_>>>()
    else {
        return false;
    };
    let mut rendered = vec![vec![0u8; w]; planes];
    let mut row = vec![0u8; w];
    let mut same = vec![true; w];
    let (mut hits, mut total) = (0usize, 0usize);
    for y in 0..h {
        let sample = y % EVERY == EVERY / 2;
        if sample {
            let mut refs: Vec<&mut [u8]> = rendered.iter_mut().map(|v| v.as_mut_slice()).collect();
            if r.next_row(&mut refs).is_err() {
                return false;
            }
            same.fill(true);
        } else {
            r.skip_row();
        }
        for (k, p) in stored.iter_mut().enumerate() {
            if !sample {
                if p.skip().is_err() {
                    return false;
                }
                continue;
            }
            if p.next(&mut row).is_err() {
                return false;
            }
            for x in 0..w {
                same[x] &= row[x] == rendered[k][x];
            }
        }
        if sample {
            hits += same.iter().filter(|&&s| s).count();
            total += w;
        }
    }
    total > 0 && hits as f64 >= SHARE * total as f64
}

/// Spans, groups and coded embedded files of a parsed file.
fn plan<'a>(psd: &Psd, d: &'a [u8], depth: usize, par: &Par) -> (Vec<Planned<'a>>, Vec<GroupDef>, Vec<Vec<u8>>) {
    let mut spans: Vec<Planned> = Vec::new();
    let mut groups = Vec::new();
    let mut nested = Vec::new();
    let pixels = psd.depth == 8;
    let rgb = psd.mode == 3;
    // Spans by a hash of their bytes and size; equal ones are confirmed byte for byte.
    let mut seen: std::collections::HashMap<(u64, usize, usize), Vec<usize>> = Default::default();
    let mut add = |spans: &mut Vec<Planned<'a>>, mut p: Planned<'a>| -> usize {
        let i = spans.len();
        let bytes = &d[p.span.start..p.span.end];
        let key = (xxhash_rust::xxh3::xxh3_64(bytes), p.span.w, p.span.h);
        let same = seen.entry(key).or_default();
        match same.iter().copied().find(|&j| &d[spans[j].span.start..spans[j].span.end] == bytes) {
            Some(j) => p.same = Some(j),
            None => same.push(i),
        }
        spans.push(p);
        i
    };
    // The channels to plan, in order: planning a span depends on nothing else, so it goes
    // ahead on the threads; spans are added (and found equal to earlier ones) in order.
    let chan = |c: &psd::Channel| {
        let (w, h) = (c.width as usize, c.height as usize);
        let comp = c.compression(d)?;
        (w != 0 && h != 0).then_some((c.span.start + 2, c.span.end, w, h, comp))
    };
    let jobs: Vec<_> =
        psd.layer_info.layers.iter().filter(|_| pixels).flat_map(|l| &l.channels).filter_map(chan).collect();
    let mut planned =
        par_map(par.threads, jobs, |(start, end, w, h, comp)| plan_span(d, start, end, 1, w, h, comp, psd.psb))
            .into_iter();
    for l in psd.layer_info.layers.iter().filter(|_| pixels) {
        let mut refs: Vec<(i16, PlaneRef)> = Vec::new();
        for c in &l.channels {
            if chan(c).is_none() {
                continue;
            }
            if let Some(p) = planned.next().flatten() {
                refs.push((c.id, (add(&mut spans, p), 0)));
            }
        }
        let pick = |id: i16| refs.iter().find(|r| r.0 == id).map(|r| r.1);
        let mut grouped = Vec::new();
        if rgb && let (Some(r), Some(g), Some(b)) = (pick(0), pick(1), pick(2)) {
            let dims = |p: PlaneRef| (spans[p.0].span.w, spans[p.0].span.h);
            if dims(g) == dims(r) && dims(b) == dims(r) {
                let mut planes = vec![r, g, b];
                let alpha = pick(-1).filter(|&a| dims(a) == dims(r));
                if let Some(a) = alpha {
                    planes.push(a);
                }
                grouped.extend(&planes);
                let n = planes.len();
                groups.push(GroupDef { planes, has_alpha: alpha.is_some(), hinted: false, same_as: vec![None; n] });
            }
        }
        for (_, p) in refs {
            if !grouped.contains(&p) {
                groups.push(GroupDef { planes: vec![p], has_alpha: false, hinted: false, same_as: vec![None] });
            }
        }
    }
    // Embedded files, which come after the layers' pixel data; and the composite. Embedded
    // files are coded, and the composite planned and its hint sampled, on the threads.
    let mut linked = Vec::new();
    if depth < MAX_DEPTH {
        let end = spans.last().map_or(0, |s| s.span.end);
        for f in psd::linked(psd, d) {
            if end > f.data.start {
                continue;
            }
            let codec = match &f.file_type {
                b"8BPB" | b"8BPS" => Codec::Psd,
                b"png " | b"PNGf" => Codec::Png,
                b"JPEG" => Codec::Jpeg,
                _ => continue,
            };
            linked.push((codec, f.data.clone()));
        }
    }
    let (w, h, n) = (psd.width as usize, psd.height as usize, psd.channels as usize);
    let comp = psd.composite_compression(d);
    let rgb_composite = rgb && n >= 3;
    let composite_job = || -> Option<(Planned<'a>, bool)> {
        if !(pixels && w > 0 && h > 0 && n > 0) {
            return None;
        }
        let mut p = plan_span(d, psd.composite.start + 2, psd.composite.end, n, w, h, comp, psd.psb)?;
        p.same = None;
        let mut hinted = false;
        if rgb_composite {
            let has_alpha = n >= 4 && psd.layer_info.merged_alpha;
            let k = 3 + has_alpha as usize;
            let t0 = std::time::Instant::now();
            hinted = !psd.layer_info.layers.is_empty() && hint_pays(psd, d, &p.span, k);
            PLAN_TIMES.add([0.0, 0.0, 0.0, t0.elapsed().as_secs_f64()]);
        }
        Some((p, hinted))
    };
    enum Task {
        Linked(Codec, std::ops::Range<usize>),
        Composite,
    }
    enum Done<'a> {
        Linked(Option<Vec<u8>>),
        Composite(Option<(Planned<'a>, bool)>),
    }
    // Several embedded files at once: a nested PSD gets one thread of the budget; alone, all.
    let inner = Par::new(if linked.len() > 1 { 1 } else { par.threads });
    let mut tasks: Vec<Task> = linked.into_iter().map(|(c, r)| Task::Linked(c, r)).collect();
    // The composite is long: first on the threads, last serially (the order it always had).
    if par.threads > 1 {
        tasks.insert(0, Task::Composite);
    } else {
        tasks.push(Task::Composite);
    }
    let mut composite_planned = None;
    for (task, done) in tasks.iter().zip(par_map(par.threads, tasks.iter().collect(), |t| match t {
        Task::Linked(codec, r) => Done::Linked(nested_compress(*codec, &d[r.clone()], depth, &inner)),
        Task::Composite => Done::Composite(composite_job()),
    })) {
        match (task, done) {
            (Task::Linked(codec, r), Done::Linked(Some(packed))) => {
                // Not inside the span before it (this is known only now that the earlier ones
                // are coded; such a file is coded for nothing).
                if spans.last().is_some_and(|s| s.span.end > r.start) {
                    continue;
                }
                let coding = Coding::Nested { codec: *codec, len: packed.len() };
                let span = Span { start: r.start, end: r.end, gap: 0, planes: 0, w: 0, h: 0, coding };
                spans.push(Planned { span, candidates: Vec::new(), same: None });
                nested.push(packed);
            }
            (_, Done::Composite(c)) => composite_planned = c,
            _ => {}
        }
    }
    if let Some((p, hinted)) = composite_planned {
        let si = spans.len();
        spans.push(p);
        let mut rest = 0;
        if rgb_composite {
            let has_alpha = n >= 4 && psd.layer_info.merged_alpha;
            let k = 3 + has_alpha as usize;
            groups.push(GroupDef {
                planes: (0..k).map(|p| (si, p)).collect(),
                has_alpha,
                hinted,
                same_as: vec![None; k],
            });
            rest = k;
        }
        for p in rest..n {
            groups.push(GroupDef { planes: vec![(si, p)], has_alpha: false, hinted: false, same_as: vec![None] });
        }
    }
    // Repeated single-plane spans become copies when their source is coded before them: in
    // an earlier group, or earlier in a row of the same group.
    let mut at = vec![(usize::MAX, 0usize); spans.len()];
    for (gi, g) in groups.iter().enumerate() {
        let order = GroupCoder::order(g.planes.len(), g.has_alpha);
        for (rank, &k) in order.iter().enumerate() {
            at[g.planes[k].0] = (gi, rank);
        }
    }
    let mut drop = vec![false; groups.len()];
    for gi in 0..groups.len() {
        for k in 0..groups[gi].planes.len() {
            let s = groups[gi].planes[k].0;
            let Some(o) = spans[s].same else { continue };
            if spans[s].span.planes != 1 || spans[o].span.planes != 1 {
                continue;
            }
            let (og, orank) = at[o];
            let (_, rank) = at[s];
            if og < gi || og == gi && orank < rank {
                spans[s].span.coding = Coding::Copy { of: o };
                if og == gi {
                    groups[gi].same_as[k] = groups[gi].planes.iter().position(|p| p.0 == o);
                } else if groups[gi].planes.len() == 1 {
                    drop[gi] = true;
                }
            }
        }
    }
    let groups = groups.into_iter().zip(drop).filter(|(_, d)| !d).map(|(g, _)| g).collect();
    (spans, groups, nested)
}

fn put_encoder(out: &mut Vec<u8>, e: Encoder) {
    match e {
        Encoder::Photoshop { phase, period } => {
            out.push(0);
            put_opt(out, phase);
            put_opt(out, period);
        }
        Encoder::Greedy { min_run, split_rest } => {
            out.push(1 + split_rest as u8);
            put_varint(out, min_run as u64);
        }
    }
}

fn get_encoder(src: &[u8], at: &mut usize) -> Result<Encoder> {
    Ok(match get_u8(src, at)? {
        0 => {
            let phase = get_opt(src, at)?;
            let period = get_opt(src, at)?;
            if period == 0 {
                return Err(corrupt());
            }
            Encoder::Photoshop { phase, period }
        }
        t @ (1 | 2) => Encoder::Greedy { min_run: get_usize(src, at)?.max(1), split_rest: t == 2 },
        _ => return Err(corrupt()),
    })
}

fn put_layout(o: &mut Vec<u8>, skeleton: usize, spans: &[Span], groups: &[GroupDef]) {
    put_varint(o, skeleton as u64);
    put_varint(o, spans.len() as u64);
    for s in spans {
        put_varint(o, s.gap as u64);
        put_varint(o, (s.end - s.start) as u64);
        put_varint(o, s.planes as u64);
        put_varint(o, s.w as u64);
        put_varint(o, s.h as u64);
        match &s.coding {
            Coding::Raw => o.push(0),
            Coding::Rle { count_bytes, encoder, plane_lens, exceptions } => {
                o.push(*count_bytes as u8);
                put_encoder(o, *encoder);
                for &l in plane_lens {
                    put_varint(o, l as u64);
                }
                put_varint(o, exceptions.len() as u64);
                let mut pr = 0;
                for (r, bytes) in exceptions {
                    put_varint(o, (r - pr) as u64);
                    pr = *r;
                    put_varint(o, bytes.len() as u64);
                    o.extend_from_slice(bytes);
                }
            }
            Coding::Nested { codec, len } => {
                o.push(16 + *codec as u8);
                put_varint(o, *len as u64);
            }
            Coding::Copy { of } => {
                o.push(32);
                put_varint(o, *of as u64);
            }
        }
    }
    put_varint(o, groups.len() as u64);
    for g in groups {
        put_varint(o, g.planes.len() as u64);
        o.push(g.has_alpha as u8 | (g.hinted as u8) << 1);
        for (k, &(s, p)) in g.planes.iter().enumerate() {
            put_varint(o, s as u64);
            put_varint(o, p as u64);
            put_opt(o, g.same_as[k].unwrap_or(usize::MAX));
        }
    }
}

type Layout<'a> = (usize, Vec<Span<'a>>, Vec<GroupDef>);

/// Reads the layout; spans get their positions in the output of `total` bytes.
fn get_layout(src: &[u8], total: usize) -> Result<Layout<'_>> {
    let mut at = 0;
    let skeleton = get_usize(src, &mut at)?;
    let n = get_usize(src, &mut at)?;
    let mut spans: Vec<Span> = Vec::with_capacity(n.min(1 << 16));
    let mut pos = 0usize;
    for _ in 0..n {
        let gap = get_usize(src, &mut at)?;
        let len = get_usize(src, &mut at)?;
        let start = pos.checked_add(gap).ok_or_else(corrupt)?;
        let end = start.checked_add(len).filter(|&e| e <= total).ok_or_else(corrupt)?;
        pos = end;
        let planes = get_usize(src, &mut at)?;
        let w = get_usize(src, &mut at)?;
        let h = get_usize(src, &mut at)?;
        let size = planes.checked_mul(w).and_then(|v| v.checked_mul(h)).ok_or_else(corrupt)?;
        let coding = match get_u8(src, &mut at)? {
            0 => {
                if size != len || size == 0 {
                    return Err(corrupt());
                }
                Coding::Raw
            }
            cb @ (2 | 4) => {
                let cb = cb as usize;
                let encoder = get_encoder(src, &mut at)?;
                if planes == 0 || planes > 64 || w == 0 || h == 0 {
                    return Err(corrupt());
                }
                let mut plane_lens = Vec::with_capacity(planes);
                for _ in 0..planes {
                    plane_lens.push(get_usize(src, &mut at)?);
                }
                let rows = planes * h;
                let used =
                    plane_lens.iter().try_fold(rows.checked_mul(cb).ok_or_else(corrupt)?, |a, &l| a.checked_add(l));
                if used != Some(len) {
                    return Err(corrupt());
                }
                let ne = get_usize(src, &mut at)?;
                let mut exceptions = Vec::with_capacity(ne.min(1 << 16));
                let mut r = 0usize;
                for i in 0..ne {
                    let dr = get_usize(src, &mut at)?;
                    r = r.checked_add(dr).filter(|&r| r < rows && (i == 0 || dr > 0)).ok_or_else(corrupt)?;
                    let bl = get_usize(src, &mut at)?;
                    exceptions.push((r, get_bytes(src, &mut at, bl)?));
                }
                Coding::Rle { count_bytes: cb, encoder, plane_lens, exceptions }
            }
            c @ 16..=18 => Coding::Nested {
                codec: [Codec::Psd, Codec::Png, Codec::Jpeg][c as usize - 16],
                len: get_usize(src, &mut at)?,
            },
            32 => {
                let of = get_usize(src, &mut at)?;
                // The source is an earlier coded span of one plane of the same size and bytes.
                let ok = spans.get(of).is_some_and(|s| {
                    s.planes == 1
                        && s.w == w
                        && s.h == h
                        && s.end - s.start == len
                        && matches!(s.coding, Coding::Raw | Coding::Rle { .. })
                });
                if !ok || planes != 1 {
                    return Err(corrupt());
                }
                Coding::Copy { of }
            }
            _ => return Err(corrupt()),
        };
        spans.push(Span { start, end, gap, planes, w, h, coding });
    }
    let ng = get_usize(src, &mut at)?;
    let mut groups = Vec::with_capacity(ng.min(1 << 16));
    let mut owned = vec![0usize; spans.len()];
    for _ in 0..ng {
        let np = get_usize(src, &mut at)?;
        let flags = get_u8(src, &mut at)?;
        let (has_alpha, hinted) = (flags & 1 != 0, flags & 2 != 0);
        if flags > 3 || np == 0 || np > 64 || has_alpha && np < 2 {
            return Err(corrupt());
        }
        let mut planes: Vec<PlaneRef> = Vec::with_capacity(np);
        let mut same_as = Vec::with_capacity(np);
        for _ in 0..np {
            let s = get_usize(src, &mut at)?;
            let p = get_usize(src, &mut at)?;
            let sp = spans.get(s).ok_or_else(corrupt)?;
            let first = &spans[planes.first().map_or(s, |f| f.0)];
            if p >= sp.planes || matches!(sp.coding, Coding::Nested { .. }) || sp.w != first.w || sp.h != first.h {
                return Err(corrupt());
            }
            planes.push((s, p));
            same_as.push(match get_opt(src, &mut at)? {
                usize::MAX => None,
                j => Some(j),
            });
        }
        // Planes are unique; a plane equal to another is a copy of it, coded before it in a
        // row; a hinted group is all of one span.
        let order = GroupCoder::order(np, has_alpha);
        let rank = |k: usize| order.iter().position(|&o| o == k).unwrap();
        for (k, &(s, p)) in planes.iter().enumerate() {
            if planes[..k].contains(&(s, p)) {
                return Err(corrupt());
            }
            if let Some(j) = same_as[k] {
                let ok =
                    j < np && rank(j) < rank(k) && matches!(spans[s].coding, Coding::Copy { of } if planes[j].0 == of);
                if !ok {
                    return Err(corrupt());
                }
            }
            if !matches!(spans[s].coding, Coding::Copy { .. }) {
                owned[s] += 1;
                if owned[s] > spans[s].planes {
                    return Err(corrupt());
                }
            }
        }
        if hinted && (np < 3 || planes.iter().any(|p| p.0 != planes[0].0)) {
            return Err(corrupt());
        }
        groups.push(GroupDef { planes, has_alpha, hinted, same_as });
    }
    if at != src.len() {
        return Err(corrupt());
    }
    Ok((skeleton, spans, groups))
}

/// A zstd level for a skeleton of `n` bytes in a file of `total`: the strongest that keeps it
/// a small part of the time budget (about 100 MB/s of file).
fn skeleton_level(n: usize, total: usize) -> i32 {
    // Approximate single-thread zstd compression speeds (MB/s).
    const LEVELS: [(i32, f64); 6] = [(19, 8.0), (12, 30.0), (9, 60.0), (6, 120.0), (3, 300.0), (1, 500.0)];
    let need = 500.0 * n as f64 / total.max(1) as f64;
    LEVELS.iter().find(|l| l.1 >= need).map_or(1, |l| l.0)
}

/// Compresses a file; never fails (a file that cannot be coded is stored). One thread.
pub fn compress(file: &[u8]) -> Vec<u8> {
    compress_with(file, &Options::default())
}

/// `compress` with the options: the same bytes whatever `opt.threads`.
pub fn compress_with(file: &[u8], opt: &Options) -> Vec<u8> {
    match try_compress(file, 0, &Par::new(opt.threads)) {
        Some(out) => out,
        None => {
            let mut out = Vec::with_capacity(6 + file.len() / 2);
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&[VERSION, 0]);
            let mut z = zstd::stream::Encoder::new(&mut out, skeleton_level(file.len(), file.len())).expect("zstd");
            z.set_pledged_src_size(Some(file.len() as u64)).expect("zstd");
            z.write_all(file).expect("zstd");
            z.finish().expect("zstd");
            out
        }
    }
}

/// Rows pipelined through helper threads: batches of this many samples at most...
const PIPE_BATCH: usize = 1 << 20;
/// ... this many batches full between two threads (and one more being filled, one used)...
const PIPE_DEPTH: usize = 3;
/// ... for groups of at least this many samples (smaller ones are not worth the threads).
const PIPE_MIN: usize = 1 << 18;

/// Rows of a batch for rows of `w` samples in `n` planes.
fn pipe_rows(w: usize, n: usize) -> usize {
    (PIPE_BATCH / (w * n).max(1)).clamp(1, 256)
}

/// Rows handed from one thread to another: per row, a buffer per plane (empty for planes not
/// passed), and the hint's.
struct Batch {
    len: usize,
    rows: Vec<Vec<Vec<u8>>>,
    hint: Vec<Vec<Vec<u8>>>,
}

impl Batch {
    fn new(count: usize, w: usize, planes: &[bool], hint: usize) -> Batch {
        let row = |p: &bool| if *p { vec![0u8; w] } else { Vec::new() };
        Batch {
            len: 0,
            rows: (0..count).map(|_| planes.iter().map(row).collect()).collect(),
            hint: (0..count).map(|_| (0..hint).map(|_| vec![0u8; w]).collect()).collect(),
        }
    }
}

/// A source of empty batches and a sink of full ones, for a helper thread filling them.
fn batch_channels(
    count: usize,
    mk: impl Fn() -> Batch,
) -> (
    std::sync::mpsc::SyncSender<Batch>,
    std::sync::mpsc::Receiver<Batch>,
    std::sync::mpsc::Sender<Batch>,
    std::sync::mpsc::Receiver<Batch>,
) {
    let (full_tx, full_rx) = std::sync::mpsc::sync_channel(PIPE_DEPTH);
    let (free_tx, free_rx) = std::sync::mpsc::channel();
    for _ in 0..count {
        free_tx.send(mk()).expect("receiver is here");
    }
    (full_tx, full_rx, free_tx, free_rx)
}

fn join<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
    h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
}

/// Codes the rows of group `g`, straight from the file `d`; returns per plane the rows its
/// span's encoder does not reproduce (for RLE spans that are not copies).
fn encode_group(
    rec: &mut model::Recorder,
    psd: &Psd,
    d: &[u8],
    spans: &[Span],
    g: &GroupDef,
    threads: usize,
) -> Option<Vec<Vec<usize>>> {
    let n = g.planes.len();
    let (s0, _) = g.planes[0];
    let (w, h) = (spans[s0].w, spans[s0].h);
    let is_copy = |k: usize| matches!(spans[g.planes[k].0].coding, Coding::Copy { .. });
    let known: Vec<bool> = (0..n).map(|k| is_copy(k) && g.same_as[k].is_none()).collect();
    let mut readers = Vec::with_capacity(n);
    for (k, &(s, p)) in g.planes.iter().enumerate() {
        let sp = &spans[s];
        let comp = if matches!(sp.coding, Coding::Raw)
            || matches!(sp.coding, Coding::Copy { of } if matches!(spans[of].coding, Coding::Raw))
        {
            Compression::Raw
        } else {
            Compression::Rle
        };
        let src = &d[sp.start..sp.end];
        readers.push(if g.same_as[k].is_some() {
            None
        } else {
            Some(PlaneRows::new(psd.psb, comp, src, w, h, p, sp.planes).ok()?)
        });
    }
    let mut hint =
        if g.hinted { Some(RowRenderer::new(psd, d, &mut composite::Report::default()).ok()?) } else { None };
    let mut gc = GroupCoder::new(w, n, g.has_alpha, known, g.same_as.clone());
    if threads > 1 && w * h * n >= PIPE_MIN {
        let encoders: Vec<Option<Encoder>> = g
            .planes
            .iter()
            .map(|&(s, _)| if let Coding::Rle { encoder, .. } = spans[s].coding { Some(encoder) } else { None })
            .collect();
        return encode_rows_piped(rec, &mut gc, readers, hint, &encoders, w, h);
    }
    let mut hint_rows = vec![vec![0u8; w]; if g.hinted { n } else { 0 }];
    let mut rows = vec![vec![0u8; w]; n];
    let mut misses = vec![Vec::new(); n];
    let timing = std::env::var_os("PSDR_STATS").is_some();
    let mut t = [0f64; 4];
    let mut clock = std::time::Instant::now();
    let mut lap = |i: usize, t: &mut [f64; 4]| {
        if timing {
            let now = std::time::Instant::now();
            t[i] += (now - clock).as_secs_f64();
            clock = now;
        }
    };
    // Per plane, the previous row's encoded bytes and whether the encoder missed it: a row
    // encoded the same is the same row, with the same outcome.
    let mut prev: Vec<Option<(&[u8], bool)>> = vec![None; n];
    for y in 0..h {
        for k in 0..n {
            let Some(r) = &mut readers[k] else { continue };
            let enc = r.encoded();
            if let (Some(e), Some((pe, miss))) = (enc, prev[k])
                && e == pe
            {
                r.skip().ok()?;
                rows[k].copy_from_slice(gc.last(k));
                if miss {
                    misses[k].push(y);
                }
                continue;
            }
            r.next(&mut rows[k]).ok()?;
            lap(0, &mut t);
            if let (Some(enc), Coding::Rle { encoder, .. }) = (enc, &spans[g.planes[k].0].coding) {
                let miss = !packbits::reproduces(*encoder, &rows[k], enc);
                if miss {
                    misses[k].push(y);
                }
                prev[k] = Some((enc, miss));
            }
            lap(1, &mut t);
        }
        if let Some(r) = &mut hint {
            let mut refs: Vec<&mut [u8]> = hint_rows.iter_mut().map(|v| v.as_mut_slice()).collect();
            r.next_row(&mut refs).ok()?;
        }
        lap(2, &mut t);
        gc.code_row(rec, &mut rows, hint.as_ref().map(|_| &mut hint_rows[..]));
        lap(3, &mut t);
    }
    if timing {
        ENC_TIMES.add(t);
    }
    Some(misses)
}

/// `encode_group`'s loop with a thread reading the rows (and checking them against the
/// encoders, and rendering the hint) ahead, in batches, while this one codes them.
fn encode_rows_piped<'a>(
    rec: &mut model::Recorder,
    gc: &mut GroupCoder,
    mut readers: Vec<Option<PlaneRows<'a>>>,
    mut hint: Option<RowRenderer<'a>>,
    encoders: &[Option<Encoder>],
    w: usize,
    h: usize,
) -> Option<Vec<Vec<usize>>> {
    let n = readers.len();
    let per = pipe_rows(w, n);
    let read: Vec<bool> = readers.iter().map(Option::is_some).collect();
    let hn = if hint.is_some() { n } else { 0 };
    let (full_tx, full_rx, free_tx, free_rx) = batch_channels(PIPE_DEPTH + 2, || Batch::new(per, w, &read, hn));
    let timing = std::env::var_os("PSDR_STATS").is_some();
    std::thread::scope(|sc| {
        let reader = sc.spawn(move || -> Option<Vec<Vec<usize>>> {
            let mut misses = vec![Vec::new(); n];
            // Per plane, the previous row's encoded bytes and whether the encoder missed it, and
            // the previous row itself.
            let mut prev: Vec<Option<(&[u8], bool)>> = vec![None; n];
            let mut last = vec![vec![0u8; w]; n];
            let mut t = [0f64; 3];
            let mut y = 0;
            while y < h {
                // The coding thread is gone.
                let Ok(mut b) = free_rx.recv() else { return Some(misses) };
                b.len = per.min(h - y);
                for r in 0..b.len {
                    let t0 = timing.then(std::time::Instant::now);
                    for k in 0..n {
                        let Some(rd) = &mut readers[k] else { continue };
                        let enc = rd.encoded();
                        if let (Some(e), Some((pe, miss))) = (enc, prev[k])
                            && e == pe
                        {
                            rd.skip().ok()?;
                            b.rows[r][k].copy_from_slice(&last[k]);
                            if miss {
                                misses[k].push(y + r);
                            }
                            continue;
                        }
                        rd.next(&mut b.rows[r][k]).ok()?;
                        last[k].copy_from_slice(&b.rows[r][k]);
                        if let (Some(enc), Some(encoder)) = (enc, encoders[k]) {
                            let miss = !packbits::reproduces(encoder, &b.rows[r][k], enc);
                            if miss {
                                misses[k].push(y + r);
                            }
                            prev[k] = Some((enc, miss));
                        }
                    }
                    let t1 = timing.then(std::time::Instant::now);
                    if let Some(rd) = &mut hint {
                        let mut refs: Vec<&mut [u8]> = b.hint[r].iter_mut().map(|v| v.as_mut_slice()).collect();
                        rd.next_row(&mut refs).ok()?;
                    }
                    if let (Some(t0), Some(t1)) = (t0, t1) {
                        t[0] += (t1 - t0).as_secs_f64();
                        t[2] += t1.elapsed().as_secs_f64();
                    }
                }
                y += b.len;
                if full_tx.send(b).is_err() {
                    return Some(misses);
                }
            }
            if timing {
                ENC_TIMES.add([t[0], 0.0, t[2], 0.0]);
            }
            Some(misses)
        });
        let mut rows = vec![vec![0u8; w]; n];
        let mut hint_rows = vec![vec![0u8; w]; hn];
        let mut t_model = 0f64;
        let mut ok = true;
        for _ in 0..h.div_ceil(per) {
            let Ok(mut b) = full_rx.recv() else {
                ok = false;
                break;
            };
            for r in 0..b.len {
                for k in 0..n {
                    if read[k] {
                        std::mem::swap(&mut rows[k], &mut b.rows[r][k]);
                    }
                }
                for (a, c) in hint_rows.iter_mut().zip(&mut b.hint[r]) {
                    std::mem::swap(a, c);
                }
                let t0 = timing.then(std::time::Instant::now);
                gc.code_row(rec, &mut rows, (hn > 0).then_some(&mut hint_rows[..]));
                if let Some(t0) = t0 {
                    t_model += t0.elapsed().as_secs_f64();
                }
                for k in 0..n {
                    if read[k] {
                        std::mem::swap(&mut rows[k], &mut b.rows[r][k]);
                    }
                }
                for (a, c) in hint_rows.iter_mut().zip(&mut b.hint[r]) {
                    std::mem::swap(a, c);
                }
            }
            // (The reader is gone after the last batch.)
            let _ = free_tx.send(b);
        }
        if timing {
            ENC_TIMES.add([0.0, 0.0, 0.0, t_model]);
        }
        // The reader may be waiting for a batch.
        drop(free_tx);
        drop(full_rx);
        let misses = join(reader)?;
        ok.then_some(misses)
    })
}

/// Picks the encoder of RLE span `sp` among `candidates` reproducing most rows, and its
/// exceptions; `first_misses` are the rows the first candidate misses.
fn settle_encoder<'a>(
    sp: &mut Span<'a>,
    d: &'a [u8],
    psb: bool,
    candidates: &[Encoder],
    first_misses: Vec<usize>,
) -> Option<()> {
    let (start, end, w, h, planes) = (sp.start, sp.end, sp.w, sp.h, sp.planes);
    let Coding::Rle { encoder, exceptions, .. } = &mut sp.coding else { return Some(()) };
    let src = &d[start..end];
    let total = planes * h;
    let mut best = (candidates[0], first_misses);
    // A poor first guess: try the others over every row, and the tile cut inferred from every
    // row (rather than a sample).
    if best.1.len() > total / 32 + 4 {
        let mut row = vec![0u8; w];
        let cb = if psb { 4 } else { 2 };
        let full = packbits::infer_tiles_from(all_splits(src, total, cb));
        for &cand in candidates[1..].iter().chain([full].iter().filter(|&&f| !candidates.contains(&f))) {
            let mut miss = Vec::new();
            'planes: for p in 0..planes {
                let mut r = PlaneRows::new(psb, Compression::Rle, src, w, h, p, planes).ok()?;
                for y in 0..h {
                    let enc = r.encoded()?;
                    r.next(&mut row).ok()?;
                    if !packbits::reproduces(cand, &row, enc) {
                        miss.push(p * h + y);
                        if miss.len() >= best.1.len() {
                            break 'planes;
                        }
                    }
                }
            }
            if miss.len() < best.1.len() {
                best = (cand, miss);
            }
        }
    }
    *encoder = best.0;
    // The bytes of the exceptions, by walking the rows' counts.
    let cb = if psb { 4 } else { 2 };
    let mut pos = total * cb;
    let mut it = best.1.iter().peekable();
    exceptions.clear();
    for r in 0..total {
        let n = rows::count(src, cb, r);
        if it.peek() == Some(&&r) {
            exceptions.push((r, &src[pos..pos + n]));
            it.next();
        }
        pos += n;
    }
    Some(())
}

/// Compresses the file but the spans (start, end) with zstd, appending to `out`.
fn write_skeleton(out: &mut Vec<u8>, file: &[u8], spans: &[(usize, usize)], sk_len: usize) -> Option<()> {
    let mut z = zstd::stream::Encoder::new(out, skeleton_level(sk_len, file.len())).ok()?;
    // With the size known, zstd fits its window and tables (memory) to it.
    z.set_pledged_src_size(Some(sk_len as u64)).ok()?;
    z.include_contentsize(false).ok()?;
    let mut prev = 0;
    for &(start, end) in spans {
        z.write_all(&file[prev..start]).ok()?;
        prev = end;
    }
    z.write_all(&file[prev..]).ok()?;
    z.finish().ok()?;
    Some(())
}

/// Codes all groups into `out`, then settles the encoders of RLE spans (their exceptions).
/// Returns the number of symbols.
fn code_pixels<'a>(
    out: &mut Vec<u8>,
    psd: &Psd,
    file: &'a [u8],
    spans: &mut [Span<'a>],
    candidates: &[Vec<Encoder>],
    groups: &[GroupDef],
    par: &Par,
) -> Option<usize> {
    let mut misses: Vec<Option<Vec<usize>>> = vec![None; spans.len()];
    let symbols;
    {
        let mut rec = model::Recorder::new(out);
        for g in groups {
            let m = encode_group(&mut rec, psd, file, spans, g, par.threads)?;
            for (k, &(s, p)) in g.planes.iter().enumerate() {
                if matches!(spans[s].coding, Coding::Rle { .. }) {
                    misses[s].get_or_insert_with(Vec::new).extend(m[k].iter().map(|&y| p * spans[s].h + y));
                }
            }
        }
        symbols = rec.finish();
    }
    // Spans are independent of each other here.
    let work: Vec<_> = spans
        .iter_mut()
        .zip(candidates)
        .zip(misses)
        .filter_map(|((sp, c), m)| {
            let mut m = m?;
            m.sort_unstable();
            Some((sp, c, m))
        })
        .collect();
    for ok in par_map(par.threads, work, |(sp, c, m)| settle_encoder(sp, file, psd.psb, c, m)) {
        ok?;
    }
    Some(symbols)
}

fn try_compress(file: &[u8], depth: usize, par: &Par) -> Option<Vec<u8>> {
    let t0 = std::time::Instant::now();
    let psd = psd::parse(file).ok()?;
    let (planned, groups, nested) = plan(&psd, file, depth, par);
    if planned.is_empty() {
        return None;
    }
    let t_plan = t0.elapsed().as_secs_f64();
    let mut spans: Vec<Span> = Vec::with_capacity(planned.len());
    let mut candidates = Vec::with_capacity(planned.len());
    for p in planned {
        spans.push(p.span);
        candidates.push(p.candidates);
    }
    let mut out = Vec::with_capacity(file.len() / 4);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, 1]);
    out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(file).to_le_bytes());
    put_varint(&mut out, file.len() as u64);
    // Skeleton.
    let sk_len = file.len() - spans.iter().map(|s| s.end - s.start).sum::<usize>();
    let mut prev = 0;
    for s in &mut spans {
        // Spans come in file order.
        if s.start < prev {
            return None;
        }
        s.gap = s.start - prev;
        prev = s.end;
    }
    let ranges: Vec<(usize, usize)> = spans.iter().map(|s| (s.start, s.end)).collect();
    let (skeleton, t_skel, t_model);
    let (symbols, model_at);
    if par.threads > 1 {
        // The skeleton is compressed meanwhile, into a buffer of its own: it comes first in
        // the output, the embedded files and the pixels after it.
        let (sk, pixels) = std::thread::scope(|sc| {
            let sk = sc.spawn(|| {
                let t0 = std::time::Instant::now();
                let mut sk = Vec::new();
                write_skeleton(&mut sk, file, &ranges, sk_len)?;
                Some((sk, t0.elapsed().as_secs_f64()))
            });
            let mut pix = Vec::new();
            let t0 = std::time::Instant::now();
            let symbols = code_pixels(&mut pix, &psd, file, &mut spans, &candidates, &groups, par);
            let t_pix = t0.elapsed().as_secs_f64();
            let sk = sk.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
            (sk, symbols.map(|s| (s, pix, t_pix)))
        });
        let (sk, t) = sk?;
        let (syms, pix, t_pix) = pixels?;
        skeleton = sk.len();
        t_skel = t;
        out.extend_from_slice(&sk);
        drop(sk);
        for packed in &nested {
            out.extend_from_slice(packed);
        }
        drop(nested);
        model_at = out.len();
        out.extend_from_slice(&pix);
        symbols = syms;
        t_model = t_pix;
    } else {
        let t0 = std::time::Instant::now();
        let before = out.len();
        write_skeleton(&mut out, file, &ranges, sk_len)?;
        skeleton = out.len() - before;
        t_skel = t0.elapsed().as_secs_f64();
        for packed in &nested {
            out.extend_from_slice(packed);
        }
        drop(nested);
        model_at = out.len();
        let t0 = std::time::Instant::now();
        symbols = code_pixels(&mut out, &psd, file, &mut spans, &candidates, &groups, par)?;
        t_model = t0.elapsed().as_secs_f64();
    }
    let model_len = out.len() - model_at;
    let layout_at = out.len();
    put_layout(&mut out, skeleton, &spans, &groups);
    let layout_len = out.len() - layout_at;
    out.extend_from_slice(&(layout_len as u64).to_le_bytes());
    if std::env::var_os("PSDR_STATS").is_some() {
        let samples: usize =
            groups.iter().map(|g| g.planes.len() * spans[g.planes[0].0].w * spans[g.planes[0].0].h).sum();
        eprintln!(
            "skeleton {sk_len} -> {skeleton}, layout {layout_len}, model {model_len} ({samples} samples, {symbols} symbols); plan {t_plan:.3}s skeleton {t_skel:.3}s pixels {t_model:.3}s"
        );
    }
    Some(out)
}

/// Seconds per phase, summed over all threads.
pub struct Times(std::sync::Mutex<[f64; 4]>);

impl Times {
    const fn new() -> Times {
        Times(std::sync::Mutex::new([0.0; 4]))
    }

    fn add(&self, t: [f64; 4]) {
        let mut v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for i in 0..4 {
            v[i] += t[i];
        }
    }

    pub fn get(&self) -> [f64; 4] {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Planning time: RLE checks, tile inference, candidate ranking, hint sampling.
pub static PLAN_TIMES: Times = Times::new();
/// Encoding time (with `PSDR_STATS`): reading rows, checking them against the encoder,
/// rendering, model.
pub static ENC_TIMES: Times = Times::new();
/// Decoding time (with `PSDR_STATS`): known rows, rendering, model, writing rows.
pub static DEC_TIMES: Times = Times::new();

/// Writes the rows of a plane of an RLE or raw span as they are decoded.
struct RowSink<'s> {
    /// Offsets in the output of the next row's count and bytes.
    count_at: usize,
    data_at: usize,
    /// 0 for raw data.
    count_bytes: usize,
    encoder: Encoder,
    exceptions: &'s [(usize, &'s [u8])],
    /// Row index within the span (plane * h + row).
    row: usize,
    /// The previous row, and where its bytes (from the encoder, not an exception) went.
    prev: Vec<u8>,
    prev_at: Option<(usize, usize)>,
}

impl RowSink<'_> {
    fn put(&mut self, out: &mut [u8], row: &[u8], buf: &mut Vec<u8>) -> Result<()> {
        let exception = self.count_bytes > 0 && self.exceptions.first().is_some_and(|e| e.0 == self.row);
        // A row equal to the previous one encodes the same.
        if self.count_bytes > 0
            && !exception
            && let Some((at, n)) = self.prev_at
            && self.prev == row
        {
            if self.data_at + n > out.len() {
                return Err(corrupt());
            }
            out.copy_within(at..at + n, self.data_at);
            self.prev_at = Some((self.data_at, n));
            self.data_at += n;
            return self.count(out, n);
        }
        let bytes: &[u8] = if self.count_bytes == 0 {
            row
        } else if exception {
            let b = self.exceptions[0].1;
            self.exceptions = &self.exceptions[1..];
            b
        } else {
            buf.clear();
            packbits::encode(self.encoder, row, buf);
            buf
        };
        if self.count_bytes > 0 {
            self.prev_at = (!exception).then_some((self.data_at, bytes.len()));
            self.prev.clear();
            self.prev.extend_from_slice(row);
        }
        let dst = out.get_mut(self.data_at..self.data_at + bytes.len()).ok_or_else(corrupt)?;
        dst.copy_from_slice(bytes);
        self.data_at += bytes.len();
        let n = bytes.len();
        self.count(out, n)
    }

    /// Writes the count of the row of `n` bytes just written, and moves to the next row.
    fn count(&mut self, out: &mut [u8], n: usize) -> Result<()> {
        if self.count_bytes > 0 {
            if n >> (8 * self.count_bytes).min(63) != 0 {
                return Err(corrupt());
            }
            let c = out.get_mut(self.count_at..self.count_at + self.count_bytes).ok_or_else(corrupt)?;
            for (k, b) in c.iter_mut().rev().enumerate() {
                *b = (n >> (8 * k)) as u8;
            }
            self.count_at += self.count_bytes;
        }
        self.row += 1;
        Ok(())
    }
}

/// `decode_group`'s loop with a thread reading the known planes and rendering the hint
/// ahead, and another writing the rows (re-encoding PackBits) behind, in batches, while this
/// one decodes the model.
#[allow(clippy::too_many_arguments)]
fn decode_rows_piped(
    player: &mut model::Player,
    gc: &mut GroupCoder,
    w: usize,
    h: usize,
    mut readers: Vec<Option<PlaneRows>>,
    mut hint: Option<RowRenderer>,
    sinks: Vec<Option<RowSink>>,
    right: &mut [u8],
) -> Result<()> {
    let n = readers.len();
    let per = pipe_rows(w, n);
    let known: Vec<bool> = readers.iter().map(Option::is_some).collect();
    let hn = if hint.is_some() { n } else { 0 };
    let has_in = hn > 0 || known.contains(&true);
    let sink_planes: Vec<usize> = (0..n).filter(|&k| sinks[k].is_some()).collect();
    let ns = sink_planes.len();
    let (in_full_tx, in_full_rx, in_free_tx, in_free_rx) =
        batch_channels(if has_in { PIPE_DEPTH + 2 } else { 0 }, || Batch::new(per, w, &known, hn));
    let (out_full_tx, out_full_rx, out_free_tx, out_free_rx) =
        batch_channels(PIPE_DEPTH + 2, || Batch::new(per, w, &vec![true; ns], 0));
    let timing = std::env::var_os("PSDR_STATS").is_some();
    std::thread::scope(|sc| {
        let reader = has_in.then(|| {
            sc.spawn(move || -> Result<()> {
                let mut t = [0f64; 2];
                let mut y = 0;
                while y < h {
                    let Ok(mut b) = in_free_rx.recv() else { return Ok(()) };
                    b.len = per.min(h - y);
                    for r in 0..b.len {
                        let t0 = timing.then(std::time::Instant::now);
                        for (k, rd) in readers.iter_mut().enumerate() {
                            if let Some(rd) = rd {
                                rd.next(&mut b.rows[r][k])?;
                            }
                        }
                        let t1 = timing.then(std::time::Instant::now);
                        if let Some(rd) = &mut hint {
                            let mut refs: Vec<&mut [u8]> = b.hint[r].iter_mut().map(|v| v.as_mut_slice()).collect();
                            rd.next_row(&mut refs)?;
                        }
                        if let (Some(t0), Some(t1)) = (t0, t1) {
                            t[0] += (t1 - t0).as_secs_f64();
                            t[1] += t1.elapsed().as_secs_f64();
                        }
                    }
                    y += b.len;
                    if in_full_tx.send(b).is_err() {
                        return Ok(());
                    }
                }
                if timing {
                    DEC_TIMES.add([t[0], t[1], 0.0, 0.0]);
                }
                Ok(())
            })
        });
        let writer = sc.spawn(move || -> Result<()> {
            let mut sinks: Vec<RowSink> = sinks.into_iter().flatten().collect();
            let mut buf = Vec::with_capacity(w + w / 64 + 8);
            let mut t = 0f64;
            // Ends when the decoding thread is done (or gone).
            while let Ok(b) = out_full_rx.recv() {
                let t0 = timing.then(std::time::Instant::now);
                for r in 0..b.len {
                    for (j, s) in sinks.iter_mut().enumerate() {
                        s.put(right, &b.rows[r][j], &mut buf)?;
                    }
                }
                if let Some(t0) = t0 {
                    t += t0.elapsed().as_secs_f64();
                }
                // (Nobody wants it back once the decoding thread is done.)
                let _ = out_free_tx.send(b);
            }
            if timing {
                DEC_TIMES.add([0.0, 0.0, 0.0, t]);
            }
            Ok(())
        });
        let mut rows = vec![vec![0u8; w]; n];
        let mut hint_rows = vec![vec![0u8; w]; hn];
        let mut t_model = 0f64;
        let main = (|| -> Result<()> {
            let mut out_b = out_free_rx.recv().map_err(|_| corrupt())?;
            let mut filled = 0;
            let mut y = 0;
            while y < h {
                let mut b = if has_in { Some(in_full_rx.recv().map_err(|_| corrupt())?) } else { None };
                for r in 0..b.as_ref().map_or(per.min(h - y), |b| b.len) {
                    if let Some(b) = &mut b {
                        for k in (0..n).filter(|&k| known[k]) {
                            std::mem::swap(&mut rows[k], &mut b.rows[r][k]);
                        }
                        for (a, c) in hint_rows.iter_mut().zip(&mut b.hint[r]) {
                            std::mem::swap(a, c);
                        }
                    }
                    let t0 = timing.then(std::time::Instant::now);
                    gc.code_row(player, &mut rows, (hn > 0).then_some(&mut hint_rows[..]));
                    if let Some(t0) = t0 {
                        t_model += t0.elapsed().as_secs_f64();
                    }
                    if player.failed {
                        return Err(corrupt());
                    }
                    if let Some(b) = &mut b {
                        for k in (0..n).filter(|&k| known[k]) {
                            std::mem::swap(&mut rows[k], &mut b.rows[r][k]);
                        }
                        for (a, c) in hint_rows.iter_mut().zip(&mut b.hint[r]) {
                            std::mem::swap(a, c);
                        }
                    }
                    for (j, &k) in sink_planes.iter().enumerate() {
                        out_b.rows[filled][j].copy_from_slice(gc.last(k));
                    }
                    filled += 1;
                    y += 1;
                    if filled == per || y == h {
                        out_b.len = filled;
                        filled = 0;
                        out_full_tx.send(out_b).map_err(|_| corrupt())?;
                        if y < h {
                            out_b = out_free_rx.recv().map_err(|_| corrupt())?;
                        } else {
                            out_b = Batch::new(0, 0, &[], 0);
                        }
                    }
                }
                if let Some(b) = b {
                    let _ = in_free_tx.send(b);
                }
            }
            Ok(())
        })();
        if timing {
            DEC_TIMES.add([0.0, 0.0, t_model, 0.0]);
        }
        // Helpers waiting for this thread end; the writer finishes what it was sent.
        drop((in_free_tx, in_full_rx, out_full_tx, out_free_rx));
        let w_res = join(writer);
        let r_res = reader.map_or(Ok(()), join);
        r_res.and(w_res).and(main)
    })
}

/// Decodes the rows of group `g` into the output, whose part before `split` is complete
/// (sources of known planes, and the file up to the composite for a hinted group).
fn decode_group(
    player: &mut model::Player,
    out: &mut [u8],
    split: usize,
    spans: &[Span],
    g: &GroupDef,
    threads: usize,
) -> Result<()> {
    let (left, right) = out.split_at_mut(split);
    let left = &*left;
    let n = g.planes.len();
    let (s0, _) = g.planes[0];
    let (w, h) = (spans[s0].w, spans[s0].h);
    let mut known = vec![false; n];
    let mut readers: Vec<Option<PlaneRows>> = Vec::with_capacity(n);
    let mut sinks: Vec<Option<RowSink>> = Vec::with_capacity(n);
    for (k, &(s, p)) in g.planes.iter().enumerate() {
        let sp = &spans[s];
        let (mut reader, mut sink) = (None, None);
        match &sp.coding {
            Coding::Copy { of } if g.same_as[k].is_none() => {
                known[k] = true;
                let src = &spans[*of];
                let (comp, psb) = match src.coding {
                    Coding::Raw => (Compression::Raw, false),
                    Coding::Rle { count_bytes, .. } => (Compression::Rle, count_bytes == 4),
                    _ => return Err(corrupt()),
                };
                let bytes = left.get(src.start..src.end).ok_or_else(corrupt)?;
                reader = Some(PlaneRows::new(psb, comp, bytes, w, h, 0, 1)?);
            }
            Coding::Copy { .. } => {}
            Coding::Raw => {
                let at = sp.start.checked_sub(split).ok_or_else(corrupt)? + p * w * h;
                sink = Some(RowSink {
                    count_at: 0,
                    data_at: at,
                    count_bytes: 0,
                    encoder: packbits::NO_TILES,
                    exceptions: &[],
                    row: 0,
                    prev: Vec::new(),
                    prev_at: None,
                });
            }
            Coding::Rle { count_bytes, encoder, plane_lens, exceptions } => {
                let base = sp.start.checked_sub(split).ok_or_else(corrupt)?;
                let rows = sp.planes * sp.h;
                let data_at = base + rows * count_bytes + plane_lens[..p].iter().sum::<usize>();
                let first = exceptions.partition_point(|e| e.0 < p * h);
                sink = Some(RowSink {
                    count_at: base + p * h * count_bytes,
                    data_at,
                    count_bytes: *count_bytes,
                    encoder: *encoder,
                    exceptions: &exceptions[first..],
                    row: p * h,
                    prev: Vec::with_capacity(w),
                    prev_at: None,
                });
            }
            Coding::Nested { .. } => return Err(corrupt()),
        }
        readers.push(reader);
        sinks.push(sink);
    }
    let psd;
    let mut hint = None;
    if g.hinted {
        psd = psd::parse(left)?;
        if psd.width as usize != w || psd.height as usize != h {
            return Err(corrupt());
        }
        hint = Some(RowRenderer::new(&psd, left, &mut composite::Report::default())?);
    }
    let mut gc = GroupCoder::new(w, n, g.has_alpha, known, g.same_as.clone());
    if threads > 1 && w * h * n >= PIPE_MIN {
        return decode_rows_piped(player, &mut gc, w, h, readers, hint, sinks, right);
    }
    let mut hint_rows = vec![vec![0u8; w]; if g.hinted { n } else { 0 }];
    let mut rows = vec![vec![0u8; w]; n];
    let mut buf = Vec::with_capacity(w + w / 64 + 8);
    let timing = std::env::var_os("PSDR_STATS").is_some();
    let mut t = [0f64; 4];
    let mut clock = std::time::Instant::now();
    let mut lap = |i: usize, t: &mut [f64; 4]| {
        if timing {
            let now = std::time::Instant::now();
            t[i] += (now - clock).as_secs_f64();
            clock = now;
        }
    };
    for _ in 0..h {
        for k in 0..n {
            if let Some(r) = &mut readers[k] {
                r.next(&mut rows[k])?;
            }
        }
        lap(0, &mut t);
        if let Some(r) = &mut hint {
            let mut refs: Vec<&mut [u8]> = hint_rows.iter_mut().map(|v| v.as_mut_slice()).collect();
            r.next_row(&mut refs)?;
        }
        lap(1, &mut t);
        gc.code_row(player, &mut rows, hint.as_ref().map(|_| &mut hint_rows[..]));
        if player.failed {
            return Err(corrupt());
        }
        lap(2, &mut t);
        for (k, sink) in sinks.iter_mut().enumerate() {
            if let Some(s) = sink {
                s.put(right, gc.last(k), &mut buf)?;
            }
        }
        lap(3, &mut t);
    }
    if timing {
        DEC_TIMES.add(t);
    }
    Ok(())
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    decompress_threads(src, 1)
}

/// Decodes the skeleton into the gaps between the spans of `out`.
fn read_skeleton(out: &mut [u8], sk: &[u8], spans: &[Span]) -> Result<()> {
    let mut z = zstd::stream::read::Decoder::with_buffer(sk).map_err(|_| corrupt())?;
    let mut prev = 0;
    for s in spans {
        z.read_exact(&mut out[prev..s.start]).map_err(|_| corrupt())?;
        prev = s.end;
    }
    z.read_exact(&mut out[prev..]).map_err(|_| corrupt())?;
    if z.read(&mut [0u8]).map_err(|_| corrupt())? != 0 {
        return Err(corrupt());
    }
    Ok(())
}

/// Embedded files being decoded on other threads: (span index, the file).
struct Pending {
    rx: std::sync::mpsc::Receiver<(usize, Result<Vec<u8>>)>,
    left: usize,
}

impl Pending {
    /// Puts the files decoded so far into `out`; with `all`, waits for the rest.
    fn drain(&mut self, out: &mut [u8], spans: &[Span], all: bool) -> Result<()> {
        while self.left > 0 {
            let (i, data) = if all {
                self.rx.recv().map_err(|_| corrupt())?
            } else {
                match self.rx.try_recv() {
                    Ok(m) => m,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(_) => return Err(corrupt()),
                }
            };
            self.left -= 1;
            let data = data?;
            if data.len() != spans[i].end - spans[i].start {
                return Err(corrupt());
            }
            out[spans[i].start..spans[i].end].copy_from_slice(&data);
        }
        Ok(())
    }
}

/// `decompress` with `threads` threads: the same bytes whatever the number. Embedded files
/// are decoded on other threads, meanwhile with the skeleton and the pixels.
pub fn decompress_threads(src: &[u8], threads: usize) -> Result<Vec<u8>> {
    if src.len() < 6 || &src[..4] != MAGIC || src[4] != VERSION {
        return Err(Error("not a psd-recons file".into()));
    }
    match src[5] {
        0 => return zstd::stream::decode_all(&src[6..]).map_err(|_| corrupt()),
        1 => {}
        _ => return Err(corrupt()),
    }
    let mut at = 6;
    let hash = u64::from_le_bytes(get_bytes(src, &mut at, 8)?.try_into().unwrap());
    let total = get_usize(src, &mut at)?;
    if total > 1 << 42 || src.len() < at + 8 {
        return Err(corrupt());
    }
    let ll = u64::from_le_bytes(src[src.len() - 8..].try_into().unwrap());
    let layout_at = usize::try_from(ll)
        .ok()
        .and_then(|l| (src.len() - 8).checked_sub(l))
        .filter(|&a| a >= at)
        .ok_or_else(corrupt)?;
    let (skeleton, spans, groups) = get_layout(&src[layout_at..src.len() - 8], total)?;
    let body = &src[at..layout_at];
    let mut out = vec![0u8; total];
    let sk = body.get(..skeleton).ok_or_else(corrupt)?;
    // Embedded files, in order after the skeleton.
    let mut at = skeleton;
    let mut files = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        if let Coding::Nested { codec, len } = s.coding {
            files.push((i, codec, get_bytes(body, &mut at, len)?));
        }
    }
    let rest = &body[at..];
    if threads <= 1 || files.is_empty() {
        read_skeleton(&mut out, sk, &spans)?;
        let par = Par::new(threads);
        for (i, codec, packed) in files {
            let data = nested_decompress(codec, packed, &par)?;
            if data.len() != spans[i].end - spans[i].start {
                return Err(corrupt());
            }
            out[spans[i].start..spans[i].end].copy_from_slice(&data);
        }
        decode_pixels(&mut out, &spans, &groups, rest, None, threads)?;
    } else {
        // The files go to the other threads (a nested PSD with one thread unless alone).
        let workers = (threads - 1).max(1).min(files.len());
        let par = Par::new(if files.len() > 1 { 1 } else { threads });
        let (tx, rx) = std::sync::mpsc::channel();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut pending = Pending { rx, left: files.len() };
        let res = std::thread::scope(|sc| {
            for _ in 0..workers {
                let tx = tx.clone();
                let (files, par, next, stop) = (&files, &par, &next, &stop);
                sc.spawn(move || {
                    use std::sync::atomic::Ordering::Relaxed;
                    while !stop.load(Relaxed) {
                        let Some(&(i, codec, packed)) = files.get(next.fetch_add(1, Relaxed)) else { break };
                        if tx.send((i, nested_decompress(codec, packed, par))).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            let res = read_skeleton(&mut out, sk, &spans)
                .and_then(|()| decode_pixels(&mut out, &spans, &groups, rest, Some(&mut pending), threads));
            // Whatever the outcome, the workers take no further file.
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            res
        });
        res?;
    }
    if xxhash_rust::xxh3::xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch".into()));
    }
    Ok(out)
}

/// Decodes the pixels of all groups into `out` (skeleton in place), and the copies.
/// `pending` brings the embedded files, which are needed by a hinted group (it renders the
/// file's layers) and at the end.
fn decode_pixels(
    out: &mut Vec<u8>,
    spans: &[Span],
    groups: &[GroupDef],
    coded: &[u8],
    mut pending: Option<&mut Pending>,
    threads: usize,
) -> Result<()> {
    let total = out.len();
    let mut player = model::Player::new(coded);
    let mut done = vec![false; spans.len()];
    for (i, s) in spans.iter().enumerate() {
        done[i] = matches!(s.coding, Coding::Nested { .. });
    }
    let copy_ready = |out: &mut Vec<u8>, done: &mut [bool]| {
        for (i, s) in spans.iter().enumerate() {
            if let Coding::Copy { of } = s.coding
                && !done[i]
                && done[of]
            {
                out.copy_within(spans[of].start..spans[of].end, s.start);
                done[i] = true;
            }
        }
    };
    for g in groups {
        if let Some(p) = pending.as_deref_mut() {
            p.drain(out, spans, g.hinted)?;
        }
        // Everything before the group's first written span is complete.
        let split = g
            .planes
            .iter()
            .filter(|p| !matches!(spans[p.0].coding, Coding::Copy { .. }))
            .map(|p| spans[p.0].start)
            .min()
            .unwrap_or(total);
        // Known planes (copies of earlier groups) are read from their source.
        for (k, &(s, _)) in g.planes.iter().enumerate() {
            if let Coding::Copy { of } = spans[s].coding
                && g.same_as[k].is_none()
                && (!done[of] || spans[of].end > split)
            {
                return Err(corrupt());
            }
        }
        if g.hinted && done[..g.planes[0].0].iter().any(|&d| !d) {
            return Err(corrupt());
        }
        decode_group(&mut player, out, split, spans, g, threads)?;
        for &(s, _) in &g.planes {
            if !matches!(spans[s].coding, Coding::Copy { .. }) {
                done[s] = true;
            }
        }
        copy_ready(out, &mut done);
    }
    if let Some(p) = pending {
        p.drain(out, spans, true)?;
    }
    player.finish()?;
    if done.iter().any(|&d| !d) {
        return Err(corrupt());
    }
    Ok(())
}
