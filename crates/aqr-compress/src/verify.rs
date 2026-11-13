//! Verification of the archive that [`compress`](crate::compress) makes (docs/xform-design.md §6):
//! it must give the file back. The applications of modules are evaluated beforehand, on threads
//! of their own while the file is still recompressed: each formula a recompressor makes is
//! evaluated as it is made ([`Prevalidator`]), and where it gives what it stands for, each of its
//! applications is recorded by its module, parameters and the content hashes of its resources
//! and inputs ([`aqr_formula::chash`]), with the hashes of its output's pieces ([`Memo`]); no
//! output is kept. The archive is then verified by its hashes: of its data by their bytes, and of
//! each application by the record of the hashes of its inputs, without running a module, the
//! whole the file's hash (`aqr::Archive::hashed`). As content hashes compose over
//! concatenation, a record holds of an application however the formula was laid out after (its
//! contents put in, its streams merged, cut into blocks). Where an application is not recorded
//! (a formula not evaluated beforehand, or failing), the archive is decoded instead, running the
//! modules but where an application is recorded as a range of a file ([`Cached`]).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crate::memory::{self, Limits};
use aqr_decompress::Native;
use aqr_formula::aqr::{PartIn, PartOut};
use aqr_formula::chash::{Ch, Records, record_key, split_key};
use aqr_formula::{Apply, Formula, Modules, Node, Of, Result, Shared};
use xxhash_rust::xxh3::Xxh3;

/// What an application is ([`record_key`]).
type Key = u128;

/// The key of application `a` of outputs of `sizes` on `inputs`.
fn key(a: &Apply, inputs: &[&[u8]], sizes: &[u64]) -> Key {
    key_of(a, &inputs.iter().map(|i| Ch::of(i)).collect::<Vec<_>>(), sizes)
}

/// [`key`] of inputs given by their hashes.
fn key_of(a: &Apply, inputs: &[Ch], sizes: &[u64]) -> Key {
    let resources: Vec<Ch> = a.resources.iter().map(|r| Ch::of(r)).collect();
    record_key(&a.module.to_string(), &a.params, &resources, inputs, sizes)
}

/// What an application gave: the hashes of the pieces of its output (each output, or each piece
/// of an output split), and the range of a file (by index) it is, where it is one.
#[derive(Clone)]
struct Record {
    outputs: Vec<Ch>,
    file: Option<(usize, Range<usize>)>,
}

/// The applications evaluated beforehand, by [`key`]; and their memory limits, worked out as they
/// are run ([`crate::memory`]).
#[derive(Default)]
pub(crate) struct Memo(HashMap<Key, Record>, pub Limits);

impl Records for Memo {
    fn outputs(&self, key: u128) -> Option<Vec<Ch>> {
        match self.0.get(&key) {
            Some(r) => Some(r.outputs.clone()),
            None => LARGE.lock().unwrap().get(&key).cloned(),
        }
    }

    fn learn(&self, key: u128, outputs: &[Ch]) {
        if outputs.iter().map(Ch::len).sum::<u64>() >= LARGE_OUTPUT {
            LARGE.lock().unwrap().insert(key, outputs.to_vec());
        }
    }
}

/// Bytes of the outputs of an application at least for its record to be kept for every archive
/// and batch after it ([`LARGE`]).
const LARGE_OUTPUT: u64 = 1 << 20;

/// The hashes of the outputs of the applications of large outputs recorded so far in the process,
/// by [`key`]: the formula of a file evaluated once though it is in several (`enwik9` and the
/// entry of `enwik9.zip` in another batch) is recorded once, and a record holds wherever the
/// application is (of the same module, parameters and inputs). Of large outputs only, as they
/// are what decoding instead would cost: a few of them.
static LARGE: std::sync::LazyLock<Mutex<HashMap<Key, Vec<Ch>>>> = std::sync::LazyLock::new(Mutex::default);

/// Runs the modules, recording every application ([`Record`]): of the applications in `known`
/// (by address), the range of a file it is.
struct Recording {
    native: Native,
    known: HashMap<usize, (usize, Range<usize>)>,
    seen: Mutex<Vec<(Key, Record)>>,
    limits: Mutex<Limits>,
}

impl Recording {
    /// The file and range of it that `a` is, if it is one of the applications at the top.
    fn file_of(&self, a: &Apply) -> Option<(usize, Range<usize>)> {
        self.known.get(&(a as *const Apply as usize)).cloned()
    }
}

impl Modules for Recording {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let out = self.native.apply(a, inputs)?;
        let file = self.file_of(a);
        let r = Record { outputs: vec![Ch::of(&out)], file };
        self.seen.lock().unwrap().push((key(a, inputs, &[a.len]), r));
        self.limit(a, &[a.len], inputs);
        Ok(out)
    }

    fn is_part(&self, a: &Apply) -> bool {
        self.native.is_part(a)
    }

    fn apply_part(&self, a: &Apply, inputs: &mut [&mut dyn PartIn], output: &mut dyn PartOut) -> Result<()> {
        // (the inputs and the output hashed as they go by, none held but the short inputs, which
        // the plan of the memory may read: the hashes are the same as of the bytes whole)
        let mut tees: Vec<InTee> = inputs
            .iter_mut()
            .map(|i| InTee { held: (i.len() <= HOLD).then(Vec::new), inner: &mut **i, ch: Ch::EMPTY, done: 0 })
            .collect();
        let mut out = OutTee { inner: output, ch: Ch::EMPTY };
        {
            let mut ins: Vec<&mut dyn PartIn> = tees.iter_mut().map(|t| t as &mut dyn PartIn).collect();
            self.native.apply_part(a, &mut ins, &mut out)?;
        }
        // (what the module did not read is hashed too)
        for t in &mut tees {
            t.finish().map_err(|f| aqr_formula::Error::new(format!("{}: {f:?}", a.module)))?;
        }
        let file = self.file_of(a);
        let hashes: Vec<Ch> = tees.iter().map(|t| t.ch).collect();
        let r = Record { outputs: vec![out.ch], file };
        self.seen.lock().unwrap().push((key_of(a, &hashes, &[a.len]), r));
        let lens: Vec<u64> = tees.iter().map(|t| t.inner.len()).collect();
        let held = Held(tees.into_iter().map(|t| t.held).collect());
        let l = memory::limit(a, &lens, &[a.len], &held);
        self.limits.lock().unwrap().put(memory::shape(a, &lens), l);
        Ok(())
    }

    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        // (a patch: no module, nothing to record)
        let Some(a) = s.apply() else { return self.native.apply_shared(s, inputs) };
        // (one output split into pieces, as the archive has it: or several outputs)
        let sizes = match s.split {
            true => vec![s.sizes.iter().sum()],
            false => s.sizes.clone(),
        };
        // (the memories of its outputs: one of a split output)
        self.limit(a, &sizes, inputs);
        let outs = self.native.apply_shared(s, inputs)?;
        let r = Record { outputs: outs.iter().map(|o| Ch::of(o)).collect(), file: None };
        self.seen.lock().unwrap().push((key(a, inputs, &sizes), r));
        Ok(outs)
    }
}

/// Bytes of an input of an application applied as a part module at most for it to be held, for
/// the plan of the memory to read it (the records and the index of a module, say): the data of
/// such an application is far longer.
const HOLD: u64 = 256 << 20;

/// An input of a part module hashed as it is pulled, and held if it is short ([`HOLD`]).
struct InTee<'a> {
    inner: &'a mut dyn PartIn,
    ch: Ch,
    held: Option<Vec<u8>>,
    /// The bytes pulled so far.
    done: u64,
}

impl InTee<'_> {
    fn take(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> aqr_formula::aqr::Decoded<()> {
        let (ch, held) = (&mut self.ch, &mut self.held);
        self.inner.pull(hi, &mut |b| {
            *ch = ch.then(b);
            if let Some(h) = held {
                h.extend_from_slice(b);
            }
            put(b);
        })?;
        self.done = hi;
        Ok(())
    }

    /// Hashes what was not pulled.
    fn finish(&mut self) -> aqr_formula::aqr::Decoded<()> {
        match self.done < self.inner.len() {
            true => self.take(self.inner.len(), &mut |_| {}),
            false => Ok(()),
        }
    }
}

impl PartIn for InTee<'_> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> aqr_formula::aqr::Decoded<()> {
        self.take(hi, put)
    }
}

/// The output of a part module hashed as it is pushed.
struct OutTee<'a> {
    inner: &'a mut dyn PartOut,
    ch: Ch,
}

impl PartOut for OutTee<'_> {
    fn push(&mut self, d: &[u8]) -> aqr_formula::aqr::Decoded<()> {
        self.ch = self.ch.then(d);
        self.inner.push(d)
    }
}

/// The inputs of an application that were held, for its plan ([`HOLD`]).
struct Held(Vec<Option<Vec<u8>>>);

impl aqr_formula::Contents for Held {
    fn input(&self, k: usize) -> Option<&[u8]> {
        self.0.get(k)?.as_deref()
    }
}

impl Recording {
    /// The memory limit of `a` of outputs of `sizes` on `inputs`, kept.
    fn limit(&self, a: &Apply, sizes: &[u64], inputs: &[&[u8]]) {
        let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
        let l = memory::limit(a, &lens, sizes, &inputs.to_vec());
        self.limits.lock().unwrap().put(memory::shape(a, &lens), l);
    }
}

/// Runs the modules but where an application is recorded as a range of a file, which `files`
/// gives (by index) where it can (else the module is run); `hits` counts those.
///
/// The output of a module run that is a whole file of [`Cached::whole`] (by its length and
/// XXH3-128) is that file's bytes: the same application again (10gb: `enwik9` coded once, for
/// the file and the entry of `enwik9.zip`, each a leaf of its own) takes them from `files`
/// rather than running the module again.
pub(crate) struct Cached<'a> {
    pub native: Native,
    pub memo: Mutex<Memo>,
    pub hits: Mutex<usize>,
    pub files: &'a (dyn Fn(usize, Range<usize>) -> Option<Vec<u8>> + Sync),
    /// The files by their lengths: each its index and XXH3-128.
    pub whole: HashMap<usize, Vec<(usize, u128)>>,
    /// The applications run whose outputs are whole files, by key: the file's index.
    ran: Mutex<HashMap<Key, usize>>,
}

impl<'a> Cached<'a> {
    pub fn new(native: Native, memo: Memo, files: &'a (dyn Fn(usize, Range<usize>) -> Option<Vec<u8>> + Sync)) -> Self {
        Cached {
            native,
            memo: memo.into(),
            hits: Default::default(),
            files,
            whole: HashMap::new(),
            ran: Default::default(),
        }
    }
}

impl Modules for Cached<'_> {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let k = key(a, inputs, &[a.len]);
        let file = |i: usize, range: Range<usize>| match (self.files)(i, range) {
            Some(d) => Ok(d),
            None => self.native.apply(a, inputs),
        };
        if let Some((i, range)) = self.memo.lock().unwrap().0.get(&k).and_then(|r| r.file.clone()) {
            *self.hits.lock().unwrap() += 1;
            return file(i, range);
        }
        let ran = self.ran.lock().unwrap().get(&k).copied();
        if let Some(i) = ran {
            return file(i, 0..a.len as usize);
        }
        let out = self.native.apply(a, inputs)?;
        if let Some(of_len) = self.whole.get(&out.len()) {
            let h = hash(&out);
            if let Some(&(i, _)) = of_len.iter().find(|f| f.1 == h) {
                self.ran.lock().unwrap().insert(k, i);
            }
        }
        Ok(out)
    }

    // (not recorded as files)
    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        self.native.apply_shared(s, inputs)
    }
}

/// Evaluates formulas as they are submitted, into a [`Memo`]: each on a worker of the pool if
/// one is free, else then and there. A formula is shared with the recompression, and checked
/// against a hash of the data it stands for: neither is copied. The files are known by their
/// addresses: data within one is part of it.
pub(crate) struct Prevalidator {
    memo: Arc<Mutex<Memo>>,
    /// The evaluations on workers.
    running: Mutex<Vec<aqr_common::pool::Handle<'static, ()>>>,
    threads: usize,
    /// Where each file is, by address (ordered by it), and its index.
    files: Vec<(Range<usize>, usize)>,
}

/// A formula, the hash of its data, and where the data is: in a file (the file's index, and the
/// offset), if it is.
type Job = (Arc<Formula>, Key, Where);

enum Where {
    File(usize, usize),
    Elsewhere,
}

/// A formula of data to evaluate beforehand, later ([`Prevalidator::job`]).
pub(crate) struct Deferred(Job);

fn hash(d: &[u8]) -> Key {
    xxhash_rust::xxh3::xxh3_128(d)
}

impl Prevalidator {
    /// A prevalidator of the formulas made of `files` and of what they hold, each evaluated on
    /// up to `threads` threads.
    pub fn spawn(threads: usize, files: &[&[u8]]) -> Prevalidator {
        let mut files: Vec<(Range<usize>, usize)> = files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.as_ptr_range().start as usize..f.as_ptr_range().end as usize, i))
            .collect();
        files.sort_unstable_by_key(|f| f.0.start);
        Prevalidator { memo: Arc::default(), running: Mutex::default(), threads, files }
    }

    /// Has formula `f` of data `d` evaluated: where it gives `d`, its applications are recorded
    /// in the memo.
    pub fn submit(&self, f: Arc<Formula>, d: &[u8]) {
        if let Some(j) = self.job(f, d) {
            self.send(j);
        }
    }

    /// What [`Prevalidator::submit`] would have evaluated, to [`Prevalidator::send`] later (or
    /// not): `None` for a formula without applications.
    pub fn job(&self, f: Arc<Formula>, d: &[u8]) -> Option<Deferred> {
        let at = d.as_ptr() as usize;
        let within = self.files[..self.files.partition_point(|f| f.0.start <= at)]
            .last()
            .filter(|(r, _)| r.contains(&at) && at + d.len() <= r.end)
            .map(|(r, i)| Where::File(*i, at - r.start));
        if f.applies().is_empty() {
            return None;
        }
        let within = within.unwrap_or(Where::Elsewhere);
        Some(Deferred((f, hash(d), within)))
    }

    /// Has a formula of [`Prevalidator::job`] evaluated: not as part of the recompression that
    /// made it, whose heap it does not count to and which it outlives (applications are held
    /// to their plans, [`crate::memory`]).
    pub fn send(&self, j: Deferred) {
        let _a = aqr_common::allowance::enter(aqr_common::allowance::Context::NONE);
        let (memo, threads) = (self.memo.clone(), self.threads);
        let eval = move || {
            let _s = aqr_common::trace::span("prevalidate");
            let mut m = Memo::default();
            evaluate(j.0, threads, &mut m);
            let mut memo = memo.lock().unwrap();
            memo.0.extend(m.0);
            memo.1.extend(m.1);
        };
        let mut worker = aqr_common::pool::reserve(1);
        match worker.is_empty() {
            true => eval(),
            false => self.running.lock().unwrap().push(worker.spawn(eval)),
        }
    }

    /// Records the Splits of formula `f` whose bytes are at hand (of a slice of a stream made, as a
    /// disk image's content is before it is recompressed in turn): the hashes of their pieces by
    /// the hash of what they split ([`split_key`]), on up to `threads` threads. The verify then
    /// cuts them by their hashes where the hashes of what they split, as it is evaluated after,
    /// do not fall where the pieces do: a DMG's disk, cut into its chunks, ran its files'
    /// modules again in the verify for their bytes (3.6 GB of heap).
    pub fn learn_splits(&self, f: &Formula, threads: usize) {
        for s in &f.shared {
            let Of::Node(Node::Slice { stream, start, len }) = s.of else { continue };
            let data = &f.streams[stream].data;
            if data.is_lazy() || data.sunk_in().is_some() || start.saturating_add(len) > data.len() as u64 {
                continue;
            }
            let mut at = vec![start as usize];
            for &n in &s.sizes {
                at.push(at.last().unwrap() + n as usize);
            }
            let pieces = aqr_common::pool::par_map(s.sizes.len(), threads.max(1), |k| Ch::of(&data[at[k]..at[k + 1]]));
            let whole = pieces.iter().fold(Ch::EMPTY, |w, &c| w.cat(c));
            let key = split_key(whole, &s.sizes);
            if len >= LARGE_OUTPUT {
                LARGE.lock().unwrap().insert(key, pieces.clone());
            }
            self.memo.lock().unwrap().0.insert(key, Record { outputs: pieces, file: None });
        }
    }

    /// [`Prevalidator::finish`] of one shared, once nothing else holds it.
    pub fn finish_shared(p: Arc<Prevalidator>) -> Memo {
        Arc::into_inner(p).expect("the prevalidator held only here").finish()
    }

    /// The memo, once every formula submitted is evaluated.
    pub fn finish(self) -> Memo {
        // (one that panicked leaves its outputs out)
        self.running.into_inner().unwrap().into_iter().for_each(|h| _ = h.join());
        Arc::into_inner(self.memo).expect("evaluations done").into_inner().unwrap()
    }
}

/// Evaluates job `j` on `threads` threads, recording its applications in `memo` where it gives
/// what it stands for: those at the top of its formula as the ranges of a file they are, where
/// its data is in one.
fn evaluate((f, d, at): Job, threads: usize, memo: &mut Memo) {
    let native = Native { threads };
    let top = top(&f.root).into_iter();
    let r = Recording {
        native,
        known: match at {
            Where::File(i, at) => top.map(|(a, r)| (a, (i, at + r.start..at + r.end))).collect(),
            Where::Elsewhere => HashMap::new(),
        },
        seen: Mutex::default(),
        limits: Mutex::default(),
    };
    // (its output hashed as it is made, not held whole; the applications among what it
    // concatenates made ahead on the threads)
    let mut h = Xxh3::new();
    let good = f
        .eval_parts(&r, threads, &mut |p| {
            h.update(p);
            Ok(())
        })
        .is_ok()
        && h.digest128() == d;
    let Recording { seen, limits, .. } = r;
    drop(f);
    if good {
        let seen = seen.into_inner().unwrap();
        let mut large = LARGE.lock().unwrap();
        for (k, r) in &seen {
            if r.outputs.iter().map(Ch::len).sum::<u64>() >= LARGE_OUTPUT {
                large.insert(*k, r.outputs.clone());
            }
        }
        drop(large);
        memo.0.extend(seen);
        memo.1.extend(limits.into_inner().unwrap());
    }
}

/// The applications at the top of `n`, by address, with the ranges of their outputs in its
/// output: `n`, or those it concatenates.
fn top(n: &Node) -> Vec<(usize, Range<usize>)> {
    let mut out = Vec::new();
    let mut p = 0;
    for n in if let Node::Concat(ns) = n { &ns[..] } else { std::slice::from_ref(n) } {
        let len = n.len() as usize;
        if let Node::Apply(a) = n {
            out.push((&**a as *const Apply as usize, p..p + len));
        }
        p += len;
    }
    out
}
