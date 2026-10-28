//! aqr-git as a module of entry formulas (see aqr-formula): a pack rebuilt from the data of its
//! entries ([`MODULE`]), their DEFLATE streams given by an application of aqr-flate's module
//! (`flate-set`) with what that derives of them (Adler-32 and sizes). `FORMAT.md` specifies
//! it. The data may be recompressed in turn: of each kind of object a stream, where the archive
//! finds the files checked out again among the blobs.

use std::sync::{Arc, Mutex};

use aqr_flate::formula::Derived;
use aqr_flate::{Hints, Parsed, zlib};
use aqr_formula::aqr::Encoder;
use aqr_formula::probe::{Held, Probe};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::layout::Writer;
use crate::pack::{self, BLOB, COMMIT, Entry, HEADER, ID, TAG, TREE};
use crate::{Result, corrupt};

pub use crate::module::decode;

/// A pack rebuilt from the data of its entries, the DEFLATE streams given by aqr-flate's module
/// ([`decode`]).
pub const MODULE: Module = Module { name: "git-pack-recons", version: 1 };

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// The most bytes of data that the entries may inflate to, all together, by the sizes
    /// their headers tell: the entries past it are kept as they are.
    pub limit: u64,
    /// Where given, the streams of at least [`PROBE_MIN`] bytes are rebuilt from their data
    /// only where the probe tells that it may pay; the others are kept as they are. By default
    /// all are rebuilt.
    pub probe: Option<Probe>,
    /// What data archived along with the pack holds, and the data (its index) the pack is part
    /// of: the probe counts the data held by the others as nothing (a blob is the file checked
    /// out), as the archive stores it once.
    pub held: Option<(Arc<Held>, Option<usize>)>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            threads: 1,
            profile: aqr_flate::DEFAULT_PROFILE,
            zlib: true,
            limit: u64::MAX,
            probe: None,
            held: None,
        }
    }
}

/// Streams shorter than this are rebuilt without a probe: the data of the objects of a kind is
/// archived together, and that of small objects compresses far better so than alone (as a
/// probe would estimate it); and they cost little to rebuild.
pub const PROBE_MIN: usize = 256 << 10;

/// The data of entries of at least this many bytes is a slice of its own, apart from the
/// others of its kind next to it: recompressed as a file of its own, and found where the same
/// file is archived along with it.
pub const OWN_MIN: usize = 256 << 10;

/// Streams whose zlib settings are found first, the largest, for the others.
const SAMPLES: usize = 8;

/// Bytes from which a stream gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

/// [`Options::limit`] for a pack of `n` bytes on its own: 64 MiB, and 32 times the pack.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data inflated (what counts against [`Options::limit`]).
    pub inflated: u64,
}

/// The formula rebuilding the pack `input`, inflating at most [`limit`] bytes.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, limit: limit(input.len()), ..Options::default() }).map(|e| e.formula)
}

/// What the data of an object of type `t` is: commits and tags text, trees records of ids.
fn kind(t: u8) -> Kind {
    match t {
        COMMIT | TAG => Kind::Text,
        TREE => Kind::Table,
        BLOB => Kind::Unknown,
        // (a delta's inserts are of its base's type, which is not told)
        _ => Kind::Unknown,
    }
}

/// An entry's stream parsed: its data made, and the record estimated where it is probed.
type Job<'a> = Option<(Parsed<'a>, Option<usize>, aqr_flate::Options)>;

/// The formula rebuilding the pack `input`, or None where it is not one (or no entry is
/// rebuilt). The entries whose data their headers tell fit in [`Options::limit`], that
/// inflate to as much, whose Adler-32 is right and that aqr-flate takes are rebuilt from their
/// data; the rest are kept as they are.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let entries = pack::entries(input)?;
    let end = entries.last().map_or(HEADER, Entry::end);
    let mut budget = opt.limit;
    let jobs: Vec<(usize, u64)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let size = pack::header_size(&input[e.header.clone()]);
            (size <= budget).then(|| {
                budget -= size;
                (i, size)
            })
        })
        .collect();
    let fopt = aqr_flate::Options { zlib: opt.zlib, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
    let fopt = |threads: usize, max: u64| aqr_flate::Options {
        threads,
        max_data: usize::try_from(max).unwrap_or(usize::MAX),
        ..fopt.clone()
    };
    let probed = |i: usize| opt.probe.is_some() && entries[i].deflate.len() >= PROBE_MIN;
    let size = |&(i, _): &(usize, u64)| entries[i].deflate.len();
    // (each with its options, made once: they hold the profile's catalog)
    let mut parsed: Vec<Job> = par_jobs(&jobs, size, opt.threads, |&(i, max), threads| {
        let e = &entries[i];
        let fopt = fopt(threads, max);
        let p = Parsed::new(&input[e.deflate.clone()], &fopt).ok()?;
        let sum = u32::from_be_bytes(input[e.deflate.end..e.end()].try_into().unwrap());
        if p.data().len() as u64 != max || aqr_common::adler32::hash(p.data()) != sum {
            return None;
        }
        let record = if probed(i) { p.estimate(&fopt).ok() } else { None };
        Some((p, record, fopt))
    });
    if let Some(probe) = &opt.probe {
        let enc = Encoder::with_threads(probe.level, opt.threads);
        for (j, p) in parsed.iter_mut().enumerate() {
            let i = jobs[j].0;
            let Some((q, record, _)) = p else { continue };
            let held = || opt.held.as_ref().is_some_and(|(h, except)| h.holds(q.data(), *except)) as u8 as f64;
            if probed(i) && !probe.pays(&enc, entries[i].deflate.len(), || *record, q.data(), held, kind(entries[i].t))
            {
                *p = None;
            }
        }
    }
    let parsed: Vec<Mutex<Job>> = parsed.into_iter().map(Mutex::new).collect();
    // git writes every object with the same zlib settings: those of a few of the largest
    // streams are tried first on the others (taken where they predict every token).
    let mut order: Vec<usize> = (0..jobs.len()).filter(|&j| parsed[j].lock().unwrap().is_some()).collect();
    order.sort_by_key(|&j| (std::cmp::Reverse(size(&jobs[j])), j));
    let (first, rest) = order.split_at(order.len().min(SAMPLES));
    let make = |j: usize, params: Option<zlib::Params>| {
        let (p, _, fopt) = parsed[j].lock().unwrap().take()?;
        let e = &entries[jobs[j].0];
        let hints = Hints { params, ..Hints::zlib_header(e.zlib(input)) };
        let (data, record, report) = p.deconstruct(&fopt, &hints).ok()?;
        // (tiny records are much alike, so that they compress well together)
        (record.len() < e.deflate.len() + 16).then_some((data, record, report.zlib))
    };
    // (on as many threads as they were parsed for: the sizes are the same)
    let sampled = par_jobs(first, |&j| size(&jobs[j]), opt.threads, |&j, _| make(j, None));
    let mut seen: Vec<(zlib::Params, usize)> = Vec::new();
    for p in sampled.iter().flatten().filter_map(|m| m.2) {
        match seen.iter_mut().find(|s| s.0 == p) {
            Some(s) => s.1 += 1,
            None => seen.push((p, 1)),
        }
    }
    let params = seen.iter().max_by_key(|s| s.1).map(|s| s.0);
    let others = par_jobs(rest, |&j| size(&jobs[j]), opt.threads, |&j, _| make(j, params));
    let mut made: Vec<Option<(Vec<u8>, Vec<u8>)>> = (0..entries.len()).map(|_| None).collect();
    for (&j, m) in first.iter().chain(rest).zip(sampled.into_iter().chain(others)) {
        made[jobs[j].0] = m.map(|m| (m.0, m.1));
    }
    if made.iter().all(Option::is_none) {
        return None;
    }

    let mut w = Writer::new(&input[..HEADER]);
    let sizes = made.iter().flatten().fold((0, 0), |(d, r), m| (d + m.0.len(), r + m.1.len()));
    let (mut data, mut records) = (Vec::with_capacity(sizes.0), Vec::with_capacity(sizes.1));
    // the kind of each entry's data and its end; the lengths of each rebuilt stream's data and
    // record; the streams, with what flate-set derives of them after them
    let (mut kinds, mut lens) = (Vec::<(Kind, usize)>::new(), Vec::new());
    let (mut flate, mut derived) = (Vec::new(), Vec::new());
    for (e, m) in entries.iter().zip(&mut made) {
        match m.take() {
            None => w.kept(&input[e.header.start..e.end()]),
            Some((d, r)) => {
                w.rebuilt(e.t, d.len() as u64, &input[e.header.clone()], &input[e.base.clone()], e.zlib(input));
                Derived::Zlib.put(&d, e.deflate.len(), &mut derived);
                flate.extend_from_slice(&input[e.deflate.clone()]);
                lens.push((d.len(), r.len()));
                match d.len() == sizes.0 {
                    true => data = d,
                    false => data.extend_from_slice(&d),
                }
                records.extend_from_slice(&r);
                kinds.push((kind(e.t), data.len()));
            }
        }
    }
    let trailer = input.get(end..end + ID).is_some_and(|t| *t == pack::sha1(&input[..end]));
    let (layout, kept) = w.finish(trailer, &input[if trailer { end + ID } else { end }..]);
    // The layout must give the pack back with the streams as they are: a check of everything
    // but aqr-flate, which is cheap.
    let flate_len = flate.len() + derived.len();
    flate.extend_from_slice(&derived);
    if crate::layout::build(&layout, &flate, &kept, input.len() as u64).ok().as_deref() != Some(input) {
        return None;
    }
    drop(flate);
    let inflated = data.len() as u64;
    // (the input the first stream, the kinds' after the others)
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let (layout, kept) = (f.push("git-pack/layout", layout), f.push("git-pack/kept", kept));
    let records = f.push("flate/record", records);
    let index = match aqr_flate::formula::index(&lens) {
        i if i.is_empty() => Node::Concat(Vec::new()),
        i => f.push("flate/index", i),
    };
    let data = f.push_by_kind(0, data, &kinds, "git-pack", OWN_MIN);
    let a = aqr_flate::formula::apply(opt.profile, data, records, index, flate_len as u64, Derived::Zlib).ok()?;
    f.root = Node::apply(MODULE, Vec::new(), vec![Node::Apply(Box::new(a)), layout, kept], input.len() as u64);
    Some(Encoded { formula: f, inflated })
}

/// `f(item, threads)` on every element of `items`, by `threads` threads, the largest by `size`
/// first: those of at least [`LARGE`] with all of them (aqr-flate codes segments in parallel,
/// but parses a stream on one), the others with one each, all side by side, so that the small
/// fill the threads a large one leaves idle.
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

/// The output of `a`, an application of [`MODULE`] or of aqr-flate's, on `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        MODULE if res.is_empty() => decode(&a.params, inputs, a.len, threads),
        aqr_flate::formula::MODULE => aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads)
            .map_err(|_| crate::Error("DEFLATE stream not rebuilt")),
        _ => Err(corrupt()),
    }
}
