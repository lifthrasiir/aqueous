//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module: the resources read, and the streams rebuilt.

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

/// What the module outputs after the streams, for each of them in order (its params).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derived {
    /// Nothing (no params).
    None = 0,
    /// The CRC-32 (u32) and the length (u64) of the data, and the length of the stream (u64):
    /// what a ZIP archive's headers say of an entry.
    Zip = 1,
    /// The CRC-32 (u32) and the length mod 2^32 (u32) of the data: a gzip member's trailer.
    Gzip = 2,
}

impl Derived {
    /// The params of the module.
    pub fn params(self) -> Vec<u8> {
        match self {
            Derived::None => Vec::new(),
            d => vec![d as u8],
        }
    }

    pub fn from_params(p: &[u8]) -> Result<Derived> {
        match p {
            [] => Ok(Derived::None),
            [1] => Ok(Derived::Zip),
            [2] => Ok(Derived::Gzip),
            _ => Err(Error::Corrupt),
        }
    }

    /// Bytes for each stream.
    pub fn bytes(self) -> usize {
        match self {
            Derived::None => 0,
            Derived::Zip => 20,
            Derived::Gzip => 8,
        }
    }

    /// Appends what is derived of `data` and `stream` (rebuilt from it) to `out`.
    pub fn put(self, data: &[u8], stream: usize, out: &mut Vec<u8>) {
        if self == Derived::None {
            return;
        }
        out.extend_from_slice(&aqr_common::crc32::hash(data).to_le_bytes());
        match self {
            Derived::Zip => {
                out.extend_from_slice(&(data.len() as u64).to_le_bytes());
                out.extend_from_slice(&(stream as u64).to_le_bytes());
            }
            _ => out.extend_from_slice(&(data.len() as u32).to_le_bytes()),
        }
    }
}

/// The index of streams of data and records of `lens` bytes: for each, the two lengths as
/// varints; empty for a single stream.
pub fn index(lens: &[(usize, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    if lens.len() > 1 {
        for &(d, r) in lens {
            aqr_common::varint::put(&mut out, d as u64);
            aqr_common::varint::put(&mut out, r as u64);
        }
    }
    out
}

/// The data and records of each stream that `index` cuts `data` and `records` into.
fn cut<'a>(data: &'a [u8], records: &'a [u8], index: &[u8]) -> Result<Vec<(&'a [u8], &'a [u8])>> {
    if index.is_empty() {
        return Ok(vec![(data, records)]);
    }
    let (mut at, mut d, mut r) = (0, data, records);
    let mut out = Vec::new();
    while at < index.len() {
        let mut take = |from: &mut &'a [u8]| {
            let n = aqr_common::varint::get(index, &mut at).map_err(|_| Error::Corrupt)?;
            let n = usize::try_from(n)
                .ok()
                .filter(|&n| n <= from.len())
                .ok_or(Error::Corrupt)?;
            let (a, b) = from.split_at(n);
            *from = b;
            Ok::<_, Error>(a)
        };
        out.push((take(&mut d)?, take(&mut r)?));
    }
    if !d.is_empty() || !r.is_empty() {
        return Err(Error::Corrupt);
    }
    Ok(out)
}

/// Bytes of data from which a stream gets threads of its own: two segments.
const LARGE: usize = 2 << 20;

/// `f(item, threads)` on `0..sizes.len()`, by `threads` threads, the largest by `sizes` first
/// (on WebAssembly, those of at least [`LARGE`] first): those of at least [`LARGE`] with all of them (segments are rebuilt in parallel, but a stream is
/// parsed on one), then the others with one each, all side by side, so that the small fill the
/// threads a large one leaves idle. On WebAssembly they run as tasks
/// (`aqr_common::pool::par_map_apart`): those of at least [`LARGE`] a task each, the others
/// together, a task for each [`LARGE`] of them or so (a task costs an instance).
fn par_jobs<R: Send + aqr_common::wire::Wire>(
    sizes: &[usize],
    threads: usize,
    f: impl Fn(usize, usize) -> R + Sync,
) -> Vec<R> {
    // (the largest first natively; on WebAssembly no sort, whose code is large in the module)
    let large = |i: &usize| sizes[*i] >= LARGE;
    let mut order: Vec<usize> = (0..sizes.len())
        .filter(large)
        .chain((0..sizes.len()).filter(|i| !large(i)))
        .collect();
    if !cfg!(target_family = "wasm") {
        order.sort_by_key(|&i| std::cmp::Reverse(sizes[i]));
    }
    let per = |i: usize| if sizes[i] >= LARGE { threads } else { 1 };
    let mut groups = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let (start, mut sum) = (k, 0);
        // (one each, but on WebAssembly)
        while k < order.len()
            && (k == start || (cfg!(target_family = "wasm") && sum + sizes[order[k]] <= LARGE))
        {
            sum += sizes[order[k]];
            k += 1;
        }
        groups.push(start..k);
    }
    let done = aqr_common::pool::par_map_apart(groups.len(), threads, |g| {
        order[groups[g].clone()]
            .iter()
            .map(|&i| f(i, per(i)))
            .collect::<Vec<R>>()
    });
    let mut out: Vec<Option<R>> = (0..sizes.len()).map(|_| None).collect();
    for (&i, r) in order.iter().zip(done.into_iter().flatten()) {
        out[i] = Some(r);
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// [`crate::reconstruct_with`], called once in the code (the module's tasks and the module
/// itself run the same jobs).
#[inline(never)]
fn rebuild(data: &[u8], record: &[u8], model: ModelData, threads: usize) -> Result<Vec<u8>> {
    crate::reconstruct_with(data, record, model, threads)
}

/// The module: the streams rebuilt from `inputs`, their data and their records (all of one
/// profile) one after another and the index cutting them ([`index`]), one after another in
/// `len` bytes, then what `params` derives of them ([`Derived`]); with `resources`, the
/// catalog's rules and the default models or priors (by the records' profile).
pub fn decode(
    params: &[u8],
    resources: &[&[u8]],
    inputs: &[&[u8]],
    len: u64,
    threads: usize,
) -> Result<Vec<u8>> {
    let derived = Derived::from_params(params)?;
    let (&[rules, coding], &[data, records, index]) = (resources, inputs) else {
        return Err(Error::Corrupt);
    };
    let items = cut(data, records, index)?;
    let profile = crate::record_profile(items[0].1)?;
    for (_, r) in &items {
        if crate::record_profile(r)? != profile {
            return Err(Error::Corrupt);
        }
    }
    let rules = rules_from_bytes(rules)?;
    let (models, priors);
    let model = if profile >= ADAPTIVE {
        priors = priors_from_bytes(coding)?;
        ModelData {
            rules: &rules,
            coding: Coding::Adaptive(&priors),
        }
    } else {
        models = models_from_bytes(coding)?;
        ModelData {
            rules: &rules,
            coding: Coding::Static(&models),
        }
    };
    let sizes: Vec<usize> = items.iter().map(|i| i.0.len()).collect();
    // (which failure it was is not kept: less code in the module's tasks)
    let mut streams = par_jobs(&sizes, threads, |i, per| {
        rebuild(items[i].0, items[i].1, model, per).ok()
    })
    .into_iter()
    .collect::<Option<Vec<Vec<u8>>>>()
    .ok_or(Error::Corrupt)?;
    let total = streams.iter().map(|s| s.len()).sum::<usize>() + items.len() * derived.bytes();
    if total as u64 != len {
        return Err(Error::Corrupt);
    }
    let lens: Vec<usize> = streams.iter().map(|s| s.len()).collect();
    // (a stream alone as it is)
    let mut out = match streams.len() {
        1 => streams.pop().unwrap(),
        _ => Vec::with_capacity(total),
    };
    out.reserve_exact(total - out.len());
    streams.iter().for_each(|s| out.extend_from_slice(s));
    drop(streams);
    for ((d, _), n) in items.iter().zip(lens) {
        derived.put(d, n, &mut out);
    }
    Ok(out)
}
