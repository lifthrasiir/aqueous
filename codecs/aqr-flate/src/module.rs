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
    // (exactly as many)
    let mut rules = Vec::with_capacity(d.len() / RULE_BYTES);
    for r in d.as_chunks::<RULE_BYTES>().0.iter().map(|b| {
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
    }) {
        rules.push(r?);
    }
    Ok(rules)
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
    /// What [`Derived::Zip`] derives, but before each stream rather than after them all: so that
    /// whoever takes the output in order (a ZIP archive's local headers) has it first.
    ZipBefore = 4,
}

impl Derived {
    /// Whether it comes before each stream ([`Derived::ZipBefore`]), else after them all.
    pub fn before(self) -> bool {
        self == Derived::ZipBefore
    }

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
            [4] => Ok(Derived::ZipBefore),
            _ => Err(Error::Corrupt),
        }
    }

    /// The bytes of the streams of an output of `len` bytes, of the streams that `index` gives
    /// (where it is empty, one).
    pub fn streams_len(self, len: u64, index: &[u8]) -> Result<u64> {
        let (mut at, mut k) = (0, 0u64);
        while at < index.len() {
            self.entry(index, &mut at)?;
            k += 1;
        }
        len.checked_sub(k.max(1) * self.bytes() as u64)
            .ok_or(Error::Corrupt)
    }

    /// The next entry of `index` at `at`: the lengths of a stream's data, record and stream, and
    /// its data's CRC-32 where `self` has it given (0 elsewhere): [`Derived::ZipBefore`] for a
    /// stream of [`LARGE`] bytes of data or more, as a fourth varint.
    fn entry(self, index: &[u8], at: &mut usize) -> Result<(u64, u64, u64, u32)> {
        let get = |at: &mut usize| aqr_common::varint::get(index, at).map_err(|_| Error::Corrupt);
        let (d, r, s) = (get(at)?, get(at)?, get(at)?);
        let crc = match self.before() && d >= LARGE as u64 {
            true => u32::try_from(get(at)?).map_err(|_| Error::Corrupt)?,
            false => 0,
        };
        Ok((d, r, s, crc))
    }

    /// Bytes for each stream.
    pub fn bytes(self) -> usize {
        match self {
            Derived::None => 0,
            Derived::Zip | Derived::Zlib | Derived::ZipBefore => 20,
            Derived::Gzip => 8,
        }
    }

    /// Appends what is derived of `data` and `stream` (rebuilt from it) to `out`.
    pub fn put(self, data: &[u8], stream: usize, out: &mut Vec<u8>) {
        let mut sum = self.sum();
        sum.update(data);
        self.put_sum(sum, data.len() as u64, stream as u64, out);
    }

    /// Whether the checksum it takes is a CRC-32 (whose tables WebAssembly makes as they are
    /// first used: `aqr_common::crc32::WASM_TABLES`).
    pub fn crc(self) -> bool {
        matches!(self.sum(), Sum::Crc(_))
    }

    /// The checksum of the data it derives, given the data a piece at a time ([`Sum::update`]).
    pub fn sum(self) -> Sum {
        match self {
            Derived::None => Sum::None,
            Derived::Zip | Derived::Gzip | Derived::ZipBefore => Sum::Crc(Default::default()),
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
            Derived::Zip | Derived::Zlib | Derived::ZipBefore => {
                out.extend_from_slice(&match self {
                    Derived::Zip | Derived::ZipBefore => sum.to_le_bytes(),
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

/// The index of streams of data, records and streams of `lens` bytes: for each, the three
/// lengths as varints; empty for a single stream.
pub fn index(lens: &[(usize, usize, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    if lens.len() > 1 {
        for &(d, r, s) in lens {
            [d, r, s]
                .iter()
                .for_each(|&n| aqr_common::varint::put(&mut out, n as u64));
        }
    }
    out
}

/// The index of streams for [`Derived::ZipBefore`]: [`index`] with the CRC-32 of the data of each
/// stream of [`LARGE`] bytes of data or more after its lengths (`lens` has them as the fourth),
/// so that the module can give what is derived before the stream without holding it. Never
/// empty (for a single stream too) where there is such a stream.
pub fn index_before(lens: &[(usize, usize, usize, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    if lens.len() > 1 || lens.iter().any(|l| l.0 >= LARGE) {
        for &(d, r, s, crc) in lens {
            [d, r, s]
                .iter()
                .for_each(|&n| aqr_common::varint::put(&mut out, n as u64));
            if d >= LARGE {
                aqr_common::varint::put(&mut out, crc as u64);
            }
        }
    }
    out
}

/// The lengths of each stream's data, record and stream that `index` gives (of `data` and
/// `records` bytes in all, and `streams` of the streams, a single stream's where it is empty),
/// and the CRC-32 of each one's data as far as `derived` has it given (else empty).
pub fn lens_of(
    derived: Derived,
    index: &[u8],
    data: u64,
    records: u64,
    streams: u64,
) -> Result<(Vec<(u64, u64, u64)>, Vec<u32>)> {
    if index.is_empty() {
        // (a single large stream's CRC-32 would be in the index)
        return match derived.before() {
            true if data >= LARGE as u64 => Err(Error::Corrupt),
            true => Ok((vec![(data, records, streams)], vec![0])),
            false => Ok((vec![(data, records, streams)], Vec::new())),
        };
    }
    // (counted first, for the room)
    let (mut at, mut k) = (0, 0);
    while at < index.len() {
        derived.entry(index, &mut at)?;
        k += 1;
    }
    let mut lens = Vec::with_capacity(k);
    let mut crcs = Vec::with_capacity(if derived.before() { k } else { 0 });
    at = 0;
    while at < index.len() {
        let (d, r, s, crc) = derived.entry(index, &mut at)?;
        lens.push((d, r, s));
        if derived.before() {
            crcs.push(crc);
        }
    }
    let sum =
        |f: fn(&(u64, u64, u64)) -> u64| lens.iter().try_fold(0u64, |s, l| s.checked_add(f(l)));
    match (sum(|l| l.0), sum(|l| l.1), sum(|l| l.2)) == (Some(data), Some(records), Some(streams)) {
        true => Ok((lens, crcs)),
        false => Err(Error::Corrupt),
    }
}

/// A stream's data, record and length.
type Item<'a> = (&'a [u8], &'a [u8], usize);

/// The data, record and length of each stream that `index` cuts `data` and `records` into (of
/// `streams` bytes in all), and the CRC-32s that `derived` is given ([`lens_of`]).
fn cut<'a>(
    derived: Derived,
    data: &'a [u8],
    records: &'a [u8],
    index: &[u8],
    streams: u64,
) -> Result<(Vec<Item<'a>>, Vec<u32>)> {
    let (lens, crcs) = lens_of(
        derived,
        index,
        data.len() as u64,
        records.len() as u64,
        streams,
    )?;
    let (mut d, mut r) = (data, records);
    let mut out = Vec::with_capacity(lens.len());
    for (dn, rn, sn) in lens {
        let (a, b) = d.split_at(dn as usize);
        let (c, e) = r.split_at(rn as usize);
        (d, r) = (b, e);
        out.push((a, c, usize::try_from(sn).map_err(|_| Error::Limit)?));
    }
    Ok((out, crcs))
}

/// Bytes of data from which a stream gets threads of its own: two segments.
pub(crate) const LARGE: usize = 2 << 20;

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
    let streams_len = derived.streams_len(len, index)?;
    let (items, crcs) = cut(derived, data, records, index, streams_len)?;
    let profile = crate::record_profile(items[0].1)?;
    for (_, r, _) in &items {
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
    // (each stream of the length the index gives, but no more than its data and record could
    // make, a token of 6 bytes at most taking one of data at least, for a corrupt length)
    let cap = |i: usize| items[i].2.min(8 * (sizes[i] + items[i].1.len()) + 64);
    // (which failure it was is not kept: less code in the module's tasks)
    let mut streams = par_jobs(&sizes, threads, |i, per| {
        rebuild(items[i].0, items[i].1, model, per, cap(i))
            .ok()
            .filter(|s| s.len() == items[i].2)
    })
    .into_iter()
    .collect::<Option<Vec<Vec<u8>>>>()
    .ok_or(Error::Corrupt)?;
    let total = streams.iter().map(|s| s.len()).sum::<usize>() + items.len() * derived.bytes();
    if total as u64 != len {
        return Err(Error::Corrupt);
    }
    let lens: Vec<usize> = streams.iter().map(|s| s.len()).collect();
    if derived.before() {
        let mut out = Vec::with_capacity(total);
        for ((((d, _, _), n), s), crc) in items.iter().zip(lens).zip(&streams).zip(crcs) {
            let at = out.len();
            derived.put(d, n, &mut out);
            // (the CRC-32 given for a large stream is that of its data)
            if d.len() >= LARGE && out[at..at + 4] != crc.to_le_bytes() {
                return Err(Error::Corrupt);
            }
            out.extend_from_slice(s);
        }
        return Ok(out);
    }
    // (a stream alone as it is)
    let mut out = match streams.len() {
        1 => streams.pop().unwrap(),
        _ => Vec::with_capacity(total),
    };
    out.reserve_exact(total - out.len());
    streams.iter().for_each(|s| out.extend_from_slice(s));
    drop(streams);
    for ((d, _, _), n) in items.iter().zip(lens) {
        derived.put(d, n, &mut out);
    }
    Ok(out)
}

/// Pages of the windows of `flate-set` as a part module ([`decode_part`]): of the data and the
/// output, and of the records and the index.
pub const SET_WINDOW: u32 = 16;
pub const SET_RECORDS_WINDOW: u32 = 4;

/// Threads of `flate-set` in WebAssembly (its task slots); natively, the pool's. The output is
/// the same whatever their number.
pub const SET_THREADS: usize = 8;

fn set_threads() -> usize {
    match cfg!(target_family = "wasm") {
        true => SET_THREADS,
        false => aqr_common::pool::size().max(1),
    }
}

/// Bytes of the data of small streams (of less than [`LARGE`] each) that [`decode_part`] takes
/// together, their data and records held: those of a run decoded side by side.
pub(crate) const RUN: usize = 4 * LARGE;

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

    /// The profile of the record at its start (its first two bytes), read without taking them.
    fn profile(&mut self) -> Result<u16> {
        use aqr_api::{Input, Memory};
        if self.len - self.at < 2 || Input::<K>::size() - self.at % Input::<K>::size() < 2 {
            return Err(Error::Corrupt);
        }
        let mut b = [0; 2];
        Input::<K>::request(self.at + 2);
        Input::<K>::read(self.at % Input::<K>::size(), &mut b);
        crate::record_profile(&b)
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
    let mut index = Vec::with_capacity(usize::try_from(inputs[2]).map_err(|_| Error::Limit)?);
    Reader::<2>::new(inputs[2]).take(&mut index, inputs[2] as usize)?;
    // each stream's data, record and stream
    let (lens, crcs) = lens_of(
        derived,
        &index,
        inputs[0],
        inputs[1],
        derived.streams_len(size, &index)?,
    )?;
    let size_of = |n: u64| usize::try_from(n).map_err(|_| Error::Limit);
    let mut out = Written { at: 0, size };
    let mut derive = Vec::with_capacity(lens.len() * derived.bytes());
    // (a CRC's tables made now, which in WebAssembly take memory as they are first used)
    derived.sum().update(&[]);
    // (the model of the first record's profile, which every record must be of)
    let model = Model::read(records_in.profile()?, &rules, &coding)?;
    let of_model = |record: &[u8]| match crate::record_profile(record)? == model.profile {
        true => Ok(()),
        false => Err(Error::Corrupt),
    };
    let threads = set_threads();
    let mut i = 0;
    while i < lens.len() {
        let (d, r, sn) = (
            size_of(lens[i].0)?,
            size_of(lens[i].1)?,
            size_of(lens[i].2)?,
        );
        if d >= LARGE {
            let mut record = Vec::with_capacity(r);
            records_in.take(&mut record, r)?;
            of_model(&record)?;
            let mut sum = derived.sum();
            let mut read = |v: &mut Vec<u8>, k: usize| {
                let start = v.len();
                data_in.take(v, k)?;
                sum.update(&v[start..]);
                Ok(())
            };
            let mut pull = crate::Pull::new(&mut read);
            let (mut stream, mut failed) = (0u64, None);
            // (what is derived comes first, with the CRC-32 the index gives: the stream is not
            // held, and the CRC-32 of the data is checked against it at the end)
            if derived.before() {
                let mut head = [0u8; 20];
                head[..4].copy_from_slice(&crcs[i].to_le_bytes());
                head[4..12].copy_from_slice(&(d as u64).to_le_bytes());
                head[12..].copy_from_slice(&(sn as u64).to_le_bytes());
                out.write(&head)?;
            }
            crate::reconstruct_pulled(d, &mut pull, &record, model.data(), threads, &mut |b| {
                stream += b.len() as u64;
                let r = match stream <= sn as u64 {
                    false => Err(Error::Corrupt),
                    true => out.write(b),
                };
                if let Err(e) = r {
                    failed.get_or_insert(e);
                }
            })?;
            drop(pull);
            if let Some(e) = failed {
                return Err(e);
            }
            if stream != sn as u64 {
                return Err(Error::Corrupt);
            }
            if derived.before() {
                derive.clear();
                derived.put_sum(sum, d as u64, stream, &mut derive);
                if derive[..4] != crcs[i].to_le_bytes() {
                    return Err(Error::Corrupt);
                }
                derive.clear();
            } else {
                derived.put_sum(sum, d as u64, stream, &mut derive);
            }
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
        for &(d, r, _) in &lens[i..j] {
            let r0 = records.len();
            data_in.take(&mut data, size_of(d)?)?;
            records_in.take(&mut records, size_of(r)?)?;
            of_model(&records[r0..])?;
        }
        let run = Run::of(&lens[i..j]);
        let m = model.data();
        // each group of streams a task, giving their streams one after another (a byte first, 0
        // where they were all rebuilt), written in turn
        let task = |g: usize| run.group(g, &data, &records, m);
        aqr_common::pool::par_each_apart(
            run.groups(),
            threads,
            1,
            task,
            || {},
            |g, b| {
                let (&[0], mut b) = b.split_at_checked(1).ok_or(Error::Corrupt)? else {
                    return Err(Error::Corrupt);
                };
                for k in run.streams(g) {
                    let (s, rest) = b.split_at_checked(run.lens[k].2).ok_or(Error::Corrupt)?;
                    derived.put(&data[run.data(k)], s.len(), &mut derive);
                    if derived.before() {
                        out.write(&derive)?;
                        derive.clear();
                    }
                    out.write(s)?;
                    b = rest;
                }
                match b.is_empty() {
                    true => Ok(()),
                    false => Err(Error::Corrupt),
                }
            },
        )?;
        i = j;
    }
    out.write(&derive)?;
    match out.at == size {
        true => Ok(()),
        false => Err(Error::Corrupt),
    }
}

/// Where the groups of streams of data of `sizes` bytes start (and the end): as many together
/// as have [`LARGE`] bytes at most, one at least.
pub(crate) fn group_starts(sizes: impl Iterator<Item = usize> + Clone) -> Vec<usize> {
    // where a group starts after the first: once the one before has as much as fits
    let cuts = || {
        let mut sum = 0;
        sizes.clone().enumerate().filter_map(move |(i, n)| {
            let cut = i > 0 && sum + n > LARGE;
            sum = if cut { n } else { sum + n };
            cut.then_some(i)
        })
    };
    let mut starts = Vec::with_capacity(cuts().count() + 2);
    starts.push(0);
    starts.extend(cuts());
    starts.push(sizes.count());
    starts
}

/// A run of small streams of [`decode_part`], decoded a group at a time (of [`LARGE`] bytes of
/// data at most, or one stream), their data and records held one after another.
struct Run {
    /// Each stream's data, record and stream, as `usize`, and where its data and record start.
    lens: Vec<(usize, usize, usize, usize, usize)>,
    /// Where each group starts (and the run's end).
    starts: Vec<usize>,
}

impl Run {
    /// Of the streams of `lens` (data, record, stream), of less than [`LARGE`] bytes of data each.
    fn of(lens: &[(u64, u64, u64)]) -> Run {
        let (mut d0, mut r0) = (0, 0);
        let mut v = Vec::with_capacity(lens.len());
        for &(d, r, s) in lens {
            v.push((d as usize, r as usize, s as usize, d0, r0));
            (d0, r0) = (d0 + d as usize, r0 + r as usize);
        }
        let starts = Run::starts_of(lens.iter().map(|l| l.0 as usize));
        Run { lens: v, starts }
    }

    fn starts_of(sizes: impl Iterator<Item = usize> + Clone) -> Vec<usize> {
        group_starts(sizes)
    }

    fn groups(&self) -> usize {
        self.starts.len() - 1
    }

    fn streams(&self, g: usize) -> std::ops::Range<usize> {
        self.starts[g]..self.starts[g + 1]
    }

    fn data(&self, k: usize) -> std::ops::Range<usize> {
        self.lens[k].3..self.lens[k].3 + self.lens[k].0
    }

    /// Bytes of a group's result: its streams' and a byte, and what writing reserves past them.
    fn result_room(&self, g: usize) -> usize {
        1 + self.streams(g).map(|k| self.lens[k].2).sum::<usize>() + crate::write::TOKEN_SLACK
    }

    /// The result of group `g` (see [`set_part`]) of the run's `data` and `records`.
    fn group(&self, g: usize, data: &[u8], records: &[u8], m: ModelData) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.result_room(g));
        out.push(0);
        for k in self.streams(g) {
            let (d, r, s, d0, r0) = self.lens[k];
            let start = out.len();
            out = match crate::reconstruct_onto(&data[d0..d0 + d], &records[r0..r0 + r], m, 1, out)
            {
                Ok(o) if o.len() - start == s => o,
                _ => return vec![1],
            };
        }
        out
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
