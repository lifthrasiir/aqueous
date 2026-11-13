//! `--session`: this program started for a frontend before it knows what to do, as a compression
//! dialog does: it asks, while its user chooses the options, how many files the inputs are and
//! how big, and when the user is done it has the archive made, in this process, with what it
//! found meanwhile ([`ipc`]'s requests, `crates/aqr/src/ipc.rs` for the rest of the protocol).
//!
//! ```text
//! > {"control":"inventory","id":7,"args":["-c","-x","node_modules","--","a","b"]}
//! < {"event":"inventory","id":7,"files":120,"bytes":3400000,"walked":false,"done":false}
//! < {"event":"inventory","id":7,"files":1400,"bytes":61000000,"walked":true,"done":true}
//! > {"control":"begin","args":["-c","-l","7","-x","node_modules","-o","a.aqr","--","a","b"]}
//! ```
//!
//! - `inventory`: the arguments are what `aqr` is given to compress (only those that bear on
//!   what is gathered need be there: `-x`, `-L`, `-p`, `--rm`, and the inputs). It is answered
//!   by `inventory` events of its `id`, now and then while the files are listed (`files` found,
//!   `bytes` of those read so far, `walked`: whether all the directories are walked, the bytes
//!   still growing until `done`), the last with `done` true; or one with `error`. Nothing is
//!   known for a while after a request: the frontend shows what it has, or nothing. A new
//!   request replaces the one at work (its work stopped), unless it gathers the same: then
//!   it is the same work, told by the new `id`.
//! - `begin`: the arguments of the compression, as `aqr` would be run with them; from here this
//!   process does what they ask, as it would have without a session. What was gathered by the
//!   inventory of the same inputs, exclusions, `-L`, `-p` and `--rm` is taken for it ([`cached`]),
//!   or waited for where it is at work; anything else is gathered again.
//!
//! The pool of threads is of the size `aqr` had on start (`-T` of `begin` does not change it).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use aqr_ipc::{BEGIN, INVENTORY};
use serde_json::{Value, json};

use crate::{Args, Exclude, Gathered, Inputs, Mapper, gather, ipc, parse_from, view};

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
    if c.as_ref().is_some_and(|(k, _)| *k == Key::new(a, inputs)) {
        c.take().map(|(_, g)| g)
    } else {
        None
    }
}

/// The arguments of a request, parsed as `aqr` would have them.
fn request(v: &Value) -> Result<Args, String> {
    let args = v["args"].as_array().ok_or("--session: a request without args")?;
    let mut argv: Vec<std::ffi::OsString> = vec!["aqr".into(), "--ipc".into()];
    for a in args {
        argv.push(a.as_str().ok_or("--session: args are strings")?.into());
    }
    parse_from(argv)
}

/// An inventory at work.
struct Running {
    key: Key,
    /// The id the events are told by: the last request's.
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

    /// Told again by request `id`, as the same.
    fn adopt(&self, id: u64) {
        self.id.store(id, Ordering::Relaxed);
        if let Some(mut v) = self.last.lock().unwrap().clone() {
            v["id"] = id.into();
            ipc::tell(v);
        }
    }

    /// Stopped, and waited for.
    fn stop(self) {
        self.listing.cancel();
        _ = self.handle.join();
    }
}

fn event(id: &AtomicU64, l: &view::Listing, done: bool) -> Value {
    let (files, bytes, walked) = l.counts();
    json!({"event": "inventory", "id": id.load(Ordering::Relaxed), "files": files, "bytes": bytes, "walked": walked, "done": done})
}

/// Gathers `a`'s inputs as a compression would, telling how far it is; what is gathered is kept
/// for [`cached`].
fn inventory(key: Key, a: Args, id: &AtomicU64, l: &view::Listing, last: &Mutex<Option<Value>>) {
    let finished = AtomicBool::new(false);
    let r = std::thread::scope(|s| {
        let ticker = s.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_millis(250));
                if !finished.load(Ordering::Relaxed) {
                    ipc::tell(event(id, l, false));
                }
            }
        });
        let r = aqr_common::pool::run(|| -> Result<Gathered, String> {
            let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
            let mut g = Gathered::new(mapper, a.preserve);
            g.gone = a.rm.then(Vec::new);
            gather(Inputs::of(&a), &mut g, l, &a)?;
            Ok(g)
        });
        finished.store(true, Ordering::Relaxed);
        ticker.thread().unpark();
        r
    });
    match r {
        Ok(g) => {
            *CACHE.lock().unwrap() = Some((key, g));
            let v = event(id, l, true);
            *last.lock().unwrap() = Some(v.clone());
            ipc::tell(v);
        }
        Err(e) if e == CANCELLED => {}
        Err(e) => {
            let v = json!({"event": "inventory", "id": id.load(Ordering::Relaxed), "error": e, "done": true});
            *last.lock().unwrap() = Some(v.clone());
            ipc::tell(v);
        }
    }
}

/// Serves the frontend's requests until it begins: the arguments of the compression then.
pub fn serve(a: Args) -> Result<Args, String> {
    aqr_common::pool::init(a.opt.threads);
    let requests = ipc::requests().ok_or("--session: no requests to read")?;
    let mut running: Option<Running> = None;
    // (the reader ends the process where the frontend is gone)
    while let Ok(v) = requests.recv() {
        let id = v["id"].as_u64().unwrap_or(0);
        match v["control"].as_str() {
            Some(INVENTORY) => match request(&v) {
                Err(e) => ipc::tell(json!({"event": "inventory", "id": id, "error": e, "done": true})),
                Ok(a) => {
                    let key = Key::new(&a, Inputs::of(&a));
                    match running.take() {
                        Some(r) if r.key == key => {
                            r.adopt(id);
                            running = Some(r);
                        }
                        other => {
                            if let Some(r) = other {
                                r.stop();
                            }
                            running = Some(Running::start(id, a));
                        }
                    }
                }
            },
            Some(BEGIN) => {
                let a = request(&v)?;
                if let Some(r) = running.take() {
                    // (the same gathering is waited for: it is the work to be done)
                    if r.key == Key::new(&a, Inputs::of(&a)) {
                        _ = r.handle.join();
                    } else {
                        r.stop();
                    }
                }
                return Ok(a);
            }
            _ => {}
        }
    }
    Err("--session: the frontend is gone".into())
}
