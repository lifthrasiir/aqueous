//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module: the resources read, and the stream rebuilt.

use crate::adapt::{self, Priors};
use crate::code::{self, ALPHABET};
use crate::tokens::{FIELDS, Kind, Rule};
use crate::{ADAPTIVE, Coding, Error, ModelData, Result};

/// Bytes of a rule in a catalog resource.
pub(crate) const RULE_BYTES: usize = 18;

/// The rules of [`rules_to_bytes`], which must be valid and none of them a chain rule.
pub fn rules_from_bytes(d: &[u8]) -> Result<Vec<Rule>> {
    if !d.len().is_multiple_of(RULE_BYTES) {
        return Err(Error::Corrupt);
    }
    d.chunks_exact(RULE_BYTES)
        .map(|b| {
            let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
            let kind = match b[0] {
                0 => Kind::Search,
                1 => Kind::Literals,
                2 => Kind::Runs,
                _ => return Err(Error::Corrupt),
            };
            if b[4] > 1 {
                return Err(Error::Corrupt);
            }
            let r = Rule {
                kind,
                key: b[1],
                ways: b[2],
                long: b[3],
                rank: b[4] == 1,
                inner: u16_at(5),
                lazy: u16_at(7),
                margin: b[9],
                min_len: b[10],
                far_len: b[11],
                far_dist: u16_at(12),
                window: u16_at(14),
                depth: b[16],
                nice: b[17],
            };
            r.check().map_err(|_| Error::Corrupt)?;
            Ok(r)
        })
        .collect()
}

pub fn models_from_bytes(d: &[u8]) -> Result<Box<[code::Model; FIELDS]>> {
    if d.len() != FIELDS * ALPHABET {
        return Err(Error::Corrupt);
    }
    let mut models = Vec::with_capacity(FIELDS);
    for q in d.chunks_exact(ALPHABET) {
        models.push(code::Model::from_qs(q).ok_or(Error::Corrupt)?);
    }
    Ok(models
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!()))
}

/// The priors of [`priors_to_bytes`], with counts up to [`adapt::LIMIT`].
pub fn priors_from_bytes(d: &[u8]) -> Result<Priors> {
    if d.len() != adapt::NUM * 3 {
        return Err(Error::Corrupt);
    }
    let (p1, n) = d.split_at(adapt::NUM * 2);
    if n.iter().any(|&n| n > adapt::LIMIT) {
        return Err(Error::Corrupt);
    }
    let p1 = p1
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    Ok(Priors { p1, n: n.to_vec() })
}

/// The module: the stream of `len` bytes rebuilt from `inputs`, the data and the record, with
/// `resources`, the catalog's rules and the default models or priors (by the record's
/// profile).
pub fn decode(
    params: &[u8],
    resources: &[&[u8]],
    inputs: &[&[u8]],
    len: u64,
    threads: usize,
) -> Result<Vec<u8>> {
    let (&[], &[rules, coding], &[data, record]) = (params, resources, inputs) else {
        return Err(Error::Corrupt);
    };
    let rules = rules_from_bytes(rules)?;
    let out = if crate::record_profile(record)? >= ADAPTIVE {
        let priors = priors_from_bytes(coding)?;
        let model = ModelData {
            rules: &rules,
            coding: Coding::Adaptive(&priors),
        };
        crate::reconstruct_with(data, record, model, threads)?
    } else {
        let models = models_from_bytes(coding)?;
        let model = ModelData {
            rules: &rules,
            coding: Coding::Static(&models),
        };
        crate::reconstruct_with(data, record, model, threads)?
    };
    if out.len() as u64 != len {
        return Err(Error::Corrupt);
    }
    Ok(out)
}
