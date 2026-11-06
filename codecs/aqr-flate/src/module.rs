//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module: the resources read, and the streams rebuilt.

use crate::adapt::{self, Priors};
use crate::code::{self, ALPHABET};
use crate::tokens::{FIELDS, Kind, Rule};
use crate::{ADAPTIVE, Coding, Error, ModelData, Result};

/// Bytes of a rule in a catalog resource.
pub(crate) const RULE_BYTES: usize = 18;

/// The rules of `rules_to_bytes`, which must be valid and none of them a chain rule.
pub fn rules_from_bytes(d: &[u8]) -> Result<Vec<Rule>> {
    if !d.len().is_multiple_of(RULE_BYTES) {
        return Err(Error::Corrupt);
    }
    d.as_chunks::<RULE_BYTES>()
        .0
        .iter()
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
    for q in d.as_chunks::<ALPHABET>().0 {
        models.push(code::Model::from_qs(q).ok_or(Error::Corrupt)?);
    }
    Ok(models
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!()))
}

/// The priors of `priors_to_bytes`, with counts up to [`adapt::LIMIT`].
pub fn priors_from_bytes(d: &[u8]) -> Result<Priors> {
    if d.len() != adapt::NUM * 3 {
        return Err(Error::Corrupt);
    }
    let (p1, n) = d.split_at(adapt::NUM * 2);
    if n.iter().any(|&n| n > adapt::LIMIT) {
        return Err(Error::Corrupt);
    }
    let p1 = p1
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| u16::from_le_bytes(b))
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
    /// The Adler-32 of the data (u32, big endian: a zlib stream's trailer), the length of the
    /// data (u64) and the length of the stream (u64): what a git pack's entries need.
    Zlib = 3,
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
            [3] => Ok(Derived::Zlib),
            _ => Err(Error::Corrupt),
        }
    }

    /// Bytes for each stream.
    pub fn bytes(self) -> usize {
        match self {
            Derived::None => 0,
            Derived::Zip | Derived::Zlib => 20,
            Derived::Gzip => 8,
        }
    }

    /// Appends what is derived of `data` and `stream` (rebuilt from it) to `out`.
    pub fn put(self, data: &[u8], stream: usize, out: &mut Vec<u8>) {
        let mut sum = self.sum();
        sum.update(data);
        self.put_sum(sum, data.len() as u64, stream as u64, out);
    }

    /// The checksum of the data it derives, given the data a piece at a time ([`Sum::update`]).
    pub fn sum(self) -> Sum {
        match self {
            Derived::None => Sum::None,
            Derived::Zip | Derived::Gzip => Sum::Crc(Default::default()),
            Derived::Zlib => Sum::Adler(Default::default()),
        }
    }

    /// [`Derived::put`] given the checksum of the data (of [`Derived::sum`]) and the lengths of
    /// the data and the stream.
    pub fn put_sum(self, sum: Sum, data: u64, stream: u64, out: &mut Vec<u8>) {
        let sum = match sum {
            Sum::None => 0,
            Sum::Crc(h) => h.finalize(),
            Sum::Adler(h) => h.finalize(),
        };
        match self {
            Derived::None => {}
            Derived::Zip | Derived::Zlib => {
                out.extend_from_slice(&match self {
                    Derived::Zip => sum.to_le_bytes(),
                    _ => sum.to_be_bytes(),
                });
                out.extend_from_slice(&data.to_le_bytes());
                out.extend_from_slice(&stream.to_le_bytes());
            }
            Derived::Gzip => {
                out.extend_from_slice(&sum.to_le_bytes());
                out.extend_from_slice(&(data as u32).to_le_bytes());
            }
        }
    }
}

/// The checksum that a [`Derived`] takes of the data.
pub enum Sum {
    None,
    Crc(aqr_common::crc32::Hasher),
    Adler(aqr_common::adler32::Hasher),
}

impl Sum {
    pub fn update(&mut self, d: &[u8]) {
        match self {
            Sum::None => {}
            Sum::Crc(h) => h.update(d),
            Sum::Adler(h) => h.update(d),
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

/// [`crate::reconstruct_sized`], called once in the code (the module's tasks and the module
/// itself run the same jobs).
#[inline(never)]
fn rebuild(
    data: &[u8],
    record: &[u8],
    model: ModelData,
    threads: usize,
    cap: usize,
) -> Result<Vec<u8>> {
    crate::reconstruct_sized(data, record, model, threads, cap)
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
    // (a stream alone is the output, of a known length: but no more than its data and record
    // could make, a token of 6 bytes at most taking one of data at least, for a corrupt length;
    // others are reserved half their data's)
    let cap = |i: usize| match items.len() {
        1 => len.min(8 * (sizes[0] + items[0].1.len()) as u64 + 64) as usize,
        _ => sizes[i] / 2 + 64,
    };
    // (which failure it was is not kept: less code in the module's tasks)
    let mut streams = par_jobs(&sizes, threads, |i, per| {
        rebuild(items[i].0, items[i].1, model, per, cap(i)).ok()
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

/// Pages of the windows of `flate-set` as a part module ([`decode_part`]): of the data and the
/// output, and of the records and the index.
pub const SET_WINDOW: u32 = 16;
pub const SET_RECORDS_WINDOW: u32 = 4;

/// Threads of `flate-set` in WebAssembly (its task slots); natively, the pool's. The output is the
/// same whatever their number.
pub const SET_THREADS: usize = 8;

fn set_threads() -> usize {
    match cfg!(target_family = "wasm") {
        true => SET_THREADS,
        false => aqr_common::pool::size().max(1),
    }
}

/// Bytes of the data of small streams (of less than [`LARGE`] each) that [`decode_part`] takes
/// together, their data and records held: those of a run decoded side by side.
const RUN: usize = 4 * LARGE;

/// The bytes of the whole memory `M` of `len` bytes (the parameters or a resource).
fn whole_of<M: aqr_api::Memory>(len: u64) -> Result<Vec<u8>> {
    let mut v = vec![0; usize::try_from(len).map_err(|_| Error::Limit)?];
    M::read(0, &mut v);
    Ok(v)
}

/// The `K`-th input of a part module read in order, through its window.
struct Reader<const K: u32> {
    at: u64,
    len: u64,
}

impl<const K: u32> Reader<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn new(len: u64) -> Reader<K> {
        Reader { at: 0, len }
    }

    /// Appends its next `n` bytes to `v`.
    fn take(&mut self, v: &mut Vec<u8>, n: usize) -> Result<()> {
        use aqr_api::{Input, Memory};
        let end = self
            .at
            .checked_add(n as u64)
            .filter(|&e| e <= self.len)
            .ok_or(Error::Corrupt)?;
        let w = Input::<K>::size();
        while self.at < end {
            // (to the end of the window at most)
            let k = (end - self.at).min(w - self.at % w);
            let start = v.len();
            v.resize(start + k as usize, 0);
            Input::<K>::request(self.at + k);
            Input::<K>::read(self.at % w, &mut v[start..]);
            Input::<K>::release(self.at + k);
            self.at += k;
        }
        Ok(())
    }
}

/// The output of a part module, written in order through its window: `size` bytes in all.
struct Written {
    at: u64,
    size: u64,
}

impl Written {
    fn write(&mut self, mut d: &[u8]) -> Result<()> {
        use aqr_api::{Memory, Output};
        if d.len() as u64 > self.size - self.at {
            return Err(Error::Corrupt);
        }
        let w = Output::<0>::size();
        while !d.is_empty() {
            let k = (d.len() as u64).min(w - self.at % w) as usize;
            Output::<0>::write(self.at % w, &d[..k]);
            self.at += k as u64;
            Output::<0>::commit(self.at);
            d = &d[k..];
        }
        Ok(())
    }
}

/// The model data of a profile, read from the resources as the first record tells its profile.
struct Model {
    profile: u16,
    rules: Vec<crate::tokens::Rule>,
    models: Option<Box<[code::Model; FIELDS]>>,
    priors: Option<Priors>,
}

impl Model {
    fn read(profile: u16, rules: &[u8], coding: &[u8]) -> Result<Model> {
        let adaptive = profile >= ADAPTIVE;
        Ok(Model {
            profile,
            rules: rules_from_bytes(rules)?,
            models: if adaptive {
                None
            } else {
                Some(models_from_bytes(coding)?)
            },
            priors: if adaptive {
                Some(priors_from_bytes(coding)?)
            } else {
                None
            },
        })
    }

    fn data(&self) -> ModelData<'_> {
        ModelData {
            rules: &self.rules,
            coding: match (&self.models, &self.priors) {
                (Some(m), _) => Coding::Static(m),
                (_, Some(p)) => Coding::Adaptive(p),
                _ => unreachable!("models or priors"),
            },
        }
    }
}

/// The module `flate-set` as a part module (its entry point, of `aqr_api::entry!`): what
/// [`decode`] gives, its inputs read and its output written through windows ([`SET_WINDOW`]).
/// A stream of [`LARGE`] bytes of data or more is rebuilt from its data pulled a batch of
/// segments at a time ([`crate::reconstruct_pulled`]), its record held; the others in runs of
/// [`RUN`] bytes of data at most, side by side as [`decode`] does, their data and records held.
/// What is derived of the streams is held until they are all written.
pub fn decode_part(
    params_len: u64,
    rules_len: u64,
    coding_len: u64,
    data_len: u64,
    records_len: u64,
    index_len: u64,
    size: u64,
) {
    aqr_api::finish(set_part(
        params_len,
        [rules_len, coding_len],
        [data_len, records_len, index_len],
        size,
    ));
}

fn set_part(params_len: u64, resources: [u64; 2], inputs: [u64; 3], size: u64) -> Result<()> {
    use aqr_api::{Params, Resource};
    let derived = Derived::from_params(&whole_of::<Params>(params_len)?)?;
    let (rules, coding) = (
        whole_of::<Resource<0>>(resources[0])?,
        whole_of::<Resource<1>>(resources[1])?,
    );
    let (mut data_in, mut records_in) = (Reader::<0>::new(inputs[0]), Reader::<1>::new(inputs[1]));
    let mut index = Vec::new();
    Reader::<2>::new(inputs[2]).take(
        &mut index,
        usize::try_from(inputs[2]).map_err(|_| Error::Limit)?,
    )?;
    // the lengths of each stream's data and record
    let mut lens = Vec::new();
    if index.is_empty() {
        lens.push((inputs[0], inputs[1]));
    } else {
        let mut at = 0;
        while at < index.len() {
            let mut get = || aqr_common::varint::get(&index, &mut at).map_err(|_| Error::Corrupt);
            lens.push((get()?, get()?));
        }
    }
    let sum = |k: fn(&(u64, u64)) -> u64| lens.iter().try_fold(0u64, |s, l| s.checked_add(k(l)));
    if sum(|l| l.0) != Some(inputs[0]) || sum(|l| l.1) != Some(inputs[1]) {
        return Err(Error::Corrupt);
    }
    let size_of = |n: u64| usize::try_from(n).map_err(|_| Error::Limit);
    let mut out = Written { at: 0, size };
    let mut derive = Vec::with_capacity(lens.len() * derived.bytes());
    let mut model: Option<Model> = None;
    // (the model of the first record's profile, which every record must be of)
    let model_of = |model: &mut Option<Model>, record: &[u8]| -> Result<()> {
        let profile = crate::record_profile(record)?;
        match model {
            Some(m) if m.profile != profile => Err(Error::Corrupt),
            Some(_) => Ok(()),
            None => {
                *model = Some(Model::read(profile, &rules, &coding)?);
                Ok(())
            }
        }
    };
    let mut i = 0;
    while i < lens.len() {
        let (d, r) = (size_of(lens[i].0)?, size_of(lens[i].1)?);
        if d >= LARGE {
            let mut record = Vec::with_capacity(r);
            records_in.take(&mut record, r)?;
            model_of(&mut model, &record)?;
            let mut sum = derived.sum();
            let mut read = |v: &mut Vec<u8>, k: usize| {
                let start = v.len();
                data_in.take(v, k)?;
                sum.update(&v[start..]);
                Ok(())
            };
            let mut pull = crate::Pull::new(&mut read);
            let (mut stream, mut failed) = (0u64, None);
            let m = model.as_ref().expect("a model");
            crate::reconstruct_pulled(d, &mut pull, &record, m.data(), set_threads(), &mut |b| {
                stream += b.len() as u64;
                if let Err(e) = out.write(b) {
                    failed.get_or_insert(e);
                }
            })?;
            drop(pull);
            if let Some(e) = failed {
                return Err(e);
            }
            derived.put_sum(sum, d as u64, stream, &mut derive);
            i += 1;
            continue;
        }
        // a run of small streams, their data and records cut as `decode` cuts them
        let mut j = i;
        let mut held = 0;
        while j < lens.len()
            && lens[j].0 < LARGE as u64
            && (j == i || held + lens[j].0 as usize <= RUN)
        {
            held += lens[j].0 as usize;
            j += 1;
        }
        let records_held = lens[i..j].iter().map(|l| l.1 as usize).sum();
        let (mut data, mut records) = (Vec::with_capacity(held), Vec::with_capacity(records_held));
        let mut cut = Vec::with_capacity(j - i);
        for &(d, r) in &lens[i..j] {
            let (d, r) = (size_of(d)?, size_of(r)?);
            let (d0, r0) = (data.len(), records.len());
            data_in.take(&mut data, d)?;
            records_in.take(&mut records, r)?;
            model_of(&mut model, &records[r0..])?;
            cut.push((d0..d0 + d, r0..r0 + r));
        }
        let m = model.as_ref().expect("a model").data();
        let sizes: Vec<usize> = cut.iter().map(|c| c.0.len()).collect();
        // (a stream alone, of the output's length at most, as `decode` reserves it)
        let alone = lens.len() == 1;
        let streams = par_jobs(&sizes, set_threads(), |k, per| {
            let (d, r) = (&data[cut[k].0.clone()], &records[cut[k].1.clone()]);
            let cap = match alone {
                true => size.min(8 * (d.len() + r.len()) as u64 + 64) as usize,
                false => d.len() / 2 + 64,
            };
            rebuild(d, r, m, per, cap).ok()
        });
        for (k, s) in streams.into_iter().enumerate() {
            let s = s.ok_or(Error::Corrupt)?;
            out.write(&s)?;
            derived.put(&data[cut[k].0.clone()], s.len(), &mut derive);
        }
        i = j;
    }
    out.write(&derive)?;
    match out.at == size {
        true => Ok(()),
        false => Err(Error::Corrupt),
    }
}

/// The module `flate-inflate`: the data of the raw DEFLATE stream that is its one input, of
/// `len` bytes (FORMAT.md, "The module flate-inflate"). No parameters.
pub fn inflate(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[], &[raw]) = (params, inputs) else {
        return Err(Error::Corrupt);
    };
    crate::inflate::data(raw, usize::try_from(len).map_err(|_| Error::Limit)?)
}

/// The pages of the windows of `flate-inflate`'s input and output (as a part module,
/// [`inflate_part`]).
pub const INFLATE_WINDOW: u32 = 1;

/// The module `flate-inflate` as a part module (its entry point, of `aqr_api::entry!`): the
/// stream read and its data written through windows of [`INFLATE_WINDOW`] pages
/// ([`crate::inflate::data_streamed`]), whatever their lengths.
pub fn inflate_part(params_len: u64, in0_len: u64, size: u64) {
    use aqr_api::{Input, Memory, Output};
    if params_len != 0 {
        aqr_api::finish(Err(Error::Corrupt));
    }
    let (w_in, w_out) = (Input::<0>::size(), Output::<0>::size());
    let (mut at_in, mut at_out) = (0u64, 0u64);
    let r = crate::inflate::data_streamed(
        in0_len,
        &mut |v: &mut Vec<u8>, n: usize| {
            // (to the end of the window at most)
            let k = (n as u64).min(in0_len - at_in).min(w_in - at_in % w_in) as usize;
            let start = v.len();
            v.resize(start + k, 0);
            Input::<0>::request(at_in + k as u64);
            Input::<0>::read(at_in % w_in, &mut v[start..]);
            Input::<0>::release(at_in + k as u64);
            at_in += k as u64;
            Ok(())
        },
        &mut |mut d: &[u8]| {
            while !d.is_empty() {
                let k = (d.len() as u64).min(w_out - at_out % w_out) as usize;
                Output::<0>::write(at_out % w_out, &d[..k]);
                at_out += k as u64;
                Output::<0>::commit(at_out);
                d = &d[k..];
            }
            Ok(())
        },
        size,
    );
    aqr_api::finish(r);
}
