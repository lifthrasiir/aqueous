//! aqr-flate as a module of entry formulas (see aqr-formula): [`reconstruct_with`] on three
//! inputs, the data and the records of streams of one profile and the index cutting them, which
//! it gives one after another, then what its parameters derive of them ([`Derived`]). Its two resources
//! are the model data of the records' profile, in binary (FORMAT.md, "Resources"): the rules of
//! the catalog, then the default models or the priors.
//!
//! [`reconstruct_with`]: crate::reconstruct_with

use std::sync::OnceLock;

use aqr_formula::{Apply, Formula, Module, Node, Resource};

use crate::adapt::Priors;
use crate::code::{self};
use crate::tokens::{FIELDS, Rule};
use crate::{ADAPTIVE, Coding, Result};

use crate::module::RULE_BYTES;
pub use crate::module::{
    Derived, decode, index, models_from_bytes, priors_from_bytes, rules_from_bytes,
};

pub const MODULE: Module = Module {
    name: "flate-set",
    version: 1,
};

/// The application rebuilding streams of `profile` from `data` and `records` (what
/// [`crate::deconstruct`] gives, one after another) as `index` cuts them ([`index`]), of `len`
/// bytes with `derived` after them.
pub fn apply(
    profile: u16,
    data: Node,
    records: Node,
    index: Node,
    len: u64,
    derived: Derived,
) -> Result<Apply> {
    Ok(Apply {
        module: MODULE,
        resources: resources(profile)?,
        params: derived.params(),
        inputs: vec![data, records, index],
        len,
    })
}

/// The application rebuilding a single stream of `len` bytes from `data` and `record`, with
/// `derived` after it.
pub fn apply_one(
    profile: u16,
    data: Node,
    record: Node,
    len: u64,
    derived: Derived,
) -> Result<Node> {
    let len = len + derived.bytes() as u64;
    let a = apply(
        profile,
        data,
        record,
        Node::Concat(Vec::new()),
        len,
        derived,
    )?;
    Ok(Node::Apply(Box::new(a)))
}

/// A DEFLATE stream of `len` bytes to rebuild from data and a record of `profile`, of these
/// nodes and lengths.
pub struct Item {
    pub profile: u16,
    pub data: (Node, usize),
    pub record: (Node, usize),
    pub len: u64,
}

/// The streams of `items` in `f`, in order: by profile, the outputs of an application split
/// into them (or the application itself, for a single stream), their indexes streams of class
/// `flate/index`.
pub fn streams(f: &mut Formula, items: Vec<Item>) -> Result<Vec<Node>> {
    let mut profiles: Vec<u16> = items.iter().map(|i| i.profile).collect();
    profiles.sort_unstable();
    profiles.dedup();
    let mut out: Vec<Option<Node>> = (0..items.len()).map(|_| None).collect();
    let mut groups: Vec<(Vec<usize>, Vec<Item>)> =
        profiles.iter().map(|_| Default::default()).collect();
    for (k, item) in items.into_iter().enumerate() {
        let g = &mut groups[profiles.binary_search(&item.profile).unwrap()];
        g.0.push(k);
        g.1.push(item);
    }
    for (&p, (at, items)) in profiles.iter().zip(groups) {
        if let [one] = &items[..] {
            let (data, record) = (one.data.0.clone(), one.record.0.clone());
            out[at[0]] = Some(apply_one(p, data, record, one.len, Derived::None)?);
            continue;
        }
        let lens: Vec<(usize, usize)> = items.iter().map(|i| (i.data.1, i.record.1)).collect();
        let sizes: Vec<u64> = items.iter().map(|i| i.len).collect();
        let index = f.push("flate/index", crate::module::index(&lens));
        // (the data of each a content of its own, so not merged)
        let (data, records): (Vec<Node>, Vec<Node>) =
            items.into_iter().map(|i| (i.data.0, i.record.0)).unzip();
        let a = apply(
            p,
            Node::Concat(data),
            Node::concat(records),
            index,
            sizes.iter().sum(),
            Derived::None,
        )?;
        for (k, n) in at.into_iter().zip(f.split(a, sizes)) {
            out[k] = Some(n);
        }
    }
    Ok(out.into_iter().map(Option::unwrap).collect())
}

/// The resources of a profile: its built-in model data in binary.
pub fn resources(profile: u16) -> Result<Vec<Resource>> {
    static CATALOGS: [OnceLock<Resource>; crate::LAMBDAS.len()] =
        [const { OnceLock::new() }; crate::LAMBDAS.len()];
    static CODINGS: [OnceLock<Resource>; 1 + crate::PRIORS.len()] =
        [const { OnceLock::new() }; 1 + crate::PRIORS.len()];
    let m = crate::profile_data(profile)?;
    // the catalog's index, as in profile_catalog
    let k = (profile as usize).min(CATALOGS.len() - 1);
    // the default models (one for every static profile), or the priors
    let c = profile.saturating_sub(ADAPTIVE - 1) as usize;
    let catalog = CATALOGS[k].get_or_init(|| Resource::new(rules_to_bytes(m.rules)));
    let coding = CODINGS[c].get_or_init(|| {
        Resource::new(match m.coding {
            Coding::Static(models) => models_to_bytes(models),
            Coding::Adaptive(priors) => priors_to_bytes(priors),
        })
    });
    Ok(vec![catalog.clone(), coding.clone()])
}

/// A catalog's rules in binary: per rule, its fields in `RULE_BYTES` bytes.
pub fn rules_to_bytes(rules: &[Rule]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rules.len() * RULE_BYTES);
    for r in rules {
        out.extend_from_slice(&[r.kind as u8, r.key, r.ways, r.long, r.rank as u8]);
        out.extend_from_slice(&r.inner.to_le_bytes());
        out.extend_from_slice(&r.lazy.to_le_bytes());
        out.extend_from_slice(&[r.margin, r.min_len, r.far_len]);
        out.extend_from_slice(&r.far_dist.to_le_bytes());
        out.extend_from_slice(&r.window.to_le_bytes());
        out.extend_from_slice(&[r.depth, r.nice]);
    }
    out
}

/// Default models in binary: every field's [`code::Model::qs`].
pub fn models_to_bytes(models: &[code::Model; FIELDS]) -> Vec<u8> {
    models.iter().flat_map(|m| m.qs()).copied().collect()
}

/// Priors in binary: every context's probability (u16 LE), then every context's count (u8).
pub fn priors_to_bytes(p: &Priors) -> Vec<u8> {
    let mut out: Vec<u8> = p.p1.iter().flat_map(|p| p.to_le_bytes()).collect();
    out.extend_from_slice(&p.n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary model data is the built-in one.
    #[test]
    fn binary_roundtrip() {
        for p in crate::profiles() {
            let m = crate::profile_data(p).unwrap();
            let [rules, coding] = &resources(p).unwrap()[..] else {
                panic!()
            };
            assert_eq!(rules_from_bytes(rules).unwrap(), m.rules);
            match m.coding {
                Coding::Static(models) => {
                    let back = models_from_bytes(coding).unwrap();
                    assert!(back.iter().zip(models).all(|(a, b)| a.qs() == b.qs()));
                }
                Coding::Adaptive(priors) => {
                    let back = priors_from_bytes(coding).unwrap();
                    assert_eq!((back.p1, back.n), (priors.p1.clone(), priors.n.clone()));
                }
            }
        }
        // Profiles share the default models, and 9 and 10 the catalog.
        assert_eq!(resources(0).unwrap()[1], resources(9).unwrap()[1]);
        assert_eq!(resources(9).unwrap()[0], resources(10).unwrap()[0]);
    }
}
