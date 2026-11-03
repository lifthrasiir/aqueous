//! Verification of the archive that [`compress`](crate::compress) makes (docs/xform-design.md §6):
//! it is decoded, and must give the file back. The applications of modules are evaluated
//! beforehand, on threads of their own while the file is still recompressed: each formula a
//! recompressor makes is evaluated as it is made ([`Prevalidator`]), and where it gives what it
//! stands for, the outputs of its applications are kept by what they were applied to ([`Memo`]).
//! Decoding the archive then takes an application's output from there where its module,
//! parameters, resources and inputs are the same ([`Cached`]), rather than running the module
//! again: modules are pure functions, so that this is the output it would give. Everything else
//! of the archive (its blocks, the formula block, the streams, the checksums) is decoded as it
//! would be. An application whose output is part of the file (that of a formula of a file in
//! the file, at its top) is not kept in the memo, but known to give that part of the file; one
//! whose output is a stream of the store (a content's formula's, the content spilled) shares
//! it rather than a copy ([`aqr_formula::store::find`]).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crate::memory::{self, Limits};
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

/// An application's output: its bytes, a range of a file (by index), or its bytes compressed
/// (a Zstandard frame at [`PACKED_LEVEL`], and their length).
#[derive(Clone)]
enum Output {
    /// (of the store where one is set, see [`crate::Options::spill`])
    Bytes(aqr_formula::Bytes),
    File(usize, Range<usize>),
    Packed(aqr_formula::Bytes, usize),
}

/// The zstd level of the outputs kept in the memo that are copies ([`Output::Packed`]): they are
/// read once, and are most of what compressing a large file holds at its end (3 GB of the
/// Windows Server DVD's 9).
const PACKED_LEVEL: i32 = 1;

/// Outputs of applications, by [`key`]; and their memory limits, worked out as they are run
/// ([`crate::memory`]).
#[derive(Default)]
pub(crate) struct Memo(HashMap<Key, Output>, pub Limits);

impl Memo {
    /// Bytes of the outputs kept.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.0
            .values()
            .map(|o| match o {
                Output::Bytes(b) | Output::Packed(b, _) => b.len(),
                Output::File(..) => 0,
            })
            .sum()
    }
}

/// Runs the modules, keeping the output of every application by its key: as it is known for
/// the applications in `known` (by address: a range of a file, or bytes of the store), else
/// the bytes themselves once the evaluation is done with them ([`Modules::keep`]), rather than
/// a copy as they are made.
struct Recording<'f> {
    native: Native,
    known: Vec<(usize, Output)>,
    seen: Mutex<Vec<(Key, Output)>>,
    /// The keys of the applications whose outputs are to be kept, by address.
    keys: Mutex<HashMap<usize, Key>>,
    limits: Mutex<Limits>,
    /// Applications run beforehand ([`Ahead`]).
    ahead: Mutex<Ahead<'f>>,
}

impl Modules for Recording<'_> {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let early = self.ahead.lock().unwrap().take(a, &self.native);
        let out = match early {
            Some(out) => out,
            None => self.native.apply(a, inputs)?,
        };
        match self.known.iter().find(|(at, _)| std::ptr::addr_eq(a, *at as *const Apply)) {
            Some((_, o)) => self.seen.lock().unwrap().push((key(a, inputs), o.clone())),
            None => _ = self.keys.lock().unwrap().insert(a as *const Apply as usize, key(a, inputs)),
        }
        self.limit(a, &[a.len], inputs);
        Ok(out)
    }

    fn keep(&self, a: &Apply, out: Vec<u8>) {
        if let Some(k) = self.keys.lock().unwrap().remove(&(a as *const Apply as usize)) {
            // (compressed, and moved to the store where one is set, see [`crate::Options::spill`])
            let packed = zstd::bulk::compress(&out, PACKED_LEVEL).expect("compressed");
            let mut b = aqr_formula::store::Buffer::new(packed.len());
            b.copy_from_slice(&packed);
            self.seen.lock().unwrap().push((k, Output::Packed(b.into_bytes(), out.len())));
        }
    }

    // (not kept)
    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        self.limit(&s.apply, &s.sizes, inputs);
        self.native.apply_shared(s, inputs)
    }
}

impl Recording<'_> {
    /// The memory limit of `a` of outputs of `sizes` on `inputs`, kept.
    fn limit(&self, a: &Apply, sizes: &[u64], inputs: &[&[u8]]) {
        let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
        let l = memory::limit(a, &lens, sizes, &inputs.to_vec());
        self.limits.lock().unwrap().put(memory::shape(a, &lens), l);
    }
}

/// Runs the modules where the output of an application is not in the memo; `hits` counts those
/// that are. An output is taken out of the memo when it is used: decoding an archive evaluates
/// an application once. `files` gives a range of a file (by index) that the formulas evaluated
/// beforehand are of, where it can (else the module is run).
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
        let k = key(a, inputs);
        let hit = self.memo.lock().unwrap().0.remove(&k);
        if hit.is_some() {
            *self.hits.lock().unwrap() += 1;
        }
        let file = |i: usize, range: Range<usize>| match (self.files)(i, range) {
            Some(d) => Ok(d),
            None => self.native.apply(a, inputs),
        };
        match hit {
            Some(Output::Bytes(out)) => Ok(out.into_vec()),
            Some(Output::Packed(c, n)) => Ok(zstd::bulk::decompress(&c, n).expect("an output kept")),
            Some(Output::File(i, range)) => file(i, range),
            None => {
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

/// A formula, the hash of its data, and where the data is: in a file (the file's index, and the
/// offset), or in the store (a stream spilled, its bytes shared), if it is.
type Job = (Arc<Formula>, Key, Where);

enum Where {
    File(usize, usize),
    Store(aqr_formula::Bytes),
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

    /// Has formula `f` of data `d` evaluated: where it gives `d`, its applications' outputs go
    /// to the memo (sharing `d` where it is of the store and `share`, else copied).
    pub fn submit(&self, f: Arc<Formula>, d: &[u8], share: bool) {
        if let Some(j) = self.job(f, d, share) {
            self.send(j);
        }
    }

    /// What [`Prevalidator::submit`] would have evaluated, to [`Prevalidator::send`] later (or
    /// not): `None` for a formula without applications.
    pub fn job(&self, f: Arc<Formula>, d: &[u8], share: bool) -> Option<Deferred> {
        let at = d.as_ptr() as usize;
        let within = self.files[..self.files.partition_point(|f| f.0.start <= at)]
            .last()
            .filter(|(r, _)| r.contains(&at) && at + d.len() <= r.end)
            .map(|(r, i)| Where::File(*i, at - r.start));
        if f.applies().is_empty() {
            return None;
        }
        let shared = || share.then(|| aqr_formula::store::find(d)).flatten();
        let within = within.or_else(|| shared().map(Where::Store)).unwrap_or(Where::Elsewhere);
        Some(Deferred((f, hash(d), within)))
    }

    /// Has a formula of [`Prevalidator::job`] evaluated.
    pub fn send(&self, j: Deferred) {
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
/// gives what it stands for. The data is not kept again where it is in a file or the store:
/// the outputs of the applications at the top of its formula are where they make it.
fn evaluate((f, d, at): Job, threads: usize, memo: &mut Memo) {
    let native = Native { threads };
    let top = top(&f.root).into_iter();
    let r = Recording {
        native,
        known: match at {
            Where::File(i, at) => top.map(|(a, r)| (a, Output::File(i, at + r.start..at + r.end))).collect(),
            Where::Store(b) => top.map(|(a, r)| (a, Output::Bytes(b.slice(r)))).collect(),
            Where::Elsewhere => Vec::new(),
        },
        seen: Mutex::default(),
        keys: Mutex::default(),
        limits: Mutex::default(),
        ahead: Mutex::new(Ahead::of(&f, threads)),
    };
    let good = match &f.root {
        // (what it concatenates hashed as it is made, not held whole)
        Node::Concat(_) => {
            let mut h = Xxh3::new();
            f.eval_parts(&r, &mut |p| {
                h.update(p);
                Ok(())
            })
            .is_ok()
                && h.digest128() == d
        }
        _ => match f.eval(&r) {
            Ok(out) if hash(&out) == d => {
                // (the output of the formula, where it is an application's)
                if let Node::Apply(a) = &f.root {
                    r.keep(a, out);
                }
                true
            }
            _ => false,
        },
    };
    let Recording { seen, limits, ahead, .. } = r;
    drop(ahead);
    drop(f);
    if good {
        memo.0.extend(seen.into_inner().unwrap());
        memo.1.extend(limits.into_inner().unwrap());
    }
}

/// The applications of a formula among what its root concatenates whose inputs are slices of
/// its streams, run beforehand on up to `threads` threads (those that fail left out): the
/// evaluation of a formula is in turn, and a file of many applications (an FSB5 bank of many
/// Vorbis samples) would run them all on one thread. They are run `threads` at a time (of
/// [`AHEAD_BYTES`] of outputs at most, but one), as the first of them is asked for, so that
/// their outputs are not all held at once (a file of JSON lines in segments of its own).
/// The most bytes of the outputs of a window of [`Ahead`] (but one).
const AHEAD_BYTES: u64 = 256 << 20;

struct Ahead<'f> {
    applies: Vec<(&'f Apply, Vec<&'f [u8]>)>,
    threads: usize,
    /// The first of `applies` not run yet.
    next: usize,
    outs: HashMap<usize, Vec<u8>>,
}

impl<'f> Ahead<'f> {
    fn of(f: &'f Formula, threads: usize) -> Ahead<'f> {
        let mut applies = ahead(f);
        if applies.len() < 2 {
            applies.clear();
        }
        Ahead { applies, threads: threads.max(1), next: 0, outs: HashMap::new() }
    }

    /// The output of `a`, where it is run beforehand (with those after it, of a window).
    fn take(&mut self, a: &Apply, native: &Native) -> Option<Vec<u8>> {
        let at = a as *const Apply as usize;
        if let Some(out) = self.outs.remove(&at) {
            return Some(out);
        }
        let i = self.next + self.applies[self.next..].iter().position(|(x, _)| std::ptr::addr_eq(*x, a))?;
        let mut end = i + 1;
        let mut bytes = a.len;
        while end < self.applies.len() && end - i < self.threads && bytes + self.applies[end].0.len <= AHEAD_BYTES {
            bytes += self.applies[end].0.len;
            end += 1;
        }
        let window = &self.applies[i..end];
        let outs =
            aqr_common::pool::par_map(window.len(), self.threads, |k| native.apply(window[k].0, &window[k].1).ok());
        for ((a, _), o) in window.iter().zip(outs) {
            if let Some(o) = o {
                self.outs.insert(*a as *const Apply as usize, o);
            }
        }
        self.next = end;
        self.outs.remove(&at)
    }
}

/// The applications of `f` among what its root concatenates whose inputs are slices of its
/// streams, in order ([`Ahead`]).
fn ahead(f: &Formula) -> Vec<(&Apply, Vec<&[u8]>)> {
    let mut applies = Vec::new();
    let mut walk = vec![&f.root];
    while let Some(n) = walk.pop() {
        match n {
            // (in the order they are evaluated)
            Node::Concat(ns) => walk.extend(ns.iter().rev()),
            Node::Apply(a) => {
                let inputs: Option<Vec<&[u8]>> = a
                    .inputs
                    .iter()
                    .map(|i| match *i {
                        Node::Slice { stream, start, len } => {
                            f.streams.get(stream)?.data.get(start as usize..)?.get(..len as usize)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(inputs) = inputs {
                    applies.push((&**a, inputs));
                }
            }
            _ => {}
        }
    }
    applies
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
