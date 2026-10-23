//! Verification of the archive that [`compress`](crate::compress) makes (xform-design.md §6):
//! it is decoded, and must give the file back. The applications of modules are evaluated
//! beforehand, on a thread of their own while the file is still recompressed: each formula a
//! recompressor makes is evaluated as it is made ([`Prevalidator`]), and where it gives what it
//! stands for, the outputs of its applications are kept by what they were applied to ([`Memo`]).
//! Decoding the archive then takes an application's output from there where its module,
//! parameters, resources and inputs are the same ([`Cached`]), rather than running the module
//! again: modules are pure functions, so that this is the output it would give. Everything else
//! of the archive (its blocks, the formula block, the streams, the checksums) is decoded as it
//! would be.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use aqr_decompress::Native;
use aqr_formula::{Apply, Formula, Modules, Result};
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

/// Outputs of applications, by [`key`].
#[derive(Default)]
pub(crate) struct Memo(HashMap<Key, Arc<Vec<u8>>>);

/// Runs the modules, keeping the output of every application by its key.
struct Recording {
    native: Native,
    seen: Mutex<Vec<(Key, Arc<Vec<u8>>)>>,
}

impl Modules for Recording {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let out = self.native.apply(a, inputs)?;
        self.seen.lock().unwrap().push((key(a, inputs), Arc::new(out.clone())));
        Ok(out)
    }
}

/// Runs the modules where the output of an application is not in the memo; `hits` counts those
/// that are. An output is taken out of the memo when it is used: decoding an archive evaluates
/// an application once.
pub(crate) struct Cached {
    pub native: Native,
    pub memo: Mutex<Memo>,
    pub hits: Mutex<usize>,
}

impl Modules for Cached {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let hit = self.memo.lock().unwrap().0.remove(&key(a, inputs));
        match hit {
            Some(out) => {
                *self.hits.lock().unwrap() += 1;
                Ok(Arc::try_unwrap(out).unwrap_or_else(|out| out.to_vec()))
            }
            None => self.native.apply(a, inputs),
        }
    }
}

/// Evaluates formulas on a thread of its own, as they are submitted, into a [`Memo`]. A
/// formula is shared with the recompression, and checked against a hash of the data it stands
/// for: neither is copied.
pub(crate) struct Prevalidator {
    jobs: Sender<(Arc<Formula>, Key)>,
    done: JoinHandle<Memo>,
}

fn hash(d: &[u8]) -> Key {
    xxhash_rust::xxh3::xxh3_128(d)
}

/// Formulas of at most this many bytes of streams are copied by [`reclaim`] rather than waited
/// for.
const RECLAIM_COPY: usize = 64 << 20;

/// `f` once the [`Prevalidator`] is done with it: a copy where it is small and still evaluated
/// (or waiting to be), else `f` itself when it is.
pub(crate) fn reclaim(mut f: Arc<Formula>) -> Formula {
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
    pub fn spawn(threads: usize) -> Prevalidator {
        let (jobs, rx) = channel::<(Arc<Formula>, Key)>();
        let done = std::thread::spawn(move || {
            let mut memo = Memo::default();
            for (f, d) in rx {
                let r = Recording { native: Native { threads }, seen: Mutex::default() };
                let good = f.eval(&r).is_ok_and(|out| hash(&out) == d);
                drop(f);
                if good {
                    memo.0.extend(r.seen.into_inner().unwrap());
                }
            }
            memo
        });
        Prevalidator { jobs, done }
    }

    /// Has formula `f` of data `d` evaluated: where it gives `d`, its applications' outputs go
    /// to the memo.
    pub fn submit(&self, f: Arc<Formula>, d: &[u8]) {
        if !f.applies().is_empty() {
            // the thread is gone only if it panicked, which `finish` reports
            let _ = self.jobs.send((f, hash(d)));
        }
    }

    /// The memo, once every formula submitted is evaluated.
    pub fn finish(self) -> Memo {
        drop(self.jobs);
        self.done.join().unwrap_or_default()
    }
}
