//! aqr-zip as a module of entry formulas (see aqr-formula): a ZIP archive rebuilt from the
//! contents of its entries ([`MODULE`]), its DEFLATE streams given by an application of
//! aqr-flate's module (`flate-set`) with what that derives of them (CRC-32 and sizes).
//! `FORMAT.md` specifies it. The contents may be recompressed in turn.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use aqr_flate::formula::Derived;
use aqr_flate::{Hints, Parsed};
use aqr_formula::aqr::Encoder;
use aqr_formula::probe::{self, Held, Probe};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::layout::{self, Made, Writer};
use crate::zip::{self, DEFLATED, ENCRYPTED, Rec, STORED};
use crate::{Result, corrupt};

pub use crate::module::decode;
use crate::module::{par_jobs, par_map};

/// An archive rebuilt from the contents of its entries, the deflated bodies given by aqr-flate's
/// module ([`decode`]).
pub const MODULE: Module = Module::new("zip-recons", 3).with_memory(memory);

/// The memory of the module: the inputs and the archive; for each entry (a byte of the layout
/// at least), its body's kind and what the predictions keep of a local header (about 96 bytes
/// on 32 bits), both vectors growing twice at a time; for each deflated body (`DERIVED` bytes
/// of `flate` each), its place and entry; and a name and an extra field copied at a time.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, flate, layout, _] = u.inputs else { return None };
    let deflated = layout.min(flate / layout::DERIVED as u64);
    Some(u.whole(3 * (96 + 1) * layout + 32 * deflated + 2 * 0xffff, 4))
}

/// Bodies shorter than this are not looked for among earlier ones.
const SAME_MIN: usize = 16;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
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
    /// What data archived along with the archive holds, and the data (its index) the archive is
    /// part of: the probe counts the contents held by the others as nothing, as the archive
    /// stores them once.
    pub held: Option<(Arc<Held>, Option<usize>)>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            threads: 1,
            profile: aqr_flate::DEFAULT_PROFILE,
            zlib: true,
            limit: u64::MAX,
            kind: |_| Kind::Unknown,
            probe: None,
            held: None,
        }
    }
}

/// Bodies shorter than this are rebuilt without a probe: the contents of entries of a kind are
/// archived together, and those of small entries compress far better so than alone (as a
/// probe would estimate them); and they cost little to rebuild.
pub const PROBE_MIN: usize = 256 << 10;

/// The contents of entries of at least this many bytes are slices of their own, apart from the
/// others of their kind next to them: each recompressed as a file of its own (a tar archive is
/// known only at its start), and found where the same file is archived along with them.
pub const OWN_MIN: usize = 256 << 10;

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
    let fopt = aqr_flate::Options { zlib: opt.zlib, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
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
                let group: Vec<usize> =
                    (0..jobs.len()).filter(|&e| kinds[e] == kinds[j] && parsed[e].is_some()).collect();
                let datas: Vec<&[u8]> = group.iter().map(|&e| parsed[e].as_ref().expect("parsed").0.data()).collect();
                let mut shares = shares.borrow_mut();
                for (&e, f) in group.iter().zip(probe::shared(&datas)) {
                    shares[e] = Some(f);
                }
                shares[j].unwrap_or(0.0)
            };
            let held = |j: usize| {
                let data = parsed[j].as_ref().map_or(&[][..], |p| p.0.data());
                opt.held.as_ref().is_some_and(|(h, except)| h.holds(data, *except)) as u8 as f64
            };
            let mut turned_down = Vec::new();
            for (j, p) in parsed.iter().enumerate() {
                let (i, Some((p, record, _))) = (jobs[j].0, p) else { continue };
                if probed(i)
                    && !probe.pays(
                        &enc,
                        locals[i].body.len(),
                        || *record,
                        p.data(),
                        || shared(j).max(held(j)),
                        kinds[j],
                    )
                {
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
        let made = par_jobs(
            &items,
            |&j| size(&jobs[j]),
            opt.threads,
            |&j, _| {
                let (p, fopt) = parsed[j].lock().unwrap().take()?;
                let body = &locals[jobs[j].0].body;
                match p.deconstruct(&fopt, &Hints::default()) {
                    // kept, the body would also take its CRC-32 and size
                    // (and tiny records are much alike, so that they compress well together)
                    Ok((data, record, _)) => {
                        (record.len() < body.len() + 16).then_some(Made::Deflated { data, record })
                    }
                    Err(_) => None,
                }
            },
        );
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
        Made::Stored => aqr_common::crc32::hash(&input[l.body.clone()]),
        Made::Deflated { data, .. } => aqr_common::crc32::hash(data),
        _ => 0,
    });

    let mut w = Writer::new(input, base);
    // The contents of the stored bodies and the data of the deflated ones apart, each of the
    // size they take, each body's dropped once in place; the records one after another.
    let sizes = made.iter().zip(&locals).fold((0, 0, 0), |(s, n, r), (m, l)| match m {
        Made::Stored => (s + l.body.len(), n, r),
        Made::Deflated { data, record } => (s, n + data.len(), r + record.len()),
        _ => (s, n, r),
    });
    // (the data of a body that is all of it taken as it is)
    let (mut stored, mut data, mut records) = (Vec::new(), Vec::new(), Vec::with_capacity(sizes.2));
    let put = |data: &mut Vec<u8>, d: &[u8], size: usize| {
        data.reserve_exact(size - data.len());
        data.extend_from_slice(d);
    };
    // the kind of each entry's contents and their end, of the stored and the deflated; the
    // lengths of each deflated body's data and record, and what flate-set derives of it
    let (mut stored_kinds, mut kinds) = (Vec::<(Kind, usize)>::new(), Vec::<(Kind, usize)>::new());
    let (mut lens, mut derived) = (Vec::new(), Vec::new());
    let mut locals = locals.iter().zip(made.iter_mut()).zip(crcs);
    for r in &recs {
        match r {
            Rec::Gap(g) => w.gap(g.clone()),
            Rec::Local(_) => {
                let ((l, m), crc) = locals.next().unwrap();
                w.local(l, m, crc);
                let kind = (opt.kind)(&input[l.name.clone()]);
                match m {
                    Made::Stored => {
                        put(&mut stored, &input[l.body.clone()], sizes.0);
                        stored_kinds.push((kind, stored.len()));
                    }
                    Made::Deflated { data: dd, record } => {
                        lens.push((dd.len(), record.len()));
                        derived.extend_from_slice(&crc.to_le_bytes());
                        derived.extend_from_slice(&(dd.len() as u64).to_le_bytes());
                        derived.extend_from_slice(&(l.body.len() as u64).to_le_bytes());
                        match dd.len() == sizes.1 {
                            true => data = std::mem::take(dd),
                            false => put(&mut data, &std::mem::take(dd), sizes.1),
                        }
                        records.extend_from_slice(record);
                        kinds.push((kind, data.len()));
                    }
                    _ => {}
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
    let mut flate: Vec<u8> = made
        .iter()
        .zip(locals_bodies(&recs, input))
        .filter_map(|(m, b)| matches!(m, Made::Deflated { .. }).then_some(b))
        .flatten()
        .copied()
        .collect();
    let flate_len = flate.len() + derived.len();
    flate.extend_from_slice(&derived);
    if layout::build(&layout, &stored, &flate, &kept, input.len() as u64).ok().as_deref() != Some(input) {
        return None;
    }
    drop(flate);
    // (the inputs the first streams, the kinds' after them)
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    f.push("raw", Vec::new());
    let (layout, kept) = (f.push("zip/layout", layout), f.push("zip/kept", kept));
    let flate = match lens.is_empty() {
        true => None,
        false => {
            let records = f.push("flate/record", records);
            let index = match aqr_flate::formula::index(&lens) {
                i if i.is_empty() => Node::Concat(Vec::new()),
                i => f.push("flate/index", i),
            };
            Some((records, index))
        }
    };
    let stored = f.push_by_kind(0, stored, &stored_kinds, "zip", OWN_MIN);
    let flate = match flate {
        None => Node::Concat(Vec::new()),
        Some((records, index)) => {
            let data = f.push_by_kind(1, data, &kinds, "zip", OWN_MIN);
            let a =
                aqr_flate::formula::apply(opt.profile, data, records, index, flate_len as u64, Derived::Zip).ok()?;
            Node::Apply(Box::new(a))
        }
    };
    f.root = Node::apply(MODULE, Vec::new(), vec![stored, flate, layout, kept], input.len() as u64);
    Some(Encoded { formula: f, inflated })
}

/// The bodies of the local entries of `recs`, in order.
fn locals_bodies<'a>(recs: &'a [Rec], d: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
    recs.iter().filter_map(move |r| if let Rec::Local(l) = r { Some(&d[l.body.clone()]) } else { None })
}

/// The output of `a`, an application of [`MODULE`] or of aqr-flate's, on `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        m if m == MODULE && res.is_empty() => decode(&a.params, inputs, a.len, threads),
        m if m == aqr_flate::formula::MODULE => aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads)
            .map_err(|_| crate::Error("DEFLATE stream not rebuilt")),
        _ => Err(corrupt()),
    }
}
