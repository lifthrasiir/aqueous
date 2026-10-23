//! aqr-zip as a module of entry formulas (see aqr-formula): a ZIP archive rebuilt from the
//! contents of its entries, the DEFLATE streams by aqr-flate. `FORMAT.md` specifies it. The
//! contents may be recompressed in turn.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use aqr_flate::{ADAPTIVE, Coding, ModelData};
use aqr_formula::{Formula, Module, Node};

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
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, profile: aqr_flate::DEFAULT_PROFILE, limit: u64::MAX }
    }
}

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
    // DEFLATE bodies by their data's size, measured without keeping it
    let flags = |l: &zip::Local| l.flags(input);
    let candidates: Vec<usize> = (0..locals.len())
        .filter(|&i| made[i].is_none() && locals[i].method(input) == DEFLATED && flags(locals[i]) & ENCRYPTED == 0)
        .collect();
    let sizes = par_map(&candidates, opt.threads, |&i| zip::deflate_len(&input[locals[i].body.clone()]).map(|s| s.1));
    // the least expanding first, within the limit
    let mut order: Vec<(usize, u64)> =
        candidates.iter().zip(&sizes).filter_map(|(&i, s)| Some((i, (*s)? as u64))).collect();
    order.sort_by_key(|&(i, n)| (n as u128 * u64::MAX as u128 / (locals[i].body.len() as u128 + 1), i));
    let mut budget = opt.limit;
    let chosen: Vec<usize> = order
        .into_iter()
        .filter(|&(_, n)| {
            let fits = n <= budget;
            if fits {
                budget -= n;
            }
            fits
        })
        .map(|(i, _)| i)
        .collect();
    let per = (opt.threads / chosen.len().max(1)).max(1);
    let fopt =
        aqr_flate::Options { threads: per, profile: opt.profile, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
    let split = par_map(&chosen, opt.threads, |&i| {
        let body = &input[locals[i].body.clone()];
        let (data, record) = aqr_flate::deconstruct_with(body, &fopt).ok()?;
        // kept, the body would also take its CRC-32 and size
        // (and tiny records are much alike, so that they compress well together)
        (record.len() < body.len() + 16).then_some(Made::Deflated { data, record })
    });
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
    let made: Vec<Made> = made.into_iter().map(Option::unwrap).collect();
    let crcs = par_map(&made.iter().zip(&locals).collect::<Vec<_>>(), opt.threads, |(m, l)| match m {
        Made::Stored => crc32fast::hash(&input[l.body.clone()]),
        Made::Deflated { data, .. } => crc32fast::hash(data),
        _ => 0,
    });

    let mut w = Writer::new(input, base);
    let (mut data, mut records) = (Vec::new(), Vec::new());
    let mut locals = locals.iter().zip(&made).zip(crcs);
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
                w.local(l, m, crc);
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
    let inputs = vec![
        f.push("raw", data),
        f.push("flate/record", records),
        f.push("zip/layout", layout),
        f.push("zip/kept", kept),
    ];
    f.root = Node::apply_with(MODULE, resources, Vec::new(), inputs, input.len() as u64);
    Some(Encoded { formula: f, inflated })
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
            let per = (threads / jobs.len()).max(1);
            par_map(&jobs, threads, |(d, r)| {
                let rec = &records[r.clone()];
                if aqr_flate::record_profile(rec).map_err(flate)? != profile {
                    return Err(Error("records of another profile"));
                }
                aqr_flate::reconstruct_with(&data[d.clone()], rec, model, per).map_err(flate)
            })
            .into_iter()
            .collect::<Result<_>>()?
        }
        _ => return Err(Error("resources not those of the records")),
    };
    let bodies: Vec<&[u8]> = bodies.iter().map(|b| &b[..]).collect();
    layout::build(layout, data, kept, &bodies, len)
}
