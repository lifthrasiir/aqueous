//! HDF5 files: aqr-hdf5's formula, the data of the deflated chunks a content of aqr-flate's
//! application. The chunks inflate within what is left of the budget ([`Options::expansion`]).
//! Datasets of floats are coded by aqr-hdf5's float model where its module is built.

use aqr_formula::Formula;

use crate::{Cx, Options};

pub(crate) fn file(d: &[u8], opt: &Options, _depth: u32, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let floats = aqr_decompress::wasm_of(aqr_hdf5::formula::FLOAT).is_some();
    let mut hopt = aqr_hdf5::formula::Options {
        threads: opt.threads,
        profile,
        zlib,
        floats,
        limit: cx.inflate_left(),
        trial: true,
    };
    let mut e = aqr_hdf5::formula::encode_with(d, &hopt)?;
    // (the adaptive profile, tried where zlib is predicted exactly, needs more memory than the
    // application may take for some data: then the profile asked for)
    if e.adaptive
        && crate::memory::exceeding(&e.formula, &crate::Native { threads: 1 }, opt.application_bound).is_some()
    {
        hopt.trial = false;
        e = aqr_hdf5::formula::encode_with(d, &hopt)?;
    }
    cx.inflate(e.inflated);
    Some(e.formula)
}
