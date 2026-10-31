//! The work: `aqr --ipc` run on the paths in a process of its own, what it tells taken into a
//! [`Job`] by threads reading it, which the app looks at on its timer ([`crate::app`]).
//!
//! As aqr-win's `run.rs` (`crates/aqr-win/src/run.rs`), but for the window.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verb {
    Compress,
    Extract,
    Test,
}

impl Verb {
    pub fn name(self) -> &'static str {
        match self {
            Verb::Compress => "compress",
            Verb::Extract => "extract",
            Verb::Test => "test",
        }
    }
}

/// The lines of `aqr`'s standard error kept, the last.
const LOG: usize = 200;

/// The time given `aqr` to stop before it is killed.
const STOP: Duration = Duration::from_secs(10);

/// What `aqr` told so far.
#[derive(Default)]
pub struct State {
    /// The archives begun, and of the last: its first input, its output (none testing), and
    /// when it began.
    pub begun: usize,
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub since: Option<Instant>,
    /// How far it is: the last `progress` event of the archive.
    pub progress: Option<Value>,
    /// The questions to answer, in order.
    pub questions: VecDeque<Value>,
    /// The `end` events with the first inputs of their archives.
    pub ended: Vec<(PathBuf, Value)>,
    pub error: Option<String>,
    pub stopped: bool,
    /// Its standard output closed (all told).
    pub eof: bool,
    /// The last lines of its standard error.
    pub log: VecDeque<String>,
}

pub struct Job {
    pub verb: Verb,
    /// The archives to make or decode.
    pub count: usize,
    /// The process of `aqr` (the name of the file it writes goes by it).
    pub pid: u32,
    pub state: Mutex<State>,
    input: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    /// When the stop was asked for.
    stopping: Mutex<Option<Instant>>,
}

/// `aqr`: `AQR`, else the one beside this program (in the bundle), else the one on `PATH`.
fn aqr() -> PathBuf {
    if let Some(p) = std::env::var_os("AQR") {
        return p.into();
    }
    let beside = std::env::current_exe().ok().map(|e| e.with_file_name("aqr"));
    beside.filter(|p| p.is_file()).unwrap_or_else(|| "aqr".into())
}

/// The archive of several `paths` (of a folder, as Finder gives them): named after the folder,
/// in it.
fn archive_of(paths: &[PathBuf]) -> PathBuf {
    let dir = paths[0].parent().unwrap_or(Path::new("."));
    let name = dir.file_name().map_or_else(|| "Archive".into(), |n| n.to_string_lossy().into_owned());
    dir.join(format!("{name}.aqr"))
}

impl Job {
    /// Runs `aqr` on `paths`, read on threads of their own.
    pub fn start(verb: Verb, mut paths: Vec<PathBuf>) -> Result<Arc<Job>, String> {
        paths.sort();
        let mut cmd = Command::new(aqr());
        cmd.arg("--ipc").arg(match verb {
            Verb::Compress => "-c",
            Verb::Extract => "-d",
            Verb::Test => "-t",
        });
        let count = match verb {
            Verb::Compress if paths.len() > 1 => {
                cmd.arg("-o").arg(archive_of(&paths));
                1
            }
            _ => paths.len(),
        };
        cmd.arg("--").args(&paths);
        if let Some(dir) = paths[0].parent().filter(|d| d.is_dir()) {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("{} is not run: {e}", cmd.get_program().display()))?;
        let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        let job = Arc::new(Job {
            verb,
            count,
            pid: child.id(),
            state: Mutex::default(),
            input: Mutex::new(child.stdin.take()),
            child: Mutex::new(child),
            stopping: Mutex::new(None),
        });
        job.send(&json!({"hello": 1, "answers": ["overwrite"]}));
        let j = job.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                j.take(&line);
            }
            j.state.lock().unwrap().eof = true;
        });
        let j = job.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines() {
                let Ok(line) = line else { break };
                let mut s = j.state.lock().unwrap();
                if s.log.len() == LOG {
                    s.log.pop_front();
                }
                s.log.push_back(line);
            }
        });
        Ok(job)
    }

    /// Sends `aqr` `v` (nothing where it is gone).
    pub fn send(&self, v: &Value) {
        if let Some(i) = self.input.lock().unwrap().as_mut() {
            _ = writeln!(i, "{v}").and_then(|_| i.flush());
        }
    }

    /// Asks `aqr` to stop (its output being written removed); it is killed where it has not
    /// in [`STOP`] ([`Job::done`]). Of any thread.
    pub fn stop(&self) {
        let mut stopping = self.stopping.lock().unwrap();
        if stopping.is_none() {
            *stopping = Some(Instant::now());
            self.send(&json!({"control": "cancel"}));
        }
    }

    /// What `aqr` ended with, once it has and all it told is taken; killed where it was asked to
    /// stop long ago.
    pub fn done(&self) -> Option<ExitStatus> {
        let mut child = self.child.lock().unwrap();
        if self.stopping.lock().unwrap().is_some_and(|t| t.elapsed() > STOP) {
            _ = child.kill();
        }
        if !self.state.lock().unwrap().eof {
            return None;
        }
        let st = child.try_wait().ok().flatten()?;
        // (its input closed, as it is gone)
        *self.input.lock().unwrap() = None;
        Some(st)
    }

    fn take(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let mut s = self.state.lock().unwrap();
        if v.get("ask").is_some() {
            s.questions.push_back(v);
            return;
        }
        let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match v["event"].as_str() {
            Some("begin") => {
                s.begun += 1;
                s.input = v["inputs"][0].as_str().unwrap_or("").into();
                s.output = v["output"].as_str().map(PathBuf::from);
                s.since = Some(Instant::now());
                s.progress = None;
            }
            Some("progress") => s.progress = Some(v),
            Some("end") => {
                let input = s.input.clone();
                s.ended.push((input, v));
            }
            Some("error") => s.error = Some(text("message")),
            Some("stopped") => s.stopped = true,
            _ => {}
        }
    }

    /// The share of the work done, of the archive at work.
    pub fn share(&self) -> f64 {
        let s = self.state.lock().unwrap();
        let Some(p) = &s.progress else { return 0.0 };
        let of = |k: &str| {
            let (done, total) = (p[k][0].as_f64().unwrap_or(0.0), p[k][1].as_f64().unwrap_or(0.0));
            if total > 0.0 { (done / total).min(1.0) } else { 0.0 }
        };
        match self.verb {
            // (the analysis, recompressing, takes the most time mostly)
            Verb::Compress => {
                let probe = if p["probe"][1].as_u64() == Some(0) { 1.0 } else { of("probe") };
                0.05 * probe + 0.65 * of("analysis") + 0.3 * of("compression")
            }
            Verb::Extract | Verb::Test => 0.3 * of("decompressed") + 0.7 * of("evaluated"),
        }
    }

    /// The file that Finder shows the archive at work on: the archive being written (under the
    /// name `aqr` writes it, `NAME.PID.part`, renamed once it is written), the folder being
    /// extracted into, or the archive being tested.
    pub fn shown(&self) -> Option<PathBuf> {
        let s = self.state.lock().unwrap();
        if s.begun == 0 {
            return None;
        }
        match (self.verb, &s.output) {
            (Verb::Compress, Some(o)) => {
                let mut name = o.file_name()?.to_owned();
                name.push(format!(".{}.part", self.pid));
                Some(o.with_file_name(name))
            }
            (Verb::Extract, Some(o)) => Some(o.clone()),
            _ => Some(s.input.clone()),
        }
    }
}

/// The name of the file at `path`, as shown.
pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Bytes of a size, for people (as `aqr` tells them).
pub fn size(n: f64) -> String {
    match n {
        n if n < 1e3 => format!("{n:.0} B"),
        n if n < 1e6 => format!("{:.1} KB", n / 1e3),
        n if n < 1e9 => format!("{:.2} MB", n / 1e6),
        n => format!("{:.2} GB", n / 1e9),
    }
}

/// What an `end` event of a test of archive `input` tells (its name too, where `named`).
pub fn tested(input: &Path, e: &Value, named: bool) -> String {
    let files = e["files"].as_u64().unwrap_or(0);
    let bytes = size(e["written"].as_u64().unwrap_or(0) as f64);
    let archive = size(e["read"].as_u64().unwrap_or(0) as f64);
    let s = if files == 1 { "" } else { "s" };
    let name = if named { format!("{}: ", file_name(input)) } else { String::new() };
    format!("{name}{files} file{s}, {bytes}, in {archive} ({:.1} s)", e["seconds"].as_f64().unwrap_or(0.0))
}
