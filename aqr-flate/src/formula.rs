//! aqr-flate as a module of entry formulas (see aqr-formula): [`reconstruct_with`] on two
//! inputs, the data and the record, and no parameters. Its two resources are the model data
//! of the record's profile, in binary (FORMAT.md, "Resources"): the rules of the catalog, then
//! the default models or the priors.
//!
//! [`reconstruct_with`]: crate::reconstruct_with

use std::sync::OnceLock;

use aqr_formula::{Module, Node, Resource};

use crate::adapt::Priors;
use crate::code::{self};
use crate::tokens::{FIELDS, Rule};
use crate::{ADAPTIVE, Coding, Result};

use crate::module::RULE_BYTES;
pub use crate::module::{decode, models_from_bytes, priors_from_bytes, rules_from_bytes};

pub const MODULE: Module = Module {
    name: "flate-recons",
    version: 1,
};

/// The application rebuilding a DEFLATE stream of `len` bytes from `data` and `record`
/// (what [`crate::deconstruct`] gives with `profile`).
pub fn apply(profile: u16, data: Node, record: Node, len: u64) -> Result<Node> {
    Ok(Node::apply_with(
        MODULE,
        resources(profile)?,
        Vec::new(),
        vec![data, record],
        len,
    ))
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
