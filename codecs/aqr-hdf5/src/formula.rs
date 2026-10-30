//! aqr-hdf5 as entry formulas (see aqr-formula): an HDF5 file is its bytes with the DEFLATE
//! streams of its chunks rebuilt by aqr-flate's module (`flate-set`) from their data, which is
//! stored instead (FORMAT.md). The rest of the file, the zlib headers and checksums and the
//! Fletcher-32 checksums with it, is a stream as it is.

use std::collections::HashMap;
use std::ops::Range;

use aqr_flate::formula::Item;
use aqr_flate::{Hints, Parsed, zlib};
use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::float;
use crate::hdf5::{self, DEFLATE, FLETCHER32, FLOAT as FLOATS, File, SHUFFLE};
use crate::module::{self, Floats};

/// The chunks of a dataset of floats from the float model's coded parts ([`module::float`]).
pub const FLOAT: Module = Module::new("hdf5-recons-float", 1).with_memory(float_memory);

/// Bytes of the float model's tables (`float::Model`: 65 × 128 and 65 × 4 bits of 4 bytes).
const FLOAT_MODEL: u64 = 4 * (65 * 128 + 65 * 4);

/// The memory of `hdf5-recons-float`, from the samples `n` (the output over a sample's bytes):
/// the values, their integers of the floats' order and the samples' bits (8 bytes each), and
/// the model's tables; the output is made from the bits, after.
fn float_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let p = Floats::from_bytes(u.params)?;
    let n = p.samples()? as u64;
    Some(u.whole(3 * 8 * n + FLOAT_MODEL + 8 * float::MAX_ORDER as u64, 4))
}

/// Samples of a dataset fewer than this are not coded by the float model.
const MIN_FLOATS: usize = 1024;

/// The order of the float model's predictor.
const FLOAT_ORDER: u8 = 32;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// Whether datasets of floats are coded by the float model ([`FLOAT`]).
    pub floats: bool,
    /// The most bytes of data that the chunks may inflate to, all together, by the sizes of
    /// their datasets' chunks: the chunks past it are kept as they are.
    pub limit: u64,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, profile: aqr_flate::DEFAULT_PROFILE, zlib: true, floats: true, limit: u64::MAX }
    }
}

/// [`Options::limit`] for a file of `n` bytes on its own: 64 MiB, and 32 times the file.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// Streams whose zlib settings are found first, the largest, for the others.
const SAMPLES: usize = 8;

/// Bytes from which a stream gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data inflated (what counts against [`Options::limit`]).
    pub inflated: u64,
    /// Chunks rebuilt.
    pub chunks: usize,
}

/// Whether `d` starts like an HDF5 file (its superblock).
pub fn starts(d: &[u8]) -> bool {
    hdf5::starts(d)
}

/// Whether `d` starts with a user block and an HDF5 file after it (a MATLAB 7.3 file, say),
/// which is not looked for in other data.
pub fn after_user_block(d: &[u8]) -> bool {
    hdf5::superblock(d).is_some_and(|p| p > 0) && hdf5::extent(d).is_some()
}

/// The end of the HDF5 file at the start of `d`.
pub fn extent(d: &[u8]) -> Option<usize> {
    hdf5::extent(d)
}

/// The formula rebuilding the HDF5 file `input`, inflating at most [`limit`] bytes.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, limit: limit(input.len()), ..Options::default() }).map(|e| e.formula)
}

/// A chunk whose DEFLATE stream may be rebuilt: the stream, its zlib header, and the bytes of
/// data its dataset's chunks have before filtering (where its filters keep that); the chunk,
/// and where its filters before deflate are shuffle and Fletcher-32 (each perhaps, in that
/// order), whether they are there.
struct Job {
    deflate: Range<usize>,
    header: [u8; 2],
    size: Option<u64>,
    chunk: usize,
    before: Option<(bool, bool)>,
}

/// The chunks of `file` in `input` whose last filter is deflate (but for Fletcher-32 after it),
/// within `limit` bytes of data all together.
fn jobs(input: &[u8], file: &File, mut limit: u64) -> Vec<Job> {
    let mut out = Vec::new();
    for (i, c) in file.chunks.iter().enumerate() {
        let ds = &file.datasets[c.dataset];
        let active: Vec<u16> = ds.active(c.mask).map(|f| f.id).collect();
        let Some(k) = active.iter().position(|&id| id == DEFLATE) else { continue };
        let after = &active[k + 1..];
        let tail = match after {
            [] => 0,
            [FLETCHER32] => 4,
            _ => continue,
        };
        let d = &input[c.at.clone()];
        if d.len() < 2 + 4 + tail || Hints::zlib_header([d[0], d[1]]).zlib_flevel.is_none() {
            continue;
        }
        // its data: the chunk, and the checksums of Fletcher-32 before deflate; other filters
        // change its size
        let before = &active[..k];
        let size = match before.iter().all(|&id| matches!(id, hdf5::SHUFFLE | FLETCHER32)) {
            true => ds.chunk_bytes().map(|n| n + 4 * before.iter().filter(|&&id| id == FLETCHER32).count() as u64),
            false => None,
        };
        let cost = size.unwrap_or(d.len() as u64);
        if cost > limit {
            continue;
        }
        limit -= cost;
        let before = match before {
            [] => Some((false, false)),
            [SHUFFLE] => Some((true, false)),
            [FLETCHER32] => Some((false, true)),
            [SHUFFLE, FLETCHER32] => Some((true, true)),
            _ => None,
        };
        out.push(Job { deflate: c.at.start + 2..c.at.end - 4 - tail, header: [d[0], d[1]], size, chunk: i, before });
    }
    out
}

/// A chunk's stream made: its data and record, and the zlib settings emulation found.
type Made = Option<(Vec<u8>, Vec<u8>, Option<zlib::Params>)>;

/// The formula rebuilding the HDF5 file `input`, or None where it is not one (or no chunk is
/// rebuilt). The deflated chunks whose data inflates to what their dataset tells (where its
/// filters tell it), whose Adler-32 is right and that aqr-flate takes are rebuilt from their
/// data; the rest is kept as it is.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let file = hdf5::parse(input)?;
    let jobs = jobs(input, &file, opt.limit);
    if jobs.is_empty() {
        return None;
    }
    let base = aqr_flate::Options {
        zlib: opt.zlib,
        max_data: usize::try_from(opt.limit).unwrap_or(usize::MAX),
        ..aqr_flate::Options::for_profile(opt.profile).ok()?
    };
    let fopts =
        [aqr_flate::Options { threads: 1, ..base.clone() }, aqr_flate::Options { threads: opt.threads, ..base }];
    let fopt = |threads: usize| &fopts[(threads > 1) as usize];
    let parse = |j: &Job, threads: usize| {
        let p = Parsed::new(&input[j.deflate.clone()], fopt(threads)).ok()?;
        let sum = u32::from_be_bytes(*input.get(j.deflate.end..)?.first_chunk()?);
        let whole = p.len() == j.deflate.len() && j.size.is_none_or(|n| p.data().len() as u64 == n);
        (whole && aqr_common::adler32::hash(p.data()) == sum).then_some(p)
    };
    let make = |j: &Job, threads: usize, params: Option<zlib::Params>| -> Made {
        let p = parse(j, threads)?;
        let hints = Hints { params, ..Hints::zlib_header(j.header) };
        let (data, record, report) = p.deconstruct(fopt(threads), &hints).ok()?;
        (record.len() < j.deflate.len()).then_some((data, record, report.zlib))
    };
    let size = |j: &usize| jobs[*j].deflate.len();
    let run = |js: &[usize], params: Option<zlib::Params>| {
        par_jobs(js, size, opt.threads, |&j, threads| make(&jobs[j], threads, params))
    };
    // The chunks of a file are deflated alike: the settings of a few of the largest are tried
    // first on the others (taken where they predict every token).
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&j| (std::cmp::Reverse(size(&j)), j));
    order.truncate(SAMPLES);
    let mut made: HashMap<usize, Made> = HashMap::new();
    let mut seen: Vec<(zlib::Params, usize)> = Vec::new();
    for (&j, m) in order.iter().zip(run(&order, None)) {
        if let Some(p) = m.as_ref().and_then(|m| m.2) {
            match seen.iter_mut().find(|s| s.0 == p) {
                Some(s) => s.1 += 1,
                None => seen.push((p, 1)),
            }
        }
        made.insert(j, m);
    }
    let params = seen.iter().max_by_key(|s| s.1).map(|s| s.0);
    let rest: Vec<usize> = (0..jobs.len()).filter(|j| !made.contains_key(j)).collect();
    made.extend(rest.iter().copied().zip(run(&rest, params)));

    let mut f = Formula::default();
    const META: usize = 0;
    const DATA: usize = 1;
    const RECORDS: usize = 2;
    f.push("hdf5/meta", Vec::new());
    f.push_as("hdf5/data", Kind::Table, Vec::new());
    f.push("flate/record", Vec::new());
    let (mut meta, mut data, mut records) = (Vec::new(), Vec::new(), Vec::new());
    // the data of the chunks of floats coded, by job (the Fletcher-32 of their data first in
    // the meta stream)
    let coded = match opt.floats {
        true => floats(&mut f, &file, &jobs, &made, &mut meta),
        false => HashMap::new(),
    };
    let mut items = Vec::new();
    // the parts of the root: slices of the meta stream, or (as None) the next item's stream
    let mut parts: Vec<Option<Node>> = Vec::new();
    let mut at = 0;
    let mut inflated = 0;
    for (j, job) in jobs.iter().enumerate() {
        let Some((d, r, _)) = made.remove(&j).flatten() else { continue };
        let Ok(profile) = aqr_flate::record_profile(&r) else { continue };
        let kept = &input[at..job.deflate.start];
        parts.push(Some(Node::slice(META, meta.len() as u64, kept.len() as u64)));
        meta.extend_from_slice(kept);
        parts.push(None);
        items.push(Item {
            profile,
            data: match coded.get(&j) {
                Some(n) => (n.clone(), d.len()),
                None => (Node::slice(DATA, data.len() as u64, d.len() as u64), d.len()),
            },
            record: (Node::slice(RECORDS, records.len() as u64, r.len() as u64), r.len()),
            len: job.deflate.len() as u64,
        });
        inflated += d.len() as u64;
        if !coded.contains_key(&j) {
            data.extend_from_slice(&d);
        }
        records.extend_from_slice(&r);
        at = job.deflate.end;
    }
    if items.is_empty() {
        return None;
    }
    let kept = &input[at..];
    parts.push(Some(Node::slice(META, meta.len() as u64, kept.len() as u64)));
    meta.extend_from_slice(kept);
    f.streams[META].data = meta.into();
    f.streams[DATA].data = data.into();
    f.streams[RECORDS].data = records.into();
    let chunks = items.len();
    let mut streams = aqr_flate::formula::streams(&mut f, items).ok()?.into_iter();
    let parts: Vec<Node> =
        parts.into_iter().filter_map(|p| p.or_else(|| streams.next())).filter(|n| !n.is_empty()).collect();
    f.root = Node::concat(parts);
    Some(Encoded { formula: f, inflated, chunks })
}

/// The data of the chunks that the float model codes, by job, as nodes of `f`: of each dataset
/// of floats (binary32 or binary64) whose chunks were made and filtered alike (shuffle and
/// Fletcher-32 at most before deflate), of [`MIN_FLOATS`] samples at least, where the model
/// codes them smaller than zstd at level 3 does. Their chunks are the outputs of an application
/// of [`FLOAT`], each followed by its Fletcher-32 where that is before deflate (in `meta`).
fn floats(
    f: &mut Formula,
    file: &File,
    jobs: &[Job],
    made: &HashMap<usize, Made>,
    meta: &mut Vec<u8>,
) -> HashMap<usize, Node> {
    let mut out = HashMap::new();
    let mut by: HashMap<usize, Vec<usize>> = HashMap::new();
    for (j, job) in jobs.iter().enumerate() {
        if job.before.is_some() && made.get(&j).is_some_and(Option::is_some) {
            by.entry(file.chunks[job.chunk].dataset).or_default().push(j);
        }
    }
    let mut sets: Vec<(usize, Vec<usize>)> = by.into_iter().collect();
    sets.sort_unstable();
    let mut streams: Option<[usize; 3]> = None;
    for (k, mut js) in sets {
        let ds = &file.datasets[k];
        let Some(t) = ds.dtype.filter(|t| t.class == FLOATS && matches!(t.size, 4 | 8)) else { continue };
        let Some(bytes) = ds.chunk_bytes().and_then(|n| usize::try_from(n).ok()) else { continue };
        let (shuffle, fletcher) = jobs[js[0]].before.unwrap();
        // (shuffle of the element's bytes, as HDF5 sets it)
        let shuffled = ds.filters.iter().find(|f| f.id == SHUFFLE).map(|f| f.client.first() == Some(&t.size));
        if js.iter().any(|&j| jobs[j].before != Some((shuffle, fletcher)))
            || bytes % t.size as usize != 0
            || shuffle && shuffled != Some(true)
        {
            continue;
        }
        js.sort_by_key(|&j| file.chunks[jobs[j].chunk].seq);
        let p = Floats {
            chunks: js.len() as u64,
            elements: (bytes / t.size as usize) as u64,
            wide: t.size == 8,
            be: t.be,
            shuffle,
            shift: 0,
            order: FLOAT_ORDER,
        };
        let mut bits = Vec::with_capacity(p.samples().unwrap_or(0));
        for &j in &js {
            let d = &made[&j].as_ref().unwrap().0;
            p.samples_of(&d[..bytes], &mut bits);
        }
        if bits.len() < MIN_FLOATS {
            continue;
        }
        // the low bits zero in every sample, all but 8 at most
        let or = bits.iter().fold(0, |a, b| a | b);
        let p = Floats { shift: or.trailing_zeros().min(8 * p.bytes() as u32 - 8) as u8, ..p };
        let c = float::encode(&bits, p.wide, p.shift as u32, p.order as usize);
        let plain: Vec<u8> = bits.iter().flat_map(|&b| b.to_le_bytes()[..p.bytes()].to_vec()).collect();
        let zstd = zstd::bulk::compress(&plain, 3).map_or(plain.len(), |z| z.len());
        // (and not more output than the module takes of so few bytes)
        if c.coefs.len() + c.model.len() + c.raw.len() >= zstd
            || !module::within(plain.len() as u64, c.model.len() + c.raw.len())
        {
            continue;
        }
        let s = *streams.get_or_insert_with(|| {
            let mut push = |class: &'static str, kind| match f.push_as(class, kind, Vec::new()) {
                Node::Slice { stream, .. } => stream,
                _ => unreachable!(),
            };
            [
                push("hdf5/float-coefs", Kind::Table),
                push("hdf5/float-model", Kind::Compressed),
                push("hdf5/float-raw", Kind::Compressed),
            ]
        });
        let inputs = [c.coefs, c.model, c.raw]
            .into_iter()
            .zip(s)
            .map(|(v, s)| {
                let mut d = std::mem::take(&mut f.streams[s].data).into_vec();
                let at = d.len() as u64;
                d.extend_from_slice(&v);
                f.streams[s].data = d.into();
                Node::slice(s, at, v.len() as u64)
            })
            .collect();
        let a = Apply {
            module: FLOAT,
            resources: Vec::new(),
            params: p.to_bytes(),
            inputs,
            len: p.out_len().unwrap(),
            memory_limit: aqr_formula::NO_LIMIT,
        };
        for (&j, o) in js.iter().zip(f.split(a, vec![bytes as u64; js.len()])) {
            let n = match fletcher {
                true => {
                    let d = &made[&j].as_ref().unwrap().0;
                    let at = meta.len() as u64;
                    meta.extend_from_slice(&d[bytes..]);
                    Node::concat([o, Node::slice(0, at, 4)])
                }
                false => o,
            };
            out.insert(j, n);
        }
    }
    out
}

/// `f(item, threads)` on every element of `items`, by `threads` threads, the largest by `size`
/// first: those of at least [`LARGE`] with all of them (aqr-flate codes segments in parallel,
/// but parses a stream on one), the others with one each, all side by side.
fn par_jobs<T: Sync, R: Send>(
    items: &[T],
    size: impl Fn(&T) -> usize + Sync,
    threads: usize,
    f: impl Fn(&T, usize) -> R + Sync,
) -> Vec<R> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(size(&items[i])));
    let threads_of = |i: usize| if size(&items[i]) >= LARGE { threads } else { 1 };
    let done = aqr_common::pool::par_map(order.len(), threads, |k| f(&items[order[k]], threads_of(order[k])));
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in order.into_iter().zip(done) {
        out[i] = Some(r);
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// The output of `a`, an application of [`FLOAT`] or of aqr-flate's module, on `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        m if m == FLOAT && res.is_empty() => module::float(&a.params, inputs, a.len).map_err(|e| e.to_string()),
        m if m == aqr_flate::formula::MODULE => aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads)
            .map_err(|_| "DEFLATE stream not rebuilt".to_string()),
        _ => Err("not a module of aqr-hdf5's".to_string()),
    }
}
