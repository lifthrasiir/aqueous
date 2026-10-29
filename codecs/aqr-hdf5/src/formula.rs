//! aqr-hdf5 as entry formulas (see aqr-formula): an HDF5 file is its bytes with the DEFLATE
//! streams of its chunks rebuilt by aqr-flate's module (`flate-set`) from their data, which is
//! stored instead (FORMAT.md). The rest of the file, the zlib headers and checksums and the
//! Fletcher-32 checksums with it, is a stream as it is.

use std::collections::HashMap;
use std::ops::Range;

use aqr_flate::formula::Item;
use aqr_flate::{Hints, Parsed, zlib};
use aqr_formula::{Formula, Kind, Node};

use crate::hdf5::{self, DEFLATE, FLETCHER32, File};

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// The most bytes of data that the chunks may inflate to, all together, by the sizes of
    /// their datasets' chunks: the chunks past it are kept as they are.
    pub limit: u64,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, profile: aqr_flate::DEFAULT_PROFILE, zlib: true, limit: u64::MAX }
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
/// data its dataset's chunks have before filtering (where its filters keep that).
struct Job {
    deflate: Range<usize>,
    header: [u8; 2],
    size: Option<u64>,
}

/// The chunks of `file` in `input` whose last filter is deflate (but for Fletcher-32 after it),
/// within `limit` bytes of data all together.
fn jobs(input: &[u8], file: &File, mut limit: u64) -> Vec<Job> {
    let mut out = Vec::new();
    for c in &file.chunks {
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
        out.push(Job { deflate: c.at.start + 2..c.at.end - 4 - tail, header: [d[0], d[1]], size });
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
            data: (Node::slice(DATA, data.len() as u64, d.len() as u64), d.len()),
            record: (Node::slice(RECORDS, records.len() as u64, r.len() as u64), r.len()),
            len: job.deflate.len() as u64,
        });
        inflated += d.len() as u64;
        data.extend_from_slice(&d);
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

/// The output of `a`, an application of aqr-flate's module, on `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        m if m == aqr_flate::formula::MODULE => aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads)
            .map_err(|_| "DEFLATE stream not rebuilt".to_string()),
        _ => Err("not a module of aqr-hdf5's".to_string()),
    }
}
