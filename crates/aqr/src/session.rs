//! `inventory` requests (`--ipc`, `man/aqripc.7`): the inputs of a compression gathered ahead of
//! it, as a compression dialog asks while its user chooses the options (how many files, how
//! big); the `run` that compresses the same takes what was gathered ([`take`]).
//!
//! ```text
//! > {"q":7,"op":"inventory","args":["-c","-x","node_modules","--","a","b"]}
//! < {"r":7,"more":true,"files":120,"bytes":3400000,"walked":false,"done":false}
//! < {"r":7,"files":1400,"bytes":61000000,"walked":true,"done":true}
//! > {"q":8,"op":"run","args":["-c","-l","7","-x","node_modules","-o","a.aqr","--","a","b"]}
//! ```
//!
//! - The arguments are what `aqr` is given to compress (only those that bear on what is gathered
//!   need be there: `-x`, `-L`, `-p`, `--rm`, and the inputs). Replies come now and then while
//!   the files are listed (`files` found, `bytes` of those read so far, `walked`: whether all the
//!   directories are walked, the bytes still growing until `done`), the last with `done` true;
//!   or an error. Nothing is known for a while after a request.
//! - A new request replaces the one at work (its work stopped, its last reply an error
//!   `cancelled`), unless it gathers the same: then it is the same work, replied to the new one.
//! - The `run` of the same inputs, exclusions, `-L`, `-p` and `--rm` takes what was gathered
//!   ([`cached`]), or waits for it where it is at work; anything else is gathered again.
//!
//! The pool of threads is of the size `aqr` had on start (`-T` of the run does not change it).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use aqr_ipc::wire::kind;
use serde_json::{Value, json};

use crate::{Args, Exclude, Gathered, Inputs, Mapper, gather, ipc, view};

/// What a gathering depends on.
#[derive(Clone, PartialEq)]
struct Key {
    inputs: Vec<PathBuf>,
    given: Vec<PathBuf>,
    renames: Vec<Option<PathBuf>>,
    exclude: Exclude,
    dereference: bool,
    preserve: bool,
    rm: bool,
}

impl Key {
    fn new(a: &Args, inputs: Inputs) -> Key {
        Key {
            inputs: inputs.paths.to_vec(),
            given: inputs.given.to_vec(),
            renames: inputs.renames.to_vec(),
            exclude: a.exclude.clone(),
            dereference: a.dereference,
            preserve: a.preserve,
            rm: a.rm,
        }
    }
}

/// The error of a gathering stopped.
pub const CANCELLED: &str = "cancelled";

/// What the last inventory gathered, to be taken by the compression of the same.
static CACHE: Mutex<Option<(Key, Gathered)>> = Mutex::new(None);

/// What was gathered for these inputs, as `a` has them gathered, where an inventory did.
pub fn cached(a: &Args, inputs: Inputs) -> Option<Gathered> {
    let mut c = CACHE.lock().unwrap();
    if c.as_ref().is_some_and(|(k, _)| *k == Key::new(a, inputs)) { c.take().map(|(_, g)| g) } else { None }
}

/// An inventory at work.
struct Running {
    key: Key,
    /// The request replied to: the last one's.
    id: Arc<AtomicU64>,
    listing: Arc<view::Listing>,
    /// The last event, once it is done.
    last: Arc<Mutex<Option<Value>>>,
    handle: JoinHandle<()>,
}

impl Running {
    fn start(id: u64, a: Args) -> Running {
        let key = Key::new(&a, Inputs::of(&a));
        let id = Arc::new(AtomicU64::new(id));
        let listing = Arc::new(view::Listing::new(false));
        let last = Arc::new(Mutex::new(None));
        let handle = {
            let (key, id, listing, last) = (key.clone(), id.clone(), listing.clone(), last.clone());
            std::thread::Builder::new()
                .name("inventory".into())
                .spawn(move || inventory(key, a, &id, &listing, &last))
                .expect("a thread for the inventory")
        };
        Running { key, id, listing, last, handle }
    }

    /// Replied to request `id` from now on, as the same (the one before told so, where it was
    /// not done: its last reply then given already).
    fn adopt(&self, id: u64) {
        let last = self.last.lock().unwrap();
        let before = self.id.swap(id, Ordering::Relaxed);
        match last.clone() {
            Some(v) => reply(id, v),
            None => ipc::fail(before, kind::CANCELLED, "inventory: replaced by another of the same"),
        }
    }

    /// Stopped, and waited for: its request told so where it was not done.
    fn stop(self) {
        self.listing.cancel();
        _ = self.handle.join();
        if self.last.lock().unwrap().is_none() {
            ipc::fail(self.id.load(Ordering::Relaxed), kind::CANCELLED, "inventory: cancelled");
        }
    }
}

fn event(l: &view::Listing, done: bool) -> Value {
    let (files, bytes, walked) = l.counts();
    json!({"files": files, "bytes": bytes, "walked": walked, "done": done})
}

/// Replies `v` to inventory `id`: the last where it is `done`.
fn reply(id: u64, v: Value) {
    let more = v["done"] != true;
    ipc::reply(id, v, more, &[]);
}

/// Gathers `a`'s inputs as a compression would, telling how far it is; what is gathered is kept
/// for [`cached`].
fn inventory(key: Key, a: Args, id: &AtomicU64, l: &view::Listing, last: &Mutex<Option<Value>>) {
    let r = crate::ticked(true, &|| reply(id.load(Ordering::Relaxed), event(l, false)), || {
        aqr_common::pool::run(|| -> Result<Gathered, String> {
            let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
            let mut g = Gathered::new(mapper, a.preserve);
            g.gone = a.rm.then(Vec::new);
            gather(Inputs::of(&a), &mut g, l, &a)?;
            Ok(g)
        })
    });
    match r {
        Ok(g) => {
            *CACHE.lock().unwrap() = Some((key, g));
            let v = event(l, true);
            *last.lock().unwrap() = Some(v.clone());
            reply(id.load(Ordering::Relaxed), v);
        }
        Err(e) if e == CANCELLED => {}
        Err(e) => {
            let v = json!({"error": aqr_ipc::wire::error(kind::FAILED, e), "done": true});
            *last.lock().unwrap() = Some(v.clone());
            reply(id.load(Ordering::Relaxed), v);
        }
    }
}

/// The inventory at work, or done last.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Request `inventory` `v`, numbered `q`.
pub fn request(q: u64, v: &Value) {
    let a = match ipc::args_of(v) {
        Ok(a) => a,
        Err(e) => return ipc::fail(q, kind::BAD_REQUEST, format!("inventory: {e}")),
    };
    let key = Key::new(&a, Inputs::of(&a));
    let mut running = RUNNING.lock().unwrap();
    match running.take() {
        Some(r) if r.key == key => {
            r.adopt(q);
            *running = Some(r);
        }
        other => {
            if let Some(r) = other {
                r.stop();
            }
            *running = Some(Running::start(q, a));
        }
    }
}

/// Inventory `of` cancelled, where it is the one at work.
pub fn cancel(of: u64) {
    let mut running = RUNNING.lock().unwrap();
    if running.as_ref().is_some_and(|r| r.id.load(Ordering::Relaxed) == of) {
        running.take().unwrap().stop();
    }
}

/// Before the run of `a`: the inventory of the same waited for (it is the work to be done, taken
/// by [`cached`]), another stopped.
pub fn take(a: &Args) {
    let Some(r) = RUNNING.lock().unwrap().take() else { return };
    if r.key == Key::new(a, Inputs::of(a)) {
        _ = r.handle.join();
    } else {
        r.stop();
    }
}
