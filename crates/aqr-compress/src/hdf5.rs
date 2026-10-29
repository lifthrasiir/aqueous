//! HDF5 files: aqr-hdf5's formula, the data of the deflated chunks a content of aqr-flate's
//! application. The chunks inflate within what is left of the budget ([`Options::expansion`]).

use aqr_formula::Formula;

use crate::{Cx, Options};

pub(crate) fn file(d: &[u8], opt: &Options, _depth: u32, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let hopt = aqr_hdf5::formula::Options { threads: opt.threads, profile, zlib, limit: cx.inflate_left() };
    let e = aqr_hdf5::formula::encode_with(d, &hopt)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}
