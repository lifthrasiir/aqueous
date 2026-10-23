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
/// that are.
pub(crate) struct Cached<'a> {
    pub native: Native,
    pub memo: &'a Memo,
    pub hits: Mutex<usize>,
}

impl Modules for Cached<'_> {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        match self.memo.0.get(&key(a, inputs)) {
            Some(out) => {
                *self.hits.lock().unwrap() += 1;
                Ok(out.to_vec())
            }
            None => self.native.apply(a, inputs),
        }
    }
}

/// Evaluates formulas on a thread of its own, as they are submitted, into a [`Memo`].
pub(crate) struct Prevalidator {
    jobs: Sender<(Formula, Vec<u8>)>,
    done: JoinHandle<Memo>,
}

impl Prevalidator {
    pub fn spawn(threads: usize) -> Prevalidator {
        let (jobs, rx) = channel::<(Formula, Vec<u8>)>();
        let done = std::thread::spawn(move || {
            let mut memo = Memo::default();
            for (f, d) in rx {
                let r = Recording { native: Native { threads }, seen: Mutex::default() };
                if f.eval(&r).is_ok_and(|out| out == d) {
                    memo.0.extend(r.seen.into_inner().unwrap());
                }
            }
            memo
        });
        Prevalidator { jobs, done }
    }

    /// Has formula `f` of data `d` evaluated: where it gives `d`, its applications' outputs go
    /// to the memo.
    pub fn submit(&self, f: &Formula, d: &[u8]) {
        if !f.applies().is_empty() {
            // the thread is gone only if it panicked, which `finish` reports
            let _ = self.jobs.send((f.clone(), d.to_vec()));
        }
    }

    /// The memo, once every formula submitted is evaluated.
    pub fn finish(self) -> Memo {
        drop(self.jobs);
        self.done.join().unwrap_or_default()
    }
}
