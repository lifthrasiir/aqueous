//! aqr-zip as a module of entry formulas (see aqr-formula): a ZIP archive rebuilt from the
//! contents of its entries, the DEFLATE streams by aqr-flate. `FORMAT.md` specifies it. The
//! contents may be recompressed in turn.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use aqr_flate::{ADAPTIVE, Coding, Hints, ModelData, Parsed};
use aqr_formula::aqr::Encoder;
use aqr_formula::probe::{self, Probe};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::layout::{self, Made, Writer};
use crate::zip::{self, DEFLATED, ENCRYPTED, Rec, STORED};
use crate::{Error, Result, corrupt};

pub const MODULE: Module = Module { name: "zip-recons", version: 1 };

/// Bodies shorter than this are not looked for among earlier ones.
const SAME_MIN: usize = 16;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// The most bytes of data that DEFLATE bodies may inflate to, all together; the bodies past
    /// it, those of the highest ratios first, are kept as they are.
    pub limit: u64,
    /// What an entry's contents are by its name (in the local header): the contents of each
    /// [`Kind`] are a stream of their own (those of [`Kind::Unknown`] the stream `raw`, the
    /// others after the four inputs), sliced in order. By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
    /// Where given, the DEFLATE bodies of at least [`PROBE_MIN`] bytes are rebuilt from their
    /// data only where the probe tells that it may pay (their contents of the kind of their
    /// names); the others are kept as they are. By default all are rebuilt.
    pub probe: Option<Probe>,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, profile: aqr_flate::DEFAULT_PROFILE, limit: u64::MAX, kind: |_| Kind::Unknown, probe: None }
    }
}

/// Bodies shorter than this are rebuilt without a probe: the contents of entries of a kind are
/// archived together, and those of small entries compress far better so than alone (as a
/// probe would estimate them); and they cost little to rebuild.
pub const PROBE_MIN: usize = 256 << 10;

/// [`Options::limit`] for an archive of `n` bytes on its own: 64 MiB, and 32 times the archive.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data inflated from DEFLATE bodies (what counts against [`Options::limit`]).
    pub inflated: u64,
}

/// The formula rebuilding the archive `input`, inflating at most [`limit`] bytes, or None where
/// it has no local entry.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, limit: limit(input.len()), ..Options::default() }).map(|e| e.formula)
}

/// Bytes from which a DEFLATE stream gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

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
    let done = par_map(&order, threads, |&i| f(&items[i], if size(&items[i]) >= LARGE { threads } else { 1 }));
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in order.into_iter().zip(done) {
        out[i] = Some(r);
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// `f` on every element of `items`, on up to `threads` threads.
fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = threads.clamp(1, items.len().max(1));
    if threads == 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut part = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(t) = items.get(i) else { break part };
                        part.push((i, f(t)));
                    }
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    out.sort_unstable_by_key(|p| p.0);
    out.into_iter().map(|p| p.1).collect()
}

/// The CRC-32 and size of the data of a body kept as it is: what the central directory says,
/// else the local header, else the data descriptor.
fn kept_entry(d: &[u8], l: &zip::Local, map: &BTreeMap<usize, zip::Info>) -> (u32, u64) {
    if let Some(i) = map.get(&l.pos) {
        return (i.crc, i.usize);
    }
    let f = l.fixed(d);
    if l.flags(d) & zip::DESCRIPTOR == 0 || l.desc == zip::Desc::None {
        return (zip::u32_at(f, 10), zip::u32_at(f, 18) as u64);
    }
    let x = l.desc_fields(d);
    (zip::u32_at(x, 0), if l.desc.long() { zip::u64_at(x, 12) } else { zip::u32_at(x, 8) as u64 })
}

/// The formula rebuilding the archive `input`, or None where it has no local entry. Stored and
/// DEFLATE bodies (within [`Options::limit`] and where aqr-flate takes them) are rebuilt from
/// their data; bodies seen before are taken again; the rest are kept as they are.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let (base, map, recs) = zip::records(input);
    let locals: Vec<&zip::Local> =
        recs.iter().filter_map(|r| if let Rec::Local(l) = r { Some(l) } else { None }).collect();
    if locals.is_empty() {
        return None;
    }
    // Bodies seen before: no need to look into them again (42.zip has every one 16 times).
    let mut first: HashMap<&[u8], usize> = HashMap::new();
    let mut made: Vec<Option<Made>> = locals
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let b = &input[l.body.clone()];
            match first.get(b) {
                Some(&j) if b.len() >= SAME_MIN => Some(Made::Same(j)),
                _ => {
                    first.entry(b).or_insert(i);
                    None
                }
            }
        })
        .collect();
    let flags = |l: &zip::Local| l.flags(input);
    let candidates: Vec<usize> = (0..locals.len())
        .filter(|&i| made[i].is_none() && locals[i].method(input) == DEFLATED && flags(locals[i]) & ENCRYPTED == 0)
        .collect();
    let fopt = aqr_flate::Options { profile: opt.profile, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
    // The bodies of `jobs` made from their data, each refused of more than it is given; None
    // where one has more. They are parsed first, the large ones' records estimated where there
    // is a probe; those are probed, what the other entries of their kind hold of their data
    // counted as nothing (the contents of a kind compress together: and an entry turned down
    // is not there for the others either), and only the bodies that may pay are rebuilt.
    let split = |jobs: &[(usize, u64)]| -> Option<Vec<Option<Made>>> {
        let fopt = |threads: usize, max: u64| aqr_flate::Options {
            threads,
            max_data: usize::try_from(max).unwrap_or(usize::MAX),
            ..fopt.clone()
        };
        let probed = |i: usize| opt.probe.is_some() && locals[i].body.len() >= PROBE_MIN;
        let size = |&(i, _): &(usize, u64)| locals[i].body.len();
        // (each with its options, made once: they hold the profile's catalog)
        let parsed = par_jobs(jobs, size, opt.threads, |&(i, max), threads| {
            let fopt = fopt(threads, max);
            match Parsed::new(&input[locals[i].body.clone()], &fopt) {
                Ok(p) => {
                    let record = if probed(i) { p.estimate(&fopt).ok() } else { None };
                    Some(Some((p, record, fopt)))
                }
                Err(aqr_flate::Error::Limit) => None,
                Err(_) => Some(None),
            }
        });
        let mut parsed: Vec<Option<(Parsed, Option<usize>, aqr_flate::Options)>> =
            parsed.into_iter().collect::<Option<_>>()?;
        if let Some(probe) = &opt.probe {
            let enc = Encoder::with_threads(probe.level, opt.threads);
            let kinds: Vec<Kind> = jobs.iter().map(|&(i, _)| (opt.kind)(&input[locals[i].name.clone()])).collect();
            // what each holds of the others of its kind, for each kind once where asked
            let shares: RefCell<Vec<Option<f64>>> = RefCell::new(vec![None; jobs.len()]);
            let shared = |j: usize| {
                if let Some(f) = shares.borrow()[j] {
                    return f;
                }
                let group: Vec<usize> = (0..jobs.len()).filter(|&e| kinds[e] == kinds[j] && parsed[e].is_some()).collect();
                let datas: Vec<&[u8]> = group.iter().map(|&e| parsed[e].as_ref().expect("parsed").0.data()).collect();
                let mut shares = shares.borrow_mut();
                for (&e, f) in group.iter().zip(probe::shared(&datas)) {
                    shares[e] = Some(f);
                }
                shares[j].unwrap_or(0.0)
            };
            let mut turned_down = Vec::new();
            for (j, p) in parsed.iter().enumerate() {
                let (i, Some((p, record, _))) = (jobs[j].0, p) else { continue };
                if probed(i) && !probe.pays(&enc, locals[i].body.len(), || *record, p.data(), || shared(j), kinds[j]) {
                    turned_down.push(j);
                }
            }
            for j in turned_down {
                parsed[j] = None;
            }
        }
        let parsed: Vec<Mutex<Option<(Parsed, aqr_flate::Options)>>> =
            parsed.into_iter().map(|p| Mutex::new(p.map(|p| (p.0, p.2)))).collect();
        let items: Vec<usize> = (0..jobs.len()).collect();
        // (on as many threads as they were parsed for: the sizes are the same)
        let made = par_jobs(&items, |&j| size(&jobs[j]), opt.threads, |&j, _| {
            let (p, fopt) = parsed[j].lock().unwrap().take()?;
            let body = &locals[jobs[j].0].body;
            match p.deconstruct(&fopt, &Hints::default()) {
                // kept, the body would also take its CRC-32 and size
                // (and tiny records are much alike, so that they compress well together)
                Ok((data, record, _)) => (record.len() < body.len() + 16).then_some(Made::Deflated { data, record }),
                Err(_) => None,
            }
        });
        Some(made)
    };
    let bound =
        candidates.iter().fold(0u64, |b, &i| b.saturating_add(locals[i].body.len() as u64 * zip::MAX_EXPANSION));
    let declared: Vec<(usize, u64)> = candidates.iter().map(|&i| (i, kept_entry(input, locals[i], &map).1)).collect();
    let fit = |jobs: &[(usize, u64)]| {
        jobs.iter().try_fold(0u64, |b, &(_, n)| b.checked_add(n)).is_some_and(|n| n <= opt.limit)
    };
    let (chosen, split) = match () {
        // all fit whatever they hold
        _ if bound <= opt.limit => {
            let all: Vec<(usize, u64)> = candidates.iter().map(|&i| (i, u64::MAX)).collect();
            (candidates, split(&all)?)
        }
        // all fit if they hold no more than the headers say, which they are held to: measuring
        // them first would choose them all as well
        _ if let Some(made) = fit(&declared).then(|| split(&declared)).flatten() => (candidates, made),
        _ => {
            // by their data's size, measured without keeping it, the least expanding first
            let sizes = par_map(&candidates, opt.threads, |&i| zip::inflated_len(&input[locals[i].body.clone()]));
            let mut order: Vec<(usize, u64)> =
                candidates.iter().zip(&sizes).filter_map(|(&i, s)| Some((i, (*s)? as u64))).collect();
            order.sort_by_key(|&(i, n)| (n as u128 * u64::MAX as u128 / (locals[i].body.len() as u128 + 1), i));
            let mut budget = opt.limit;
            let chosen: Vec<(usize, u64)> = order
                .into_iter()
                .filter(|&(_, n)| {
                    let fits = n <= budget;
                    if fits {
                        budget -= n;
                    }
                    fits
                })
                .collect();
            let made = split(&chosen)?;
            (chosen.into_iter().map(|(i, _)| i).collect(), made)
        }
    };
    let mut inflated = 0;
    for (&i, m) in chosen.iter().zip(split) {
        if let Some(Made::Deflated { data, .. }) = &m {
            inflated += data.len() as u64;
        }
        made[i] = m;
    }
    for (i, l) in locals.iter().enumerate() {
        if made[i].is_none() {
            made[i] = Some(if l.method(input) == STORED && flags(l) & ENCRYPTED == 0 {
                Made::Stored
            } else {
                let (crc, usize) = kept_entry(input, l, &map);
                Made::Kept { crc, usize }
            });
        }
    }
    let mut made: Vec<Made> = made.into_iter().map(Option::unwrap).collect();
    let crcs = par_map(&made.iter().zip(&locals).collect::<Vec<_>>(), opt.threads, |(m, l)| match m {
        Made::Stored => crc32fast::hash(&input[l.body.clone()]),
        Made::Deflated { data, .. } => crc32fast::hash(data),
        _ => 0,
    });

    let mut w = Writer::new(input, base);
    // the data and records of all the bodies, of the size they take, each body's dropped once
    // in place
    let sizes = made.iter().zip(&locals).fold((0, 0), |(n, r), (m, l)| match m {
        Made::Stored => (n + l.body.len(), r),
        Made::Deflated { data, record } => (n + data.len(), r + record.len()),
        _ => (n, r),
    });
    let (mut data, mut records) = (Vec::with_capacity(sizes.0), Vec::with_capacity(sizes.1));
    // the kind of each entry's contents, and their end in `data`
    let mut kinds: Vec<(Kind, usize)> = Vec::new();
    let mut locals = locals.iter().zip(made.iter_mut()).zip(crcs);
    for r in &recs {
        match r {
            Rec::Gap(g) => w.gap(g.clone()),
            Rec::Local(_) => {
                let ((l, m), crc) = locals.next().unwrap();
                match m {
                    Made::Stored => data.extend_from_slice(&input[l.body.clone()]),
                    Made::Deflated { data: dd, record } => {
                        data.extend_from_slice(dd);
                        records.extend_from_slice(record);
                    }
                    _ => {}
                }
                if matches!(m, Made::Stored | Made::Deflated { .. }) {
                    kinds.push(((opt.kind)(&input[l.name.clone()]), data.len()));
                }
                w.local(l, m, crc);
                if let Made::Deflated { data: dd, .. } = m {
                    *dd = Vec::new();
                }
            }
            Rec::Central(c) => w.central(c),
            Rec::End64 { pos, ext } => w.end64(*pos, ext.clone()),
            Rec::Locator { pos } => w.locator(*pos),
            Rec::End { pos, comment } => w.end(*pos, comment.clone()),
        }
    }
    let (layout, kept) = (w.layout, w.kept);
    // The layout must give the archive back with the bodies as they are: a check of everything
    // but aqr-flate, which is cheap.
    let jobs = layout::jobs(&layout, &data, &records).ok()?;
    let deflated: Vec<&[u8]> = made
        .iter()
        .zip(locals_bodies(&recs, input))
        .filter_map(|(m, b)| matches!(m, Made::Deflated { .. }).then_some(b))
        .collect();
    if jobs.len() != deflated.len()
        || layout::build(&layout, &data, &kept, &deflated, input.len() as u64).ok().as_deref() != Some(input)
    {
        return None;
    }
    let resources = if records.is_empty() { Vec::new() } else { aqr_flate::formula::resources(opt.profile).ok()? };
    let mut f = Formula::default();
    let mut inputs = vec![
        f.push("raw", Vec::new()),
        f.push("flate/record", records),
        f.push("zip/layout", layout),
        f.push("zip/kept", kept),
    ];
    inputs[0] = by_kind(&mut f, 0, data, &kinds);
    f.root = Node::apply_with(MODULE, resources, Vec::new(), inputs, input.len() as u64);
    Some(Encoded { formula: f, inflated })
}

/// `data` as streams of `f` by kind, of the contents ending at each of `kinds`: the node of
/// their slices, in order, a slice for each file of a [`Kind::Format`] (those of other kinds
/// one after another are one). The contents of [`Kind::Unknown`] are stream `raw` (empty, of
/// `f`), cut out of `data` in place; those of other kinds streams added after the others.
fn by_kind(f: &mut Formula, raw: usize, mut data: Vec<u8>, kinds: &[(Kind, usize)]) -> Node {
    if kinds.iter().all(|k| k.0 == Kind::Unknown) {
        let len = data.len() as u64;
        f.streams[raw].data = data;
        return Node::slice(raw, 0, len);
    }
    // the stream of each kind, by its first entry
    let mut order: Vec<Kind> = vec![Kind::Unknown];
    kinds.iter().for_each(|k| {
        if !order.contains(&k.0) {
            order.push(k.0)
        }
    });
    let mut of_kind: Vec<Vec<u8>> = vec![Vec::new(); order.len()];
    let (mut parts, mut start, mut at) = (Vec::<(usize, usize, usize)>::with_capacity(kinds.len()), 0, 0);
    for &(kind, end) in kinds {
        let k = order.iter().position(|&o| o == kind).unwrap();
        let len = end - start;
        let from = if k == 0 {
            data.copy_within(start..end, at);
            at += len;
            at - len
        } else {
            of_kind[k].extend_from_slice(&data[start..end]);
            of_kind[k].len() - len
        };
        match parts.last_mut() {
            _ if len == 0 => {}
            Some(p) if p.0 == k && !matches!(kind, Kind::Format(_)) => p.2 += len,
            _ => parts.push((k, from, len)),
        }
        start = end;
    }
    data.truncate(at);
    data.shrink_to_fit();
    f.streams[raw].data = data;
    let base = f.streams.len() - 1;
    for (kind, d) in order.into_iter().zip(of_kind).skip(1) {
        let class: Cow<'static, str> = match kind {
            Kind::Format(name) => format!("zip/{name}").into(),
            kind => format!("zip/{kind}").into(),
        };
        f.push_as(class, kind, d);
    }
    let stream = |k| if k == 0 { raw } else { base + k };
    let mut parts: Vec<Node> =
        parts.into_iter().map(|(k, start, len)| Node::slice(stream(k), start as u64, len as u64)).collect();
    if parts.len() == 1 { parts.pop().unwrap() } else { Node::Concat(parts) }
}

/// The bodies of the local entries of `recs`, in order.
fn locals_bodies<'a>(recs: &'a [Rec], d: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
    recs.iter().filter_map(move |r| if let Rec::Local(l) = r { Some(&d[l.body.clone()]) } else { None })
}

/// The module: the archive of `len` bytes rebuilt from `inputs`, the data, the records, the
/// layout and the bytes kept, with `resources`, aqr-flate's model data of the records' profile
/// (none where there is no record).
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, records, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    let jobs = layout::jobs(layout, data, records)?;
    let flate = |e: aqr_flate::Error| match e {
        aqr_flate::Error::Corrupt => Error("corrupt record"),
        _ => Error("DEFLATE stream not rebuilt"),
    };
    let bodies: Vec<Vec<u8>> = match (resources, jobs.first()) {
        (_, None) if resources.is_empty() => Vec::new(),
        (&[rules, coding], Some((_, r0))) => {
            let profile = aqr_flate::record_profile(&records[r0.clone()]).map_err(flate)?;
            let rules = aqr_flate::formula::rules_from_bytes(rules).map_err(flate)?;
            let (models, priors);
            let model = if profile >= ADAPTIVE {
                priors = aqr_flate::formula::priors_from_bytes(coding).map_err(flate)?;
                ModelData { rules: &rules, coding: Coding::Adaptive(&priors) }
            } else {
                models = aqr_flate::formula::models_from_bytes(coding).map_err(flate)?;
                ModelData { rules: &rules, coding: Coding::Static(&models) }
            };
            par_jobs(
                &jobs,
                |(d, _)| d.len(),
                threads,
                |(d, r), per| {
                    let rec = &records[r.clone()];
                    if aqr_flate::record_profile(rec).map_err(flate)? != profile {
                        return Err(Error("records of another profile"));
                    }
                    aqr_flate::reconstruct_with(&data[d.clone()], rec, model, per).map_err(flate)
                },
            )
            .into_iter()
            .collect::<Result<_>>()?
        }
        _ => return Err(Error("resources not those of the records")),
    };
    let bodies: Vec<&[u8]> = bodies.iter().map(|b| &b[..]).collect();
    layout::build(layout, data, kept, &bodies, len)
}
