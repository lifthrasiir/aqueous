//! aqr-git as a module of entry formulas (see aqr-formula): a pack rebuilt from the data of its
//! entries ([`MODULE`]), their DEFLATE streams given by an application of aqr-flate's module
//! (`flate-set`) with what that derives of them (Adler-32 and sizes). `FORMAT.md` specifies
//! it. The data may be recompressed in turn: of each kind of object a stream, where the archive
//! finds the files checked out again among the blobs.

use std::collections::HashMap;
use std::sync::Arc;

use aqr_flate::formula::Derived;
use aqr_flate::{Hints, Parsed, zlib};
use aqr_formula::aqr::Encoder;
use aqr_formula::probe::{Held, Probe};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::layout::Writer;
use crate::pack::{self, BLOB, COMMIT, Entry, HEADER, ID, TAG, TREE};
use crate::{Result, corrupt};

pub use crate::module::{decode, decode_index};

/// A pack rebuilt from the data of its entries, the DEFLATE streams given by aqr-flate's module
/// ([`decode`]).
pub const MODULE: Module = Module::new("git-pack-recons", 1).with_memory(memory);

/// The memory of the module: the inputs and the pack, reserved for the bytes the inputs give
/// at most and growing past them (twice at a time, the old and new held at once) where that is
/// less than the pack.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[deflated, layout, kept] = u.inputs else { return None };
    let reserved = kept + deflated + 4 * layout + ID as u64;
    let growth = if reserved < u.output() { 2 * u.output() } else { 0 };
    Some(u.whole(growth, 1))
}

/// A pack with its index and reverse index after it ([`decode_index`]): the pack as
/// [`MODULE`] makes it, the indexes of the ids of its entries.
pub const INDEX_MODULE: Module = Module::new("git-pack-index-recons", 1).with_memory(index_memory);

/// The memory of [`INDEX_MODULE`]: [`MODULE`]'s, the indexes reserved past the pack and made
/// apart, the entries' offsets, and their order and places.
fn index_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[deflated, layout, kept, ids] = u.inputs else { return None };
    let mut at = 1;
    let pack = aqr_common::varint::get(u.params, &mut at).ok()?;
    let more = u.output().checked_sub(pack)?;
    let reserved = kept + deflated + 4 * layout + ID as u64 + more;
    let growth = if reserved < u.output() { 2 * u.output() } else { 0 };
    Some(u.whole(growth + more + ids / ID as u64 * 16, 4))
}

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
    /// About the bytes of streams made at once, in order (but the largest few and the probed),
    /// whose data and records are held until put in place: by default [`WINDOW`].
    pub window: usize,
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
            window: WINDOW,
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

/// [`Options::window`] by default.
pub const WINDOW: usize = 64 << 20;

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

/// An entry's stream made: its data and record, and the zlib settings emulation found.
type Made = Option<(Vec<u8>, Vec<u8>, Option<zlib::Params>)>;

/// The formula rebuilding the pack `input`, or None where it is not one (or no entry is
/// rebuilt). The entries whose data their headers tell fit in [`Options::limit`], that
/// inflate to as much, whose Adler-32 is right and that aqr-flate takes are rebuilt from their
/// data; the rest are kept as they are.
///
/// Each entry is parsed and deconstructed at once, so that what is held is what is made (a
/// pack of 222 MB has 817k entries and 490 MB of data); the options, which hold aqr-flate's
/// catalog, are shared.
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
    let base = aqr_flate::Options {
        zlib: opt.zlib,
        // (each entry is held to its header's size below; this bounds what a lying one inflates)
        max_data: usize::try_from(opt.limit).unwrap_or(usize::MAX),
        ..aqr_flate::Options::for_profile(opt.profile).ok()?
    };
    let fopts =
        [aqr_flate::Options { threads: 1, ..base.clone() }, aqr_flate::Options { threads: opt.threads, ..base }];
    let fopt = |threads: usize| &fopts[(threads > 1) as usize];
    let size = |&(i, _): &(usize, u64)| entries[i].deflate.len();
    // its stream parsed, if its data is as its header and Adler-32 tell
    let parse = |j: usize, threads: usize| {
        let (i, max) = jobs[j];
        let e = &entries[i];
        let p = Parsed::new(&input[e.deflate.clone()], fopt(threads)).ok()?;
        let sum = u32::from_be_bytes(input[e.deflate.end..e.end()].try_into().unwrap());
        (p.data().len() as u64 == max && aqr_common::adler32::hash(p.data()) == sum).then_some(p)
    };
    let make = |j: usize, p: Parsed, threads: usize, params: Option<zlib::Params>| -> Made {
        let e = &entries[jobs[j].0];
        let hints = Hints { params, ..Hints::zlib_header(e.zlib(input)) };
        let (data, record, report) = p.deconstruct(fopt(threads), &hints).ok()?;
        // (tiny records are much alike, so that they compress well together)
        (record.len() < e.deflate.len() + 16).then_some((data, record, report.zlib))
    };
    // The large streams, where there is a probe, one at a time, as it tells: and only where it
    // may pay (the probe's encoder is of one thread).
    let probed = |j: usize| opt.probe.is_some() && size(&jobs[j]) >= PROBE_MIN;
    // (made apart from the others: a few)
    let mut early: HashMap<usize, Made> = HashMap::new();
    if let Some(probe) = &opt.probe {
        let enc = Encoder::with_threads(probe.level, opt.threads);
        for j in (0..jobs.len()).filter(|&j| probed(j)) {
            let threads = if size(&jobs[j]) >= LARGE { opt.threads } else { 1 };
            let Some(p) = parse(j, threads) else { continue };
            let e = &entries[jobs[j].0];
            let held = || opt.held.as_ref().is_some_and(|(h, except)| h.holds(p.data(), *except)) as u8 as f64;
            let record = || p.estimate(fopt(threads)).ok();
            if probe.pays(&enc, e.deflate.len(), record, p.data(), held, kind(e.t)) {
                early.insert(j, make(j, p, threads, None));
            }
        }
    }
    // git writes every object with the same zlib settings: those of a few of the largest
    // streams are tried first on the others (taken where they predict every token).
    let mut order: Vec<usize> = (0..jobs.len()).filter(|&j| !probed(j)).collect();
    order.sort_by_key(|&j| (std::cmp::Reverse(size(&jobs[j])), j));
    order.truncate(SAMPLES);
    let run = |js: &[usize], params: Option<zlib::Params>| {
        par_jobs(js, |&j| size(&jobs[j]), opt.threads, |&j, threads| make(j, parse(j, threads)?, threads, params))
    };
    let mut seen: Vec<(zlib::Params, usize)> = Vec::new();
    for (&j, m) in order.iter().zip(run(&order, None)) {
        if let Some(p) = m.as_ref().and_then(|m| m.2) {
            match seen.iter_mut().find(|s| s.0 == p) {
                Some(s) => s.1 += 1,
                None => seen.push((p, 1)),
            }
        }
        early.insert(j, m);
    }
    let params = seen.iter().max_by_key(|s| s.1).map(|s| s.0);

    // The others made a window at a time, in order, and put in place as they are made.
    let mut w = Writer::new(&input[..HEADER]);
    let declared = jobs.iter().map(|j| j.1).sum::<u64>();
    let mut data = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
    let mut records = Vec::new();
    // the kind of each entry's data and its end; the lengths of each rebuilt stream's data and
    // record; what flate-set derives of each
    let (mut kinds, mut lens, mut derived) = (Vec::<(Kind, usize)>::new(), Vec::new(), Vec::new());
    // (and the bytes of the streams)
    let mut streams = 0;
    let (mut next, mut at) = (0, 0);
    while at < entries.len() {
        let start = next;
        let mut bytes = 0;
        while next < jobs.len() && (next == start || bytes < opt.window) {
            bytes += size(&jobs[next]);
            next += 1;
        }
        let todo: Vec<usize> = (start..next).filter(|&j| !probed(j) && !early.contains_key(&j)).collect();
        let mut made: HashMap<usize, Made> = todo.iter().copied().zip(run(&todo, params)).collect();
        let until = jobs.get(next).map_or(entries.len(), |j| j.0);
        let mut j = start;
        for (i, e) in entries.iter().enumerate().take(until).skip(at) {
            let m = match jobs.get(j) {
                Some(&(ji, _)) if ji == i => {
                    j += 1;
                    early.remove(&(j - 1)).or_else(|| made.remove(&(j - 1))).flatten()
                }
                _ => None,
            };
            match m {
                None => w.kept(&input[e.header.start..e.end()]),
                Some((d, r, _)) => {
                    w.rebuilt(e.t, d.len() as u64, &input[e.header.clone()], &input[e.base.clone()], e.zlib(input));
                    Derived::Zlib.put(&d, e.deflate.len(), &mut derived);
                    streams += e.deflate.len();
                    lens.push((d.len(), r.len()));
                    data.extend_from_slice(&d);
                    records.extend_from_slice(&r);
                    kinds.push((kind(e.t), data.len()));
                }
            }
        }
        at = until;
    }
    if lens.is_empty() {
        return None;
    }
    let trailer = input.get(end..end + ID).is_some_and(|t| *t == pack::sha1(&input[..end]));
    let (layout, kept) = w.finish(trailer, &input[if trailer { end + ID } else { end }..]);
    // The layout must give the pack back with the streams as they are: a check of everything
    // but aqr-flate, which is cheap.
    if !crate::layout::check(&layout, &derived, &kept, input) {
        return None;
    }
    let flate_len = streams + derived.len();
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

/// The ids of the entries of pack `pack` in order, where its index `idx` and reverse index
/// `rev` (where there is one) are what [`INDEX_MODULE`] makes of them; else None. The
/// entries are parsed on up to `threads` threads.
pub fn index_ids(pack: &[u8], idx: &[u8], rev: Option<&[u8]>, threads: usize) -> Option<Vec<u8>> {
    let mut starts = crate::index::starts(idx)?;
    starts.push(pack::entries_end(pack, &starts, threads)?);
    crate::index::ids(pack, &starts, idx, rev)
}

/// Whether `f` is the formula of a pack of `len` bytes, [`encode_with`]'s.
pub fn is_pack(f: &Formula, len: usize) -> bool {
    matches!(&f.root, Node::Apply(a) if a.module == MODULE && a.len == len as u64)
}

/// `pack_formula` (a pack's, [`is_pack`]) with the index of `idx_len` bytes and the reverse
/// index of `rev_len` (where there is one) of the ids `ids` ([`index_ids`]) after the pack,
/// [`INDEX_MODULE`]'s outputs one after another.
pub fn with_index(pack_formula: Formula, ids: Vec<u8>, idx_len: usize, rev_len: Option<usize>) -> Formula {
    let mut f = pack_formula;
    let Node::Apply(a) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else {
        panic!("not a pack's formula")
    };
    let mut a = *a;
    assert!(a.module == MODULE, "not a pack's formula");
    let pack_len = a.len;
    let ids = f.push_as("git-pack/ids", Kind::Compressed, ids);
    a.module = INDEX_MODULE;
    a.params = vec![if rev_len.is_some() { crate::module::WITH_REV } else { 0 }];
    aqr_common::varint::put(&mut a.params, pack_len);
    a.inputs.push(ids);
    let sizes = [Some(pack_len as usize), Some(idx_len), rev_len].into_iter().flatten().map(|n| n as u64).collect();
    f.root = Node::Concat(f.split(a, sizes));
    f
}

/// The formula of pack `pack` with its index `idx` and reverse index `rev` (where there is
/// one) of `pack_formula` (the pack's own, [`encode_with`]'s), as [`with_index`] makes it,
/// where [`index_ids`] takes them; else None.
pub fn encode_index(
    pack_formula: Formula,
    pack: &[u8],
    idx: &[u8],
    rev: Option<&[u8]>,
    threads: usize,
) -> Option<Formula> {
    if !is_pack(&pack_formula, pack.len()) {
        return None;
    }
    let ids = index_ids(pack, idx, rev, threads)?;
    Some(with_index(pack_formula, ids, idx.len(), rev.map(<[u8]>::len)))
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
        m if m == MODULE && res.is_empty() => decode(&a.params, inputs, a.len, threads),
        m if m == INDEX_MODULE && res.is_empty() => decode_index(&a.params, inputs, a.len, threads),
        m if m == aqr_flate::formula::MODULE => aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads)
            .map_err(|_| crate::Error("DEFLATE stream not rebuilt")),
        _ => Err(corrupt()),
    }
}
