//! git packs: aqr-git's formula, the data of the entries a content of the application,
//! recompressed in turn, each by the kind of its object. Its zlib streams inflate within what
//! is left of the budget ([`Options::expansion`]), and the large ones are rebuilt only where
//! that may pay ([`crate::probe`]): where the files archived along with it hold their data, a
//! blob checked out, it does.

use aqr_formula::Formula;

use crate::{Cx, Options};

pub(crate) fn pack(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    // (the streams are zlib's, all of the same settings: emulated whatever the level, they
    // take next to no record)
    let gopt = aqr_git::formula::Options {
        threads: opt.threads,
        profile: aqr_flate::ADAPTIVE,
        zlib: true,
        limit: cx.inflate_left(),
        probe: crate::probe(opt, depth),
        held: cx.held.clone().map(|h| {
            let except = h.owner(d);
            (h, except)
        }),
    };
    let e = aqr_git::formula::encode_with(d, &gopt)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}
