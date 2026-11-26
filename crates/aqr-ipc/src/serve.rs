//! The protocol of `aqr --serve` (`crates/aqr/src/serve.rs` tells it), as a frontend browsing
//! archives speaks it: `aqr --serve` run ([`Served::start`]), requests sent from any thread and
//! each waited for by its id, the answers read by a thread of their own.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// The version of the protocol.
pub const VERSION: u64 = 1;

/// The most bytes a `read` is answered with.
pub const CHUNK: usize = 1 << 20;

/// Why a request was not answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `aqr --serve` is gone (it ended, or failed to start): what it had open is gone with it.
    Gone,
    /// It answered with an error: its message.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Gone => f.write_str("aqr --serve is gone"),
            Error::Failed(m) => f.write_str(m),
        }
    }
}

/// An answer: its line, and the bytes after it (of a `read`).
pub type Answer = (Value, Vec<u8>);

/// `aqr --serve` at work.
pub struct Served {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    /// By request, where its answers go.
    pending: Arc<Mutex<HashMap<u64, Sender<Answer>>>>,
    next: AtomicU64,
    gone: Arc<AtomicBool>,
}

impl Served {
    /// `aqr --serve` (the program at `aqr`, with `args` before) started and greeted, of no
    /// console (none made for it).
    pub fn start(aqr: &Path, args: &[&str]) -> std::io::Result<Served> {
        let mut cmd = Command::new(aqr);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        cmd.args(args).arg("--serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let mut stdin = child.stdin.take().expect("piped");
        let mut out = BufReader::new(child.stdout.take().expect("piped"));
        let greeted = (|| {
            writeln!(stdin, "{}", json!({"hello": VERSION}))?;
            stdin.flush()?;
            let mut l = String::new();
            out.read_line(&mut l)?;
            let hello: Value = serde_json::from_str(&l).map_err(std::io::Error::other)?;
            match hello["hello"].as_u64() {
                Some(v) if v >= VERSION => Ok(()),
                _ => Err(std::io::Error::other(format!("aqr --serve: no hello ({})", l.trim()))),
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
            std::thread::Builder::new().name("aqr-serve".into()).spawn(move || read(out, &pending, &gone))?;
        }
        Ok(Served { child: Mutex::new(child), stdin: Mutex::new(Some(stdin)), pending, next: AtomicU64::new(1), gone })
    }

    /// Whether `aqr --serve` is gone: every request fails ([`Error::Gone`]).
    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    /// `aqr --serve` killed (as one hung is): gone, every request waiting failing.
    pub fn kill(&self) {
        _ = self.child.lock().unwrap().kill();
    }

    /// Request `v` sent (its `id` given here): where its answers come.
    pub fn send(&self, mut v: Value) -> Result<Receiver<Answer>, Error> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        v["id"] = id.into();
        let (to, from) = channel();
        self.pending.lock().unwrap().insert(id, to);
        let mut stdin = self.stdin.lock().unwrap();
        let sent = match stdin.as_mut() {
            Some(i) if !self.gone() => writeln!(i, "{v}").and_then(|_| i.flush()).is_ok(),
            _ => false,
        };
        if !sent {
            self.pending.lock().unwrap().remove(&id);
            return Err(Error::Gone);
        }
        Ok(from)
    }

    /// The answer to request `v` (of one answer): its line, or its error.
    pub fn ask(&self, v: Value) -> Result<Value, Error> {
        let (v, _) = answered(self.send(v)?.recv())?;
        Ok(v)
    }

    /// The entries of `dir` of archive `handle`, of all the answers.
    pub fn list(&self, handle: u64, dir: &str) -> Result<Vec<Value>, Error> {
        self.list_with(handle, dir, false)
    }

    /// [`Served::list`], with what `-t -v` tells too where `verbose` (all of the archive's blocks
    /// read for it, once).
    pub fn list_with(&self, handle: u64, dir: &str, verbose: bool) -> Result<Vec<Value>, Error> {
        let from = self.send(json!({"list": handle, "dir": dir, "verbose": verbose}))?;
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

    /// At most `len` bytes of file `path` of archive `handle` from `offset` (at most [`CHUNK`]:
    /// fewer only at its end).
    pub fn read(&self, handle: u64, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let from = self.send(json!({"read": handle, "path": path, "offset": offset, "len": len}))?;
        Ok(answered(from.recv())?.1)
    }
}

/// An answer received, or why there is none.
fn answered(r: Result<Answer, std::sync::mpsc::RecvError>) -> Result<Answer, Error> {
    match r {
        Err(_) => Err(Error::Gone),
        Ok((v, _)) if v.get("error").is_some() => Err(Error::Failed(v["error"].as_str().unwrap_or("").to_owned())),
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

/// Reads the answers, each to its request's channel (kept while it has `more`), until the
/// program is gone: then every request waiting fails.
fn read(mut out: impl BufRead, pending: &Mutex<HashMap<u64, Sender<Answer>>>, gone: &AtomicBool) {
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(n) = out.read_line(&mut line) else { break };
        if n == 0 {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { break };
        let mut data = vec![0; v["data"].as_u64().unwrap_or(0) as usize];
        if out.read_exact(&mut data).is_err() {
            break;
        }
        let Some(id) = v["id"].as_u64() else { continue };
        let mut p = pending.lock().unwrap();
        let to = match v["more"] == true {
            true => p.get(&id).cloned(),
            false => p.remove(&id),
        };
        drop(p);
        if let Some(to) = to {
            _ = to.send((v, data));
        }
    }
    gone.store(true, Ordering::SeqCst);
    pending.lock().unwrap().clear();
}
