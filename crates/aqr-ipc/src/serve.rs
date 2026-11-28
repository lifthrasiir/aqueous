//! `aqr --ipc` serving archives (`man/aqripc.7`; `crates/aqr/src/serve.rs` tells the
//! requests), as a frontend browsing them speaks to it: `aqr` run ([`Served::start`]), requests
//! sent from any thread and each waited for by its number, the replies read by a thread of their
//! own.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::wire;

/// The most bytes of a blob, the hello's: a read longer comes in several replies.
pub const CHUNK: usize = wire::CHUNK;

/// Why a request was not answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `aqr` is gone (it ended, or failed to start): what it had open is gone with it.
    Gone,
    /// It failed: of what kind (`wire::kind`), and its message.
    Failed { kind: String, message: String },
}

impl Error {
    /// Whether it failed of `kind`.
    pub fn is(&self, kind: &str) -> bool {
        matches!(self, Error::Failed { kind: k, .. } if k == kind)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Gone => f.write_str("aqr --ipc is gone"),
            Error::Failed { message, .. } => f.write_str(message),
        }
    }
}

/// A reply: its line, and its blob.
pub type Answer = (Value, Vec<u8>);

/// `aqr --ipc` at work.
pub struct Served {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    /// By request, where its replies go.
    pending: Arc<Mutex<HashMap<u64, Sender<Answer>>>>,
    next: AtomicU64,
    gone: Arc<AtomicBool>,
}

impl Served {
    /// `aqr --ipc` (the program at `aqr`, with `args` before) started and greeted, of no console
    /// (none made for it).
    pub fn start(aqr: &Path, args: &[&str]) -> std::io::Result<Served> {
        let mut cmd = Command::new(aqr);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        cmd.args(args).arg("--ipc").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let mut stdin = child.stdin.take().expect("piped");
        let mut out = BufReader::new(child.stdout.take().expect("piped"));
        let greeted = (|| {
            // (the largest chunk: reads of a mount are long, a shell extension's of a chunk each)
            wire::write(&mut stdin, &json!({"hello": wire::VERSION, "chunk": wire::CHUNK_MAX}), &[])?;
            let hello = wire::read(&mut out)?.map(|(v, _)| v).unwrap_or(Value::Null);
            match hello["hello"].as_u64() {
                Some(v) if v >= wire::VERSION => Ok(()),
                _ => Err(std::io::Error::other(format!("aqr --ipc: no hello ({hello})"))),
            }
        })();
        if let Err(e) = greeted {
            _ = child.kill();
            _ = child.wait();
            return Err(e);
        }
        let pending: Arc<Mutex<HashMap<u64, Sender<Answer>>>> = Arc::default();
        let gone = Arc::new(AtomicBool::new(false));
        {
            let (pending, gone) = (pending.clone(), gone.clone());
            std::thread::Builder::new().name("aqr-ipc".into()).spawn(move || read(out, &pending, &gone))?;
        }
        Ok(Served { child: Mutex::new(child), stdin: Mutex::new(Some(stdin)), pending, next: AtomicU64::new(1), gone })
    }

    /// Whether `aqr` is gone: every request fails ([`Error::Gone`]).
    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    /// `aqr` killed (as one hung is): gone, every request waiting failing.
    pub fn kill(&self) {
        _ = self.child.lock().unwrap().kill();
    }

    /// Request `v` (its `op` and fields) sent, numbered here: where its replies come, each but
    /// the last with `more`.
    pub fn send(&self, mut v: Value) -> Result<Receiver<Answer>, Error> {
        let q = self.next.fetch_add(1, Ordering::Relaxed);
        v["q"] = q.into();
        let (to, from) = channel();
        self.pending.lock().unwrap().insert(q, to);
        let mut stdin = self.stdin.lock().unwrap();
        let sent = match stdin.as_mut() {
            Some(i) if !self.gone() => wire::write(i, &v, &[]).is_ok(),
            _ => false,
        };
        if !sent {
            self.pending.lock().unwrap().remove(&q);
            return Err(Error::Gone);
        }
        Ok(from)
    }

    /// The reply to request `v` (of one reply): its line, or its error.
    pub fn ask(&self, v: Value) -> Result<Value, Error> {
        let (v, _) = answered(self.send(v)?.recv())?;
        Ok(v)
    }

    /// The entries of `dir` of archive `handle`, of all the replies.
    pub fn list(&self, handle: u64, dir: &str) -> Result<Vec<Value>, Error> {
        self.list_with(handle, dir, false)
    }

    /// [`Served::list`], with what `-t -v` tells too where `verbose` (all of the archive's blocks
    /// read for it, once).
    pub fn list_with(&self, handle: u64, dir: &str, verbose: bool) -> Result<Vec<Value>, Error> {
        let from = self.send(json!({"op": "list", "archive": handle, "dir": dir, "verbose": verbose}))?;
        let mut all = Vec::new();
        loop {
            let (v, _) = answered(from.recv())?;
            if let Some(e) = v["entries"].as_array() {
                all.extend(e.iter().cloned());
            }
            if v["more"] != true {
                return Ok(all);
            }
        }
    }

    /// At most `len` bytes of file `path` of archive `handle` from `offset` (fewer only at its
    /// end), of all the replies.
    pub fn read(&self, handle: u64, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let from = self.send(json!({"op": "read", "archive": handle, "path": path, "offset": offset, "len": len}))?;
        let mut all = Vec::new();
        loop {
            let (v, d) = answered(from.recv())?;
            if all.is_empty() {
                all = d;
            } else {
                all.extend_from_slice(&d);
            }
            if v["more"] != true {
                return Ok(all);
            }
        }
    }
}

/// A reply received, or why there is none.
fn answered(r: Result<Answer, std::sync::mpsc::RecvError>) -> Result<Answer, Error> {
    match r {
        Err(_) => Err(Error::Gone),
        Ok((v, _)) if v.get("error").is_some() => Err(Error::Failed {
            kind: v["error"]["kind"].as_str().unwrap_or(wire::kind::FAILED).to_owned(),
            message: v["error"]["message"].as_str().unwrap_or("").to_owned(),
        }),
        Ok(a) => Ok(a),
    }
}

impl Drop for Served {
    /// Its standard input closed, for it to end; killed where it does not at once.
    fn drop(&mut self) {
        drop(self.stdin.lock().unwrap().take());
        let mut child = self.child.lock().unwrap();
        for _ in 0..50 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        _ = child.kill();
        _ = child.wait();
    }
}

/// Reads the replies, each to its request's channel (kept while it has `more`), until the
/// program is gone: then every request waiting fails.
fn read(mut out: impl std::io::BufRead, pending: &Mutex<HashMap<u64, Sender<Answer>>>, gone: &AtomicBool) {
    while let Ok(Some((v, blob))) = wire::read(&mut out) {
        let Some(r) = v["r"].as_u64() else { continue };
        let mut p = pending.lock().unwrap();
        let to = match v["more"] == true {
            true => p.get(&r).cloned(),
            false => p.remove(&r),
        };
        drop(p);
        if let Some(to) = to {
            _ = to.send((v, blob));
        }
    }
    gone.store(true, Ordering::SeqCst);
    pending.lock().unwrap().clear();
}
