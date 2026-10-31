//! `--ipc`: this program driven by another (a graphical frontend, such as `aqr-win`) through
//! its standard input and output, both ways at once, a JSON object a line.
//!
//! The frontend opens with a hello, which this program answers before doing anything else:
//!
//! ```text
//! > {"hello":1,"answers":["overwrite"]}
//! < {"hello":1,"controls":["cancel"]}
//! ```
//!
//! `hello` is the version of the protocol (this is 1); `answers`, the kinds of question the
//! frontend answers. Then this program tells, as it goes:
//!
//! - `{"event":"begin","mode":M,"inputs":[...],"output":O}`: an archive to make (`M` is
//!   `compress`) or to decode (`decompress`, `test`; `output` null testing).
//! - `{"event":"progress",...}`: how far it is, now and then (every quarter of a second or so,
//!   and as each batch is written). Compressing, `probe`, `analysis` and `compression`, each
//!   `[done, total]` (files for the probe, bytes otherwise: [`aqr_compress::Event`]);
//!   decoding, `decompressed` and `evaluated` (bytes: [`aqr_formula::aqr::Reached`]). Both
//!   with `file`, where one is being worked on: the one the longest, `{"path", "seconds"}`
//!   (with `len` and `done`, its bytes reached, where its recompressors tell).
//! - `{"event":"end","output":O,"files":N,"read":R,"written":W,"seconds":S}`: an archive done
//!   (`written` is what was decoded, decoding).
//! - `{"event":"skipped","output":O}`: an archive not made, as the frontend answered.
//! - `{"event":"error","message":...}`: the failure that the program ends with.
//! - `{"event":"stopped"}`: stopped as the frontend asked; the output being written removed
//!   ([`crate::stop`]).
//!
//! Where the options leave something open that the frontend answers, it is asked, and the work
//! that asks waits for the answer (the threads at other work go on):
//!
//! ```text
//! < {"ask":1,"kind":"overwrite","path":"x.aqr"}
//! > {"answer":1,"value":true}
//! ```
//!
//! Every question has an option that answers it before it is asked (`overwrite`: `-f`), and an
//! answer where nobody is asked (an error, as without `--ipc`): a frontend that does not take
//! a kind of question still works. The questions are numbered, as several may be open at once.
//!
//! The frontend may send `{"control":"cancel"}` at any time. Closing this program's standard
//! input cancels as well, so that it never outlives a frontend gone. What it does not know of
//! what it reads, it ignores.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use serde_json::{Value, json};

/// The version of the protocol.
const VERSION: u64 = 1;

/// The exit code where the frontend stopped the work (as shells tell a process stopped by
/// Ctrl-C).
const STOPPED: i32 = 130;

pub struct Ipc {
    /// The kinds of question that the frontend answers.
    answers: Vec<String>,
    next: AtomicU64,
    /// By question asked, where its answer goes.
    waiting: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
}

static IPC: OnceLock<Ipc> = OnceLock::new();

/// The frontend, where there is one (`--ipc`).
pub fn get() -> Option<&'static Ipc> {
    IPC.get()
}

/// Tells the frontend `v`, where there is one.
pub fn tell(v: Value) {
    if IPC.get().is_some() {
        send(&v);
    }
}

fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    // (nothing to do where the frontend is gone: its end of standard input closes too)
    _ = writeln!(out, "{v}").and_then(|_| out.flush());
}

/// Takes the frontend's hello and answers it, then reads what it sends on a thread of its own.
pub fn start() -> Result<(), String> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| format!("--ipc: {e}"))?;
    let hello: Value = serde_json::from_str(&line).map_err(|_| "--ipc: no hello from the frontend")?;
    match hello["hello"].as_u64() {
        Some(v) if v >= VERSION => {}
        Some(v) => return Err(format!("--ipc: version {v} of the protocol is not known")),
        None => return Err("--ipc: no hello from the frontend".into()),
    }
    let answers = hello["answers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from);
    let ipc = Ipc { answers: answers.collect(), next: AtomicU64::new(1), waiting: Mutex::default() };
    if IPC.set(ipc).is_err() {
        return Err("--ipc: started twice".into());
    }
    send(&json!({"hello": VERSION, "controls": ["cancel"]}));
    std::thread::Builder::new().name("ipc".into()).spawn(read).map_err(|e| format!("--ipc: {e}"))?;
    Ok(())
}

/// Reads what the frontend sends, until it cancels or is gone.
fn read() {
    let ipc = IPC.get().expect("started");
    for line in std::io::stdin().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(id) = v["answer"].as_u64() {
            if let Some(to) = ipc.waiting.lock().unwrap().remove(&id) {
                _ = to.send(v["value"].clone());
            }
        } else if v["control"] == "cancel" {
            break;
        }
    }
    send(&json!({"event": "stopped"}));
    crate::stop::now(STOPPED);
}

impl Ipc {
    /// The frontend's answer to question `kind` (with `q`'s fields), `None` where it does not
    /// answer that kind.
    pub fn ask(&self, kind: &str, mut q: Value) -> Option<Value> {
        if !self.answers.iter().any(|k| k == kind) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (to, answer) = mpsc::channel();
        self.waiting.lock().unwrap().insert(id, to);
        q["ask"] = id.into();
        q["kind"] = kind.into();
        send(&q);
        // (the reader never gone but with the process)
        answer.recv().ok()
    }
}
