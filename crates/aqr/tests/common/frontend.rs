//! `aqr --ipc` driven as the frontends drive it (`man/aqripc.7`), its messages seen as the tests
//! check them: a run's replies as its events (`{"event":...}`), aqr's questions as `{"ask":N,...}`
//! (answered `{"answer":N,"value":...}`), an inventory's replies as `{"event":"inventory","id":N,
//! ...}`, `{"control":"cancel"|"finish"}` and a session's `inventory`/`begin` sent as the
//! requests they are now ([`send`]).

use std::ffi::OsStr;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};

use serde_json::{Value, json};

/// The number of the run's request.
pub const RUN: u64 = 1000;

/// The numbers of the cancels sent.
static CANCELS: AtomicU64 = AtomicU64::new(2000);

/// A message of `aqr` as the tests check it; `None` of what they do not see (the run's last reply,
/// `{"exit":N}`; the replies of a cancel).
pub fn seen(mut v: Value) -> Option<Value> {
    if v.get("hello").is_some() {
        return Some(v);
    }
    if v["op"] == "ask" {
        let q = v["q"].clone();
        let o = v.as_object_mut().unwrap();
        o.remove("q");
        o.remove("op");
        o.remove("job");
        o.insert("ask".into(), q);
        return Some(v);
    }
    let r = v["r"].as_u64()?;
    if r == RUN {
        if v.get("exit").is_some() {
            return None;
        }
        if let Some(m) = v["error"]["message"].as_str() {
            return Some(json!({"event": "error", "message": m}));
        }
        let o = v.as_object_mut().unwrap();
        o.remove("r");
        o.remove("more");
        return Some(v);
    }
    // (an inventory's)
    if v.get("files").is_some() || v.get("error").is_some() {
        let o = v.as_object_mut().unwrap();
        o.remove("r");
        o.remove("more");
        o.insert("event".into(), "inventory".into());
        o.insert("id".into(), r.into());
        if let Some(m) = v["error"]["message"].as_str().map(String::from) {
            v["error"] = m.into();
            v["done"] = true.into();
        }
        return Some(v);
    }
    None
}

/// `v`, a message of a frontend as the tests write it, as it is sent now.
pub fn now(v: &Value) -> Value {
    if v.get("hello").is_some() {
        let mut h = v.clone();
        h["hello"] = 2.into();
        return h;
    }
    if v.get("answer").is_some() {
        return json!({"r": v["answer"], "value": v["value"]});
    }
    match v["control"].as_str() {
        Some("cancel") | Some("finish") => {
            let q = CANCELS.fetch_add(1, Ordering::Relaxed);
            json!({"q": q, "op": "cancel", "of": RUN, "finish": v["control"] == "finish"})
        }
        Some("inventory") => {
            let mut r = json!({"q": v["id"], "op": "inventory"});
            if let Some(a) = v.get("args") {
                r["args"] = a.clone();
            }
            r
        }
        Some("begin") => {
            let mut r = json!({"q": RUN, "op": "run"});
            if let Some(a) = v.get("args") {
                r["args"] = a.clone();
            }
            r
        }
        _ => v.clone(),
    }
}

/// Sends `v` (as the tests write it) to `c`; nothing where it is gone.
pub fn send(c: &mut Child, v: &Value) {
    if let Some(i) = c.stdin.as_mut() {
        _ = writeln!(i, "{}", now(v)).and_then(|_| i.flush());
    }
}

/// Sends `bytes`, a line as the tests write it (parsed and sent as it is now where it is JSON,
/// else as it is: junk stays junk).
pub fn send_bytes(c: &mut Child, bytes: &[u8]) {
    let Some(i) = c.stdin.as_mut() else { return };
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => _ = writeln!(i, "{}", now(&v)).and_then(|_| i.flush()),
        Err(_) => _ = i.write_all(bytes).and_then(|_| i.flush()),
    }
}

/// The messages of `c`, as [`seen`], through a channel (read on a thread of their own).
pub fn messages(c: &mut Child) -> Receiver<Value> {
    let mut out = BufReader::new(c.stdout.take().unwrap());
    let (to, from) = channel();
    std::thread::spawn(move || {
        while let Ok(Some((v, _))) = aqr_ipc::wire::read(&mut out) {
            if let Some(v) = seen(v)
                && to.send(v).is_err()
            {
                return;
            }
        }
    });
    from
}

/// `aqr --ipc` in `dir` (where given), `-T N` of `args` before it too (the pool is of the command
/// line), greeted taking `answers` and its hello read; then the run of `args` (as `aqr` would be
/// run with them). The process and its messages.
pub fn start(dir: Option<&Path>, args: &[&OsStr], answers: &[&str], stderr: Stdio) -> (Child, Receiver<Value>) {
    start_of(Path::new(env!("CARGO_BIN_EXE_aqr")), dir, args, answers, stderr)
}

/// [`start`] of the program at `aqr`.
pub fn start_of(
    aqr: &Path,
    dir: Option<&Path>,
    args: &[&OsStr],
    answers: &[&str],
    stderr: Stdio,
) -> (Child, Receiver<Value>) {
    let mut cmd = Command::new(aqr);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    if let Some(i) = args.iter().position(|a| *a == "-T") {
        cmd.args(&args[i..(i + 2).min(args.len())]);
    }
    let mut c = cmd.arg("--ipc").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(stderr).spawn().unwrap();
    send(&mut c, &json!({"hello": 1, "answers": answers}));
    let rx = messages(&mut c);
    let hello = rx.recv().unwrap();
    assert_eq!(hello["hello"], 2, "{hello}");
    let args: Vec<Value> = args.iter().map(|a| aqr_ipc::wire::os_value(a)).collect();
    send(&mut c, &json!({"control": "begin", "args": args}));
    (c, rx)
}
