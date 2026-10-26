//! Verification of the archive that [`compress`](crate::compress) makes (xform-design.md §6):
//! it is decoded, and must give the file back. The applications of modules are evaluated
//! beforehand, on threads of their own while the file is still recompressed: each formula a
//! recompressor makes is evaluated as it is made ([`Prevalidator`]), and where it gives what it
//! stands for, the outputs of its applications are kept by what they were applied to ([`Memo`]).
//! Decoding the archive then takes an application's output from there where its module,
//! parameters, resources and inputs are the same ([`Cached`]), rather than running the module
//! again: modules are pure functions, so that this is the output it would give. Everything else
//! of the archive (its blocks, the formula block, the streams, the checksums) is decoded as it
//! would be. An application whose output is part of the file (that of a formula of a file in
//! the file, at its top) is not kept in the memo, but known to give that part of the file.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use aqr_decompress::Native;
use aqr_formula::{Apply, Formula, Modules, Node, Result, Shared};
use xxhash_rust::xxh3::Xxh3;

/// What an application is: its module, parameters, resources and inputs, hashed.
type Key = u128;

fn key(a: &Apply, inputs: &[&[u8]]) -> Key {
    let mut h = Xxh3::new();
    let mut part = |b: &[u8]| {
        h.update(&(b.len() as u64).to_le_bytes());
        h.update(b);
    };
    part(a.module.name.as_bytes());
    part(&a.module.version.to_le_bytes());
    part(&a.params);
    part(&(a.resources.len() as u64).to_le_bytes());
    a.resources.iter().for_each(|r| part(r));
    part(&(inputs.len() as u64).to_le_bytes());
    inputs.iter().for_each(|i| part(i));
    part(&a.len.to_le_bytes());
    h.digest128()
}

/// An application's output: its bytes, or a range of a file (by index).
enum Output {
    Bytes(Arc<Vec<u8>>),
    File(usize, Range<usize>),
}

/// Outputs of applications, by [`key`].
#[derive(Default)]
pub(crate) struct Memo(HashMap<Key, Output>);

impl Memo {
    /// Bytes of the outputs kept.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.0.values().map(|o| if let Output::Bytes(b) = o { b.len() } else { 0 }).sum()
    }
}

/// Runs the modules, keeping the output of every application by its key: as a range of a
/// file for the applications in `file` (by address, with the file's index and the range).
struct Recording {
    native: Native,
    file: Vec<(usize, usize, Range<usize>)>,
    seen: Mutex<Vec<(Key, Output)>>,
}

impl Modules for Recording {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let out = self.native.apply(a, inputs)?;
        let kept = match self.file.iter().find(|(at, ..)| std::ptr::addr_eq(a, *at as *const Apply)) {
            Some((_, i, range)) => Output::File(*i, range.clone()),
            None => Output::Bytes(Arc::new(out.clone())),
        };
        self.seen.lock().unwrap().push((key(a, inputs), kept));
        Ok(out)
    }

    // (not kept)
    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        self.native.apply_shared(s, inputs)
    }
}

/// Runs the modules where the output of an application is not in the memo; `hits` counts those
/// that are. An output is taken out of the memo when it is used: decoding an archive evaluates
/// an application once. `files` gives a range of a file (by index) that the formulas evaluated
/// beforehand are of, where it can (else the module is run).
pub(crate) struct Cached<'a> {
    pub native: Native,
    pub memo: Mutex<Memo>,
    pub hits: Mutex<usize>,
    pub files: &'a (dyn Fn(usize, Range<usize>) -> Option<Vec<u8>> + Sync),
}

impl Modules for Cached<'_> {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let hit = self.memo.lock().unwrap().0.remove(&key(a, inputs));
        if hit.is_some() {
            *self.hits.lock().unwrap() += 1;
        }
        match hit {
            Some(Output::Bytes(out)) => Ok(Arc::try_unwrap(out).unwrap_or_else(|out| out.to_vec())),
            Some(Output::File(i, range)) => match (self.files)(i, range) {
                Some(d) => Ok(d),
                None => self.native.apply(a, inputs),
            },
            None => self.native.apply(a, inputs),
        }
    }

    // (not in the memo)
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

/// A formula, the hash of its data, and where the data is in a file if it is (the file's
/// index, and the offset).
type Job = (Arc<Formula>, Key, Option<(usize, usize)>);

/// A formula of data to evaluate beforehand, later ([`Prevalidator::job`]).
pub(crate) struct Deferred(Job);

fn hash(d: &[u8]) -> Key {
    xxhash_rust::xxh3::xxh3_128(d)
}

/// Formulas of at most this many bytes of streams are copied by [`reclaim`] rather than waited
/// for.
const RECLAIM_COPY: usize = 64 << 20;

/// `f` once the [`Prevalidator`] is done with it: a copy where it is small and still evaluated
/// (or waiting to be), else `f` itself when it is.
pub(crate) fn reclaim(mut f: Arc<Formula>) -> Formula {
    #[cfg(test)]
    if crate::tests::COPY.with(std::cell::Cell::get) {
        return (*f).clone();
    }
    loop {
        match Arc::try_unwrap(f) {
            Ok(f) => return f,
            Err(shared) if shared.streams.iter().map(|s| s.data.len()).sum::<usize>() <= RECLAIM_COPY => {
                return (*shared).clone();
            }
            Err(shared) => {
                f = shared;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
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

    /// Has formula `f` of data `d` evaluated: where it gives `d`, its applications' outputs go
    /// to the memo.
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
            .map(|(r, i)| (*i, at - r.start));
        (!f.applies().is_empty()).then(|| Deferred((f, hash(d), within)))
    }

    /// Has a formula of [`Prevalidator::job`] evaluated.
    pub fn send(&self, j: Deferred) {
        let (memo, threads) = (self.memo.clone(), self.threads);
        let eval = move || {
            let mut m = Memo::default();
            evaluate(j.0, threads, &mut m);
            memo.lock().unwrap().0.extend(m.0);
        };
        let mut worker = aqr_common::pool::reserve(1);
        match worker.is_empty() {
            true => eval(),
            false => self.running.lock().unwrap().push(worker.spawn(eval)),
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

/// Evaluates job `j` on `threads` threads, keeping its applications' outputs in `memo` where it
/// gives what it stands for. The file is not kept again: the outputs of the applications at
/// the top of a formula of data in it are where they make it.
fn evaluate((f, d, at): Job, threads: usize, memo: &mut Memo) {
    let r = Recording {
        native: Native { threads },
        file: at.map_or_else(Vec::new, |(i, at)| top(&f.root, i, at)),
        seen: Mutex::default(),
    };
    let good = f.eval(&r).is_ok_and(|out| hash(&out) == d);
    drop(f);
    if good {
        memo.0.extend(r.seen.into_inner().unwrap());
    }
}

/// The applications at the top of `n`, by address, with the ranges of their outputs in file
/// `file`, where the output of `n` is at `at`: `n`, or those it concatenates.
fn top(n: &Node, file: usize, at: usize) -> Vec<(usize, usize, Range<usize>)> {
    let mut out = Vec::new();
    let mut p = at;
    for n in if let Node::Concat(ns) = n { &ns[..] } else { std::slice::from_ref(n) } {
        let len = n.len() as usize;
        if let Node::Apply(a) = n {
            out.push((&**a as *const Apply as usize, file, p..p + len));
        }
        p += len;
    }
    out
}
