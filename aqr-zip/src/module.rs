//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

use aqr_flate::{ADAPTIVE, Coding, ModelData};

use crate::layout;
use crate::{Error, Result, corrupt};

/// Bytes from which a DEFLATE stream gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

/// `f(item, threads)` on every element of `items`, by `threads` threads, the largest by `size`
/// first: those of at least [`LARGE`] with all of them (aqr-flate codes segments in parallel,
/// but parses a stream on one), the others with one each, all side by side, so that the small
/// fill the threads a large one leaves idle.
pub(crate) fn par_jobs<T: Sync, R: Send>(
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
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    aqr_common::pool::par_map(items.len(), threads, |i| f(&items[i]))
}

/// The module [`MODULE`]: the archive of `len` bytes rebuilt from `inputs`, the data, the
/// records, the layout and the bytes kept, with `resources`, aqr-flate's model data of the
/// records' profile.
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    if resources.len() != 2 {
        return Err(Error("resources not those of the records"));
    }
    rebuild(params, resources, inputs, len, threads)
}

/// The module [`STORED_MODULE`]: [`decode`] of an archive without deflated bodies, so without
/// records or resources.
pub fn decode_stored(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    rebuild(params, &[], inputs, len, threads)
}

fn rebuild(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
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
            let rules = aqr_flate::module::rules_from_bytes(rules).map_err(flate)?;
            let (models, priors);
            let model = if profile >= ADAPTIVE {
                priors = aqr_flate::module::priors_from_bytes(coding).map_err(flate)?;
                ModelData { rules: &rules, coding: Coding::Adaptive(&priors) }
            } else {
                models = aqr_flate::module::models_from_bytes(coding).map_err(flate)?;
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
