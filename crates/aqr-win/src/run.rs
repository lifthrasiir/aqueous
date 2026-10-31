//! The work: `aqr --ipc` run on the paths, what it tells taken into a [`Job`] that the window
//! shows ([`crate::dialog::progress`]), and what came of it shown at the end.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

use crate::dialog::{self, Icon};

#[derive(Clone, Copy, PartialEq, Eq)]
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

    /// What it is doing, as the window says.
    pub fn doing(self) -> &'static str {
        match self {
            Verb::Compress => "Compressing",
            Verb::Extract => "Extracting",
            Verb::Test => "Testing",
        }
    }
}

/// The lines of `aqr`'s standard error kept, the last.
const LOG: usize = 200;

/// What `aqr` told so far.
#[derive(Default)]
pub struct State {
    /// The archives begun, and of the last: the input (or the output, of several inputs)
    /// shown, and when.
    pub begun: usize,
    pub name: String,
    pub since: Option<Instant>,
    /// How far it is: the last `progress` event of the archive.
    pub progress: Option<Value>,
    /// The questions to answer, in order.
    pub questions: VecDeque<Value>,
    /// The `end` events with the names of their archives, the outputs skipped.
    pub ended: Vec<(String, Value)>,
    pub skipped: Vec<String>,
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
    pub state: Mutex<State>,
    input: Mutex<Option<ChildStdin>>,
}

impl Job {
    /// Sends `aqr` `v` (nothing where it is gone).
    pub fn send(&self, v: &Value) {
        if let Some(i) = self.input.lock().unwrap().as_mut() {
            _ = writeln!(i, "{v}").and_then(|_| i.flush());
        }
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
                let inputs = v["inputs"].as_array().map_or(0, Vec::len);
                let shown = if inputs == 1 { v["inputs"][0].as_str() } else { v["output"].as_str() };
                s.name = file_name(shown.unwrap_or(""));
                s.since = Some(Instant::now());
                s.progress = None;
            }
            Some("progress") => s.progress = Some(v),
            Some("end") => {
                let name = s.name.clone();
                s.ended.push((name, v));
            }
            Some("skipped") => s.skipped.push(text("output")),
            Some("error") => s.error = Some(text("message")),
            Some("stopped") => s.stopped = true,
            _ => {}
        }
    }
}

/// The name of the file at `path`, as shown.
pub fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// `aqr.exe`: `AQR`, else the one beside this program, else the one on `PATH`.
fn aqr() -> PathBuf {
    if let Some(p) = std::env::var_os("AQR") {
        return p.into();
    }
    let beside = std::env::current_exe().ok().map(|e| e.with_file_name("aqr.exe"));
    beside.filter(|p| p.is_file()).unwrap_or_else(|| "aqr.exe".into())
}

/// The archive of several `paths` (of a folder, as Explorer gives them): named after the
/// folder, in it.
fn archive_of(paths: &[PathBuf]) -> PathBuf {
    let dir = paths[0].parent().unwrap_or(Path::new("."));
    let name = dir.file_name().map_or_else(|| "Archive".into(), |n| n.to_string_lossy().into_owned());
    dir.join(format!("{name}.aqr"))
}

pub fn run(verb: Verb, mut paths: Vec<PathBuf>) -> ExitCode {
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
    /// `CREATE_NO_WINDOW`: no console for it.
    const NO_WINDOW: u32 = 0x0800_0000;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(NO_WINDOW);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let what = format!("{} is not run: {e}", cmd.get_program().to_string_lossy());
            dialog::message(Icon::Error, &format!("Could not {} anything", verb.name()), &what, None);
            return ExitCode::FAILURE;
        }
    };
    let job = Job { verb, count, state: Mutex::default(), input: Mutex::new(child.stdin.take()) };
    job.send(&json!({"hello": 1, "answers": ["overwrite"]}));
    let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let status = std::thread::scope(|s| {
        let job = &job;
        s.spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                job.take(&line);
            }
            job.state.lock().unwrap().eof = true;
        });
        s.spawn(move || {
            for line in BufReader::new(err).lines() {
                let Ok(line) = line else { break };
                let mut s = job.state.lock().unwrap();
                if s.log.len() == LOG {
                    s.log.pop_front();
                }
                s.log.push_back(line);
            }
        });
        let status = dialog::progress(job, &mut child);
        // (its input closed: it stops where it has not, and the readers end)
        *job.input.lock().unwrap() = None;
        status
    });
    let s = job.state.into_inner().unwrap();
    if s.stopped {
        return ExitCode::FAILURE;
    }
    let log: Vec<&str> = s.log.iter().map(String::as_str).collect();
    let log = log.join("\n");
    let failed = match (&s.error, status) {
        (Some(e), _) => Some(e.clone()),
        (None, Some(st)) if !st.success() => Some(format!("aqr ended with {st}.")),
        (None, None) => Some("aqr did not end.".into()),
        _ => None,
    };
    if let Some(e) = failed {
        let main = match s.name.as_str() {
            "" => format!("Could not {}", verb.name()),
            n => format!("Could not {} {n}", verb.name()),
        };
        dialog::message(Icon::Error, &main, &e, (!log.is_empty()).then_some(&log));
        return ExitCode::FAILURE;
    }
    if verb == Verb::Test {
        let lines: Vec<String> = s.ended.iter().map(|(n, e)| tested(n, e, count > 1)).collect();
        let main =
            if count == 1 { format!("{} is intact", s.name) } else { format!("The {count} archives are intact") };
        dialog::message(Icon::Information, &main, &lines.join("\n"), None);
    }
    ExitCode::SUCCESS
}

/// What an `end` event of a test of archive `name` tells (the name too, where `named`).
fn tested(name: &str, e: &Value, named: bool) -> String {
    let files = e["files"].as_u64().unwrap_or(0);
    let bytes = dialog::size(e["written"].as_u64().unwrap_or(0) as f64);
    let archive = dialog::size(e["read"].as_u64().unwrap_or(0) as f64);
    let s = if files == 1 { "" } else { "s" };
    let name = if named { format!("{name}: ") } else { String::new() };
    format!("{name}{files} file{s}, {bytes}, in {archive} ({:.1} s)", e["seconds"].as_f64().unwrap_or(0.0))
}
