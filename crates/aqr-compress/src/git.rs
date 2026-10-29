//! git packs: aqr-git's formula, the data of the entries a content of the application,
//! recompressed in turn, each by the kind of its object. Its zlib streams inflate within what
//! is left of the budget ([`Options::expansion`]), and the large ones are rebuilt only where
//! that may pay ([`crate::probe`]): where the files archived along with it hold their data, a
//! blob checked out, it does.
//!
//! A pack's index and reverse index among the contents archived along with it are coded with
//! it ([`sets`]): of them, only the ids of the entries are stored.

use std::collections::HashMap;

use aqr_formula::{Formula, Node};
use aqr_git::index::{IDX_MAGIC, REV_MAGIC};
use aqr_git::pack::ID;

use crate::sets::Set;
use crate::{Cx, Made, Options};

pub(crate) fn pack(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let gopt = aqr_git::formula::Options {
        threads: opt.threads,
        profile,
        zlib,
        limit: cx.inflate_left(),
        probe: crate::probe(opt, depth),
        held: cx.held.clone().map(|h| {
            let except = h.owner(d);
            (h, except)
        }),
        ..Default::default()
    };
    let e = aqr_git::formula::encode_with(d, &gopt)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// The packs among the contents `children` of `f` (each its slice and its formula) with their
/// indexes and reverse indexes, those found by the pack's trailer that theirs repeat, each a
/// set of the pack, the index and the reverse index (where there is one), in that order
/// ([`aqr_git::formula::with_index`]); their formulas are taken out of `children`.
pub(crate) fn sets(f: &Formula, children: &mut [(Node, Option<Made>)], opt: &Options) -> Vec<Set> {
    let data = |slice: &Node| -> Option<&[u8]> {
        let &Node::Slice { stream, start, len } = slice else { return None };
        f.streams.get(stream)?.data.get(start as usize..)?.get(..len as usize)
    };
    // the indexes and reverse indexes, by the pack's trailer they hold
    let (mut idx, mut rev) = (HashMap::new(), HashMap::new());
    for (i, (slice, _)) in children.iter().enumerate() {
        let Some(d) = data(slice).filter(|d| d.len() >= 2 * ID) else { continue };
        let trailer = &d[d.len() - 2 * ID..][..ID];
        if d.starts_with(&IDX_MAGIC) {
            idx.entry(trailer).or_insert(i);
        } else if d.starts_with(&REV_MAGIC) {
            rev.entry(trailer).or_insert(i);
        }
    }
    let mut out = Vec::new();
    if idx.is_empty() {
        return out;
    }
    for p in 0..children.len() {
        let (slice, made) = &children[p];
        let (Some(pack), Some(made)) = (data(slice), made) else { continue };
        if pack.len() < ID || !aqr_git::formula::is_pack(&made.f, pack.len()) {
            continue;
        }
        let Some(i) = idx.remove(&pack[pack.len() - ID..]) else { continue };
        let r = rev.remove(&pack[pack.len() - ID..]);
        let (index, reverse) = (data(&children[i].0).unwrap(), r.and_then(|r| data(&children[r].0)));
        // (the index alone, where the reverse index is not what the ids make)
        let ids = |reverse| aqr_git::formula::index_ids(pack, index, reverse, opt.threads);
        let (ids, r) = match ids(reverse) {
            Some(ids) => (ids, r),
            None if r.is_some() => match ids(None) {
                Some(ids) => (ids, None),
                None => continue,
            },
            None => continue,
        };
        let pack_formula = children[p].1.take().unwrap().cut(pack);
        let (idx_len, rev_len) = (index.len(), r.map(|r| children[r].0.len() as usize));
        let set = aqr_git::formula::with_index(pack_formula, ids, idx_len, rev_len);
        children[i].1 = None;
        let mut members = vec![p, i];
        if let Some(r) = r {
            children[r].1 = None;
            members.push(r);
        }
        out.push(Set { f: set, members });
    }
    out
}
