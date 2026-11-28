//! `--ipc`: this program driven by another (a frontend: aqr-win, aqr-mac, aqr-mount) through its
//! standard input and output, by the protocol of `man/aqripc.7` (the frame: aqr-ipc's `wire`).
//!
//! After the hello, a reader thread takes the frontend's messages: the replies to this program's
//! questions ([`Ipc::ask`]), and its requests, served where they belong:
//!
//! - `run` (one, ever): its arguments go to the main thread ([`wait_run`]), which goes on as if
//!   run with them; what it tells as it goes ([`tell`], [`begin`], [`between`]) is the run's
//!   replies, and the last, `{"exit":CODE}`, comes as the process ends ([`ended`]).
//! - `inventory` ([`crate::session`]): the inputs gathered ahead of the run, which takes them.
//! - `open`, `list`, `stat`, `read`, `close`, `journal`, `progress`, `trim`
//!   ([`crate::serve`]): the archives browsed.
//! - `cancel`: of the run (as it was, or ended as it is where `finish`), of an inventory, of a
//!   read at work.
//!
//! Standard input closed cancels the run, if one is at work (as `cancel`), else ends the process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use aqr_ipc::wire::{self, kind};
use serde_json::{Value, json};

use crate::Args;

/// The exit code where the frontend stopped the work (as shells tell a process stopped by
/// Ctrl-C).
const STOPPED: i32 = 130;

/// The requests this program serves (the hello's `ops`).
const OPS: [&str; 11] =
    ["run", "inventory", "open", "list", "stat", "read", "close", "journal", "progress", "trim", "cancel"];

pub struct Ipc {
    /// The kinds of question that the frontend answers.
    answers: Vec<String>,
    /// The most bytes of a blob.
    pub chunk: usize,
    /// This program's requests (its questions), numbered.
    next: AtomicU64,
    /// By question asked, where its answer goes.
    waiting: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    /// The run's request, once it came.
    run: Mutex<Option<u64>>,
    /// The run's arguments, to the main thread.
    runs: Mutex<Option<mpsc::Sender<Args>>>,
    to_run: Mutex<Option<mpsc::Receiver<Args>>>,
    /// The run's last reply told.
    ended: AtomicBool,
}

static IPC: OnceLock<Ipc> = OnceLock::new();

/// Between archives ([`between`]: one's `end` or `skipped` told, the next not begun), and
/// whether the frontend cancelled (or went) then, for the next to stop as it begins.
struct Pause {
    between: bool,
    cancelled: bool,
}

static PAUSE: Mutex<Pause> = Mutex::new(Pause { between: false, cancelled: false });

aqr_common::zopt! {
    /// For tests: milliseconds waited after an archive's `end` or `skipped` is told, for a
    /// frontend's cancel to come between archives (or after the last one).
    static TEST_LINGER: Opt<u64> =
        Opt::value("test:linger", 0, "with --ipc, wait so many milliseconds after each archive is told done, for tests");
}

/// Sends message `v` and its blob, at once (nothing to do where the frontend is gone: its end of
/// standard input closes too).
pub fn send(v: &Value, blob: &[u8]) {
    let mut out = std::io::stdout().lock();
    _ = wire::write(&mut out, v, blob);
}

/// Replies `v` to request `r` (the last of them unless `more`), with its blob.
pub fn reply(r: u64, mut v: Value, more: bool, blob: &[u8]) {
    v["r"] = r.into();
    if more {
        v["more"] = true.into();
    }
    send(&v, blob);
}

/// Request `r` failed, as `kind` (aqr-ipc's `wire::kind`): its last reply.
pub fn fail(r: u64, kind: &str, message: impl Into<String>) {
    send(&json!({"r": r, "error": wire::error(kind, message)}), &[]);
}

/// Tells the frontend `v`, a reply of the run, where there is one.
pub fn tell(v: Value) {
    if let Some(ipc) = IPC.get()
        && let Some(r) = *ipc.run.lock().unwrap()
        && !ipc.ended.load(Ordering::SeqCst)
    {
        reply(r, v, true, &[]);
    }
}

/// The run's last reply, `{"exit":code}`, as the process ends (once).
pub fn ended(code: i32) {
    if let Some(ipc) = IPC.get()
        && let Some(r) = *ipc.run.lock().unwrap()
        && !ipc.ended.swap(true, Ordering::SeqCst)
    {
        reply(r, json!({"exit": code}), false, &[]);
    }
}

/// Tells the frontend `v`, the `end` or `skipped` of an archive: from then until the next
/// [`begin`], a cancel stops the next archive, not this one.
pub fn between(v: Value) {
    if IPC.get().is_some() {
        // (before it is told: the frontend may close its end as soon as it is)
        PAUSE.lock().unwrap().between = true;
        tell(v);
        std::thread::sleep(std::time::Duration::from_millis(TEST_LINGER.get()));
    }
}

/// The next archive's work begun: stopped now where the frontend cancelled between archives.
fn resume() {
    let mut p = PAUSE.lock().unwrap();
    if p.cancelled {
        tell(json!({"event": "stopped"}));
        crate::stop::now(STOPPED);
    }
    p.between = false;
}

/// Tells the frontend `v`, the `begin` of an archive, or stops where it cancelled between
/// archives.
pub fn begin(v: Value) {
    if IPC.get().is_some() {
        resume();
        tell(v);
    }
}

/// The frontend, where there is one (`--ipc`).
pub fn get() -> Option<&'static Ipc> {
    IPC.get()
}

/// Takes the frontend's hello and answers it, then reads what it sends on a thread of its own;
/// `a` are the options of the command line, the defaults of what is served.
pub fn start(a: &'static Args) -> Result<(), String> {
    let mut stdin = std::io::stdin().lock();
    let hello = match wire::read(&mut stdin) {
        Ok(Some((v, _))) => v,
        _ => return Err("--ipc: no hello from the frontend".into()),
    };
    match hello["hello"].as_u64() {
        Some(v) if v >= wire::VERSION => {}
        Some(v) => return Err(format!("--ipc: version {v} of the protocol is not known (this is {})", wire::VERSION)),
        None => return Err("--ipc: no hello from the frontend".into()),
    }
    drop(stdin);
    let answers = hello["answers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from);
    let chunk = hello["chunk"].as_u64().map_or(wire::CHUNK, |c| (c as usize).clamp(wire::CHUNK, wire::CHUNK_MAX));
    let (runs, to_run) = mpsc::channel();
    let ipc = Ipc {
        answers: answers.collect(),
        chunk,
        next: AtomicU64::new(1),
        waiting: Mutex::default(),
        run: Mutex::new(None),
        runs: Mutex::new(Some(runs)),
        to_run: Mutex::new(Some(to_run)),
        ended: AtomicBool::new(false),
    };
    if IPC.set(ipc).is_err() {
        return Err("--ipc: started twice".into());
    }
    crate::serve::init(a);
    send(&json!({"hello": wire::VERSION, "ops": OPS, "chunk": chunk}), &[]);
    std::thread::Builder::new().name("ipc".into()).spawn(read).map_err(|e| format!("--ipc: {e}"))?;
    Ok(())
}

/// The arguments of the run, once it comes (parsed as `aqr` would have them, `--ipc` with
/// them); `None` where the frontend is gone first.
pub fn wait_run() -> Option<Args> {
    let to_run = IPC.get()?.to_run.lock().unwrap().take()?;
    to_run.recv().ok()
}

/// The arguments of request `v` (`args`: strings, or paths of no text as `{"raw":...}`), parsed
/// as `aqr` would have them with `--ipc`.
pub fn args_of(v: &Value) -> Result<Args, String> {
    let args = v["args"].as_array().ok_or("a request without args")?;
    let mut argv: Vec<std::ffi::OsString> = vec!["aqr".into(), "--ipc".into()];
    for a in args {
        argv.push(wire::os_of(a).ok_or("args are strings (or {\"raw\":...})")?);
    }
    let a = crate::parse_from(argv)?;
    if a.inputs.is_empty() {
        return Err("no input".into());
    }
    Ok(a)
}

/// Request `run` `v`, numbered `q`: its arguments to the main thread, once.
fn run(ipc: &Ipc, q: u64, v: &Value) {
    let a = match args_of(v) {
        Ok(a) => a,
        Err(e) => return fail(q, kind::BAD_REQUEST, format!("run: {e}")),
    };
    let mut run = ipc.run.lock().unwrap();
    if run.is_some() {
        return fail(q, kind::BUSY, "run: a run is at work (one a process)");
    }
    *run = Some(q);
    drop(run);
    if let Some(to) = ipc.runs.lock().unwrap().take() {
        _ = to.send(a);
    }
}

/// The run cancelled (as the frontend's `cancel`, or as it is gone): ended as it is where
/// `finish`, else as it was.
fn cancel_run(finish: bool) {
    if finish {
        // (where nothing is to be ended as it is, cancelled)
        crate::stop::request(STOPPED);
        return;
    }
    // (between archives: the next one stopped as it begins, if any)
    {
        let mut p = PAUSE.lock().unwrap();
        if p.between {
            p.cancelled = true;
            return;
        }
    }
    // (an extraction ended as it is, the file being written removed: the writer tells `stopped`
    // and ends the process, as for `finish`)
    if crate::stop::is_extracting() {
        crate::stop::request(STOPPED);
        return;
    }
    tell(json!({"event": "stopped"}));
    crate::stop::now(STOPPED);
}

/// Reads what the frontend sends, until it is gone.
fn read() {
    let ipc = IPC.get().expect("started");
    let mut stdin = std::io::stdin().lock();
    while let Ok(Some((v, _blob))) = wire::read(&mut stdin) {
        if let Some(r) = v["r"].as_u64() {
            if let Some(to) = ipc.waiting.lock().unwrap().remove(&r) {
                _ = to.send(v);
            }
            continue;
        }
        let (Some(q), Some(op)) = (v["q"].as_u64(), v["op"].as_str()) else { continue };
        match op {
            "run" => run(ipc, q, &v),
            "inventory" => crate::session::request(q, &v),
            "cancel" => {
                let of = v["of"].as_u64();
                if of.is_some() && of == *ipc.run.lock().unwrap() {
                    reply(q, json!({}), false, &[]);
                    cancel_run(v["finish"] == true);
                    continue;
                }
                if let Some(of) = of {
                    crate::session::cancel(of);
                    crate::serve::cancel(of);
                }
                reply(q, json!({}), false, &[]);
            }
            "open" | "list" | "stat" | "read" | "close" | "journal" | "progress" | "trim" => {
                crate::serve::request(q, v);
            }
            _ => fail(q, kind::NOT_KNOWN, format!("{op}: a request not known")),
        }
    }
    // (the frontend gone: the run cancelled where there is one, else nothing left to do)
    drop(stdin);
    if ipc.run.lock().unwrap().is_some() && !ipc.ended.load(Ordering::SeqCst) {
        cancel_run(false);
        // (ended by the work, as it stops)
        return;
    }
    std::process::exit(0);
}

impl Ipc {
    /// Whether the frontend answers question `kind`.
    pub fn answers(&self, kind: &str) -> bool {
        self.answers.iter().any(|k| k == kind)
    }

    /// The frontend's answer to question `kind` (with `q`'s fields), `None` where it does not
    /// answer that kind.
    pub fn ask(&self, kind: &str, mut q: Value) -> Option<Value> {
        if !self.answers(kind) {
            return None;
        }
        // (of the next archive's work, where asked between archives: begun, as a cancel would leave
        // the question unanswered)
        resume();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (to, answer) = mpsc::channel();
        self.waiting.lock().unwrap().insert(id, to);
        q["q"] = id.into();
        q["op"] = "ask".into();
        q["kind"] = kind.into();
        if let Some(r) = *self.run.lock().unwrap() {
            q["job"] = r.into();
        }
        send(&q, &[]);
        // (the reader never gone but with the process)
        answer.recv().ok().map(|v| v["value"].clone())
    }
}
