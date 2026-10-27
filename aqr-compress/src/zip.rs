//! ZIP archives: aqr-zip's formula, the contents of the entries a content of the application,
//! recompressed in turn, each by its kind by its name ([`kind_by_name`]). Its DEFLATE bodies
//! inflate within what is left of the budget ([`Options::expansion`]), and the large ones are
//! rebuilt only where that may pay ([`crate::probe`]).

use aqr_formula::Formula;

use crate::{Cx, Options, kind_by_name};

pub(crate) fn zip(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let zopt = aqr_zip::formula::Options {
        threads: opt.threads,
        profile,
        zlib,
        limit: cx.inflate_left(),
        kind: kind_by_name,
        probe: crate::probe(opt, depth),
        held: cx.held.clone().map(|h| {
            let except = h.owner(d);
            (h, except)
        }),
        ..aqr_zip::formula::Options::default()
    };
    let e = aqr_zip::formula::encode_with(d, &zopt)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}
