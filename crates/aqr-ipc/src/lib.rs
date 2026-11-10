//! The protocol of `aqr --ipc` (`crates/aqr/src/ipc.rs` tells it), as the frontends speak it:
//! `aqr` run on the paths given ([`command`]), what it tells taken into a [`State`] by threads
//! reading it ([`Job::read_out`], [`Job::read_err`]), questions answered and the work stopped
//! ([`Job::send`]). What is shown, and how, is the frontend's (aqr-win's window, aqr-mac's
//! Finder progress).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

/// The version of the protocol.
pub const VERSION: u64 = 1;

/// The kinds of question asked, which a frontend answers where its hello says so.
pub mod ask {
    /// Whether an output there already is written over (`path`).
    pub const OVERWRITE: &str = "overwrite";
    /// The passphrase (`path`, `mode`, `again`, `confirm`).
    pub const PASSPHRASE: &str = "passphrase";
}

/// The control that stops the work, undoing what was done to its output.
pub const CANCEL: &str = "cancel";

/// The control that stops the work keeping what was done: an archive being written ended as
/// it is, of the files written (and the work cancelled where it cannot be: `aqr` tells the
/// controls it takes in its hello).
pub const FINISH: &str = "finish";

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

    /// What it is doing to archive `name` (shown; none: "Compressing…").
    pub fn doing(self, name: &str) -> String {
        use aqr_l10n::common as c;
        match (self, name) {
            (Verb::Compress, "") => c::compressing().into(),
            (Verb::Extract, "") => c::extracting().into(),
            (Verb::Test, "") => c::testing().into(),
            (Verb::Compress, n) => c::compressing_name(n),
            (Verb::Extract, n) => c::extracting_name(n),
            (Verb::Test, n) => c::testing_name(n),
        }
    }

    /// What is told where it failed on archive `name` (none: before any was begun).
    pub fn failed(self, name: &str) -> String {
        use aqr_l10n::common as c;
        match (self, name) {
            (Verb::Compress, "") => c::could_not_compress().into(),
            (Verb::Extract, "") => c::could_not_extract().into(),
            (Verb::Test, "") => c::could_not_test().into(),
            (Verb::Compress, n) => c::could_not_compress_name(n),
            (Verb::Extract, n) => c::could_not_extract_name(n),
            (Verb::Test, n) => c::could_not_test_name(n),
        }
    }

    /// `aqr`'s option of it.
    fn flag(self) -> &'static str {
        match self {
            Verb::Compress => "-c",
            Verb::Extract => "-d",
            Verb::Test => "-K",
        }
    }
}

/// `aqr`: `AQR`, else the one beside this program, else the one on `PATH`.
pub fn aqr() -> PathBuf {
    if let Some(p) = std::env::var_os("AQR") {
        return p.into();
    }
    let name = format!("aqr{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name(&name));
    beside
        .filter(|p| p.is_file())
        .unwrap_or_else(|| name.into())
}

/// The archive of several `paths` (of a folder, as a file manager gives them) in `format` (its
/// extension): named after the folder, in it.
pub fn archive_of(paths: &[PathBuf], format: &str) -> PathBuf {
    let dir = paths[0].parent().unwrap_or(Path::new("."));
    let name = dir
        .file_name()
        .map_or_else(|| "Archive".into(), |n| n.to_string_lossy().into_owned());
    dir.join(format!("{name}.{format}"))
}

/// The command running `aqr` (the program at that path, as [`aqr()`] finds it) on `paths` (not
/// empty; sorted here) with `--ipc`, its standard streams piped (`-P` where `encrypt`;
/// compressing, into an archive of `format`, `aqr` or another that `aqr --format` takes); and
/// the number of archives it makes or decodes.
pub fn command(
    aqr: &Path,
    verb: Verb,
    mut paths: Vec<PathBuf>,
    encrypt: bool,
    format: &str,
) -> (Command, usize) {
    paths.sort();
    let mut cmd = Command::new(aqr);
    cmd.arg("--ipc").arg(verb.flag());
    if encrypt {
        cmd.arg("-P");
    }
    if verb == Verb::Compress && format != "aqr" {
        cmd.arg("--format").arg(format);
    }
    // (every path an archive: several of them selected; a single entry at the top beside the
    // archive, else a directory of its name)
    if verb != Verb::Compress {
        cmd.arg("--each");
    }
    if verb == Verb::Extract {
        cmd.arg("--auto-dir");
    }
    let count = match verb {
        Verb::Compress if paths.len() > 1 => {
            cmd.arg("-o").arg(archive_of(&paths, format));
            1
        }
        _ => paths.len(),
    };
    cmd.arg("--").args(&paths);
    if let Some(dir) = paths[0].parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (cmd, count)
}

/// A journal point of an archive (`aqr --history --json`): a version of it that was named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    /// Its number (from 1), which `--at #N` takes.
    pub number: u64,
    /// When it was named, in local time as `aqr` shows it (`2026-11-01 12:00:00`).
    pub shown: String,
    pub label: String,
}

impl Point {
    /// The point as a list shows it: `#3  2026-11-01 12:00:00  label`.
    pub fn row(&self) -> String {
        format!("#{}  {}  {}", self.number, self.shown, self.label)
    }
}

/// The points of `archive`, as the `aqr` at that path tells them (none: it has none).
pub fn points(aqr: &Path, archive: &Path) -> Result<Vec<Point>, String> {
    let out = Command::new(aqr)
        .args(["--history", "--json", "--"])
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| aqr_l10n::common::not_run(aqr.to_string_lossy(), e))?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    parse_points(&String::from_utf8_lossy(&out.stdout))
}

/// The points of the line of JSON `aqr --history --json` writes.
fn parse_points(line: &str) -> Result<Vec<Point>, String> {
    let v: Value = serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
    let list = v["points"].as_array().ok_or("no points told")?;
    Ok(list
        .iter()
        .filter_map(|p| {
            Some(Point {
                number: p["number"].as_u64()?,
                shown: p["shown"].as_str()?.to_owned(),
                label: p["label"].as_str()?.to_owned(),
            })
        })
        .collect())
}

/// Names the state `archive` is in `label` (`aqr --tag`, `{today}` and `{now}` filled in by it):
/// what `aqr` told, or why it could not.
pub fn tag(aqr: &Path, archive: &Path, label: &str) -> Result<String, String> {
    let out = Command::new(aqr)
        .arg("--tag")
        .arg(label)
        .arg("--")
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| aqr_l10n::common::not_run(aqr.to_string_lossy(), e))?;
    match out.status.success() {
        true => Ok(last_line(&out.stderr)),
        false => Err(last_line(&out.stderr)),
    }
}

/// The last line of `aqr`'s standard error, without its `aqr: `.
fn last_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    line.strip_prefix("aqr: ").unwrap_or(line).to_owned()
}

/// A name for the folder that version `point` of `archive` is extracted into, beside the archive:
/// its name without the extension, the label in parentheses (the number where it has none); what
/// a file name cannot hold is `_`.
pub fn version_folder(archive: &Path, point: &Point) -> PathBuf {
    let stem = archive
        .file_stem()
        .map_or_else(|| "Archive".into(), |s| s.to_string_lossy().into_owned());
    let label: String = point
        .label
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let label = label.trim_end_matches(['.', ' ']);
    let label = match label.is_empty() {
        true => format!("#{}", point.number),
        false => label.to_owned(),
    };
    archive.with_file_name(format!("{stem} ({label})"))
}

/// The command extracting `archive` as it was at `point` into `into` (`aqr --ipc -d --at`), its
/// standard streams piped: one archive, as [`command`] has it.
pub fn command_at(aqr: &Path, archive: &Path, point: &Point, into: &Path) -> (Command, usize) {
    let mut cmd = Command::new(aqr);
    cmd.arg("--ipc")
        .arg("-d")
        .arg("--at")
        .arg(format!("#{}", point.number))
        .arg("-o")
        .arg(into)
        .arg("--")
        .arg(archive);
    if let Some(dir) = archive.parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (cmd, 1)
}

/// The lines of `aqr`'s standard error kept, the last.
const LOG: usize = 200;

/// What `aqr` told so far.
#[derive(Default)]
pub struct State {
    /// The archives begun, and of the last: its inputs, its output (none testing, and with
    /// `--auto-dir` until decided), and when it began.
    pub begun: usize,
    pub inputs: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    pub since: Option<Instant>,
    /// How far it is: the last `progress` event of the archive.
    pub progress: Option<Value>,
    /// The questions to answer, in order.
    pub questions: VecDeque<Value>,
    /// The `end` events with the names of their archives ([`State::name`]), the outputs skipped.
    pub ended: Vec<(String, Value)>,
    pub skipped: Vec<PathBuf>,
    pub error: Option<String>,
    pub stopped: bool,
    /// The user gave up a passphrase (`aqr`'s error for that is not told).
    pub cancelled: bool,
    /// Its standard output closed (all told).
    pub eof: bool,
    /// The last lines of its standard error.
    pub log: VecDeque<String>,
}

impl State {
    /// The first input of the archive at work, once one has begun.
    pub fn input(&self) -> Option<&Path> {
        self.inputs.first().map(PathBuf::as_path)
    }

    /// The name of the archive at work, as shown: its input, or its output where it has several
    /// inputs; empty before any has begun.
    pub fn name(&self) -> String {
        let shown = if self.inputs.len() == 1 {
            self.input()
        } else {
            self.output.as_deref()
        };
        shown.map(file_name).unwrap_or_default()
    }

    /// Takes a line `aqr` told.
    fn take(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if v.get("ask").is_some() {
            self.questions.push_back(v);
            return;
        }
        let path = |v: &Value| v.as_str().map(PathBuf::from);
        match v["event"].as_str() {
            Some("begin") => {
                self.begun += 1;
                self.inputs = v["inputs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(path)
                    .collect();
                self.output = path(&v["output"]);
                self.since = Some(Instant::now());
                self.progress = None;
            }
            // (with --auto-dir, what is extracted once decided)
            Some("output") => self.output = path(&v["output"]),
            Some("progress") => self.progress = Some(v),
            Some("end") => {
                let name = self.name();
                self.ended.push((name, v));
            }
            Some("skipped") => self.skipped.extend(path(&v["output"])),
            Some("error") => self.error = Some(v["message"].as_str().unwrap_or("").to_string()),
            Some("stopped") => self.stopped = true,
            _ => {}
        }
    }
}

/// `aqr` at work.
pub struct Job {
    pub verb: Verb,
    /// The archives to make or decode.
    pub count: usize,
    pub state: Mutex<State>,
    input: Mutex<Option<ChildStdin>>,
}

impl Job {
    /// The job of `aqr` reading `input` (the [`command`] of `verb` making or decoding `count`
    /// archives), told the frontend's hello: it answers both kinds of question ([`ask`]).
    pub fn new(verb: Verb, count: usize, input: Option<ChildStdin>) -> Job {
        let job = Job {
            verb,
            count,
            state: Mutex::default(),
            input: Mutex::new(input),
        };
        job.send(&json!({"hello": VERSION, "answers": [ask::OVERWRITE, ask::PASSPHRASE]}));
        job
    }

    /// Sends `aqr` `v` (nothing where it is gone).
    pub fn send(&self, v: &Value) {
        if let Some(i) = self.input.lock().unwrap().as_mut() {
            _ = writeln!(i, "{v}").and_then(|_| i.flush());
        }
    }

    /// Answers question `q` with `value`.
    pub fn answer(&self, q: &Value, value: Value) {
        self.send(&json!({"answer": q["ask"], "value": value}));
    }

    /// Asks `aqr` to stop, its output being written removed.
    pub fn cancel(&self) {
        self.send(&json!({"control": CANCEL}));
    }

    /// Asks `aqr` to stop, keeping what was done ([`FINISH`]).
    pub fn finish(&self) {
        self.send(&json!({"control": FINISH}));
    }

    /// Closes `aqr`'s standard input: it stops where it has not, and the readers end.
    pub fn close(&self) {
        *self.input.lock().unwrap() = None;
    }

    /// Takes what `aqr` tells on `out`, its standard output, until it closes.
    pub fn read_out(&self, out: impl Read) {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            self.state.lock().unwrap().take(&line);
        }
        self.state.lock().unwrap().eof = true;
    }

    /// Keeps the last lines of `err`, its standard error, until it closes.
    pub fn read_err(&self, err: impl Read) {
        for line in BufReader::new(err).lines() {
            let Ok(line) = line else { break };
            let mut s = self.state.lock().unwrap();
            if s.log.len() == LOG {
                s.log.pop_front();
            }
            s.log.push_back(line);
        }
    }

    /// The share of the work done, of the archive at work.
    pub fn share(&self) -> f64 {
        let s = self.state.lock().unwrap();
        let Some(p) = &s.progress else { return 0.0 };
        match self.verb {
            // (the analysis, recompressing, takes the most time mostly)
            Verb::Compress => {
                let probe = if p["probe"][1].as_u64() == Some(0) {
                    1.0
                } else {
                    done(p, "probe")
                };
                0.05 * probe + 0.65 * done(p, "analysis") + 0.3 * done(p, "compression")
            }
            Verb::Extract | Verb::Test => {
                0.3 * done(p, "decompressed") + 0.7 * done(p, "evaluated")
            }
        }
    }
}

/// The share done of `k` (`[done, total]`) of `progress` event `p`, 0 where it tells none.
pub fn done(p: &Value, k: &str) -> f64 {
    let (done, total) = (
        p[k][0].as_f64().unwrap_or(0.0),
        p[k][1].as_f64().unwrap_or(0.0),
    );
    if total > 0.0 {
        (done / total).min(1.0)
    } else {
        0.0
    }
}

/// The name of the file at `path`, as shown.
pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// What an `end` event `e` of a test of archive `name` tells (the name too, where `named`).
pub fn tested(name: &str, e: &Value, named: bool) -> String {
    let files = e["files"].as_u64().unwrap_or(0);
    let bytes = aqr_l10n::size(e["written"].as_u64().unwrap_or(0) as f64);
    let archive = aqr_l10n::size(e["read"].as_u64().unwrap_or(0) as f64);
    let time = aqr_l10n::duration(e["seconds"].as_f64().unwrap_or(0.0), 1);
    let text = aqr_l10n::common::tested(files, bytes, archive, time);
    if named {
        aqr_l10n::common::named(name, text)
    } else {
        text
    }
}

/// What is told of `count` archives (`name` the last, where one) all tested intact.
pub fn intact(name: &str, count: usize) -> String {
    if count == 1 {
        aqr_l10n::common::intact(name)
    } else {
        aqr_l10n::common::all_intact(count as u64)
    }
}

/// What failed of the job, ended with `status` (`None`: unknown), where it did: `aqr`'s error,
/// else how it ended where that was not a success.
pub fn failure(s: &State, status: Option<std::process::ExitStatus>) -> Option<String> {
    match (&s.error, status) {
        (Some(e), _) => Some(e.clone()),
        (None, Some(st)) if !st.success() => Some(aqr_l10n::common::ended_with(st)),
        (None, None) => Some(aqr_l10n::common::did_not_end().into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `aqr` tells of two archives tested, one of them failing.
    #[test]
    fn state_takes_events() {
        let mut s = State::default();
        for line in [
            r#"{"event":"begin","mode":"test","inputs":["d/a.aqr"],"output":null}"#,
            r#"{"event":"progress","decompressed":[5,10],"evaluated":[0,10]}"#,
            r#"{"event":"end","output":null,"files":3,"read":100,"written":2000,"seconds":0.5}"#,
            r#"{"ask":1,"kind":"passphrase","path":"d/b.aqr","mode":"test","again":false,"confirm":false}"#,
            "not json",
            r#"{"event":"begin","mode":"compress","inputs":["x","y"],"output":"d/xy.aqr"}"#,
            r#"{"event":"skipped","output":"d/xy.aqr"}"#,
            r#"{"event":"error","message":"no"}"#,
        ] {
            s.take(line);
        }
        assert_eq!(s.begun, 2);
        assert_eq!(s.name(), "xy.aqr");
        assert_eq!(s.input(), Some(Path::new("x")));
        assert!(s.progress.is_none());
        assert_eq!(s.ended.len(), 1);
        assert_eq!(s.ended[0].0, "a.aqr");
        assert_eq!(
            tested(&s.ended[0].0, &s.ended[0].1, true),
            "a.aqr: 3 files, 2.0 KB, in 100 B (0.5 s)"
        );
        assert_eq!(s.questions.len(), 1);
        assert_eq!(s.skipped, [PathBuf::from("d/xy.aqr")]);
        assert_eq!(s.error.as_deref(), Some("no"));
    }

    #[test]
    fn versions() {
        let line = r#"{"archive":"a.aqr","points":[{"number":1,"time":1,"shown":"2026-11-01 12:00:00","label":"first"},{"number":2,"time":2,"shown":"2026-11-02 12:00:00","label":""},{"bad":1}]}"#;
        let points = parse_points(line).unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].row(), "#1  2026-11-01 12:00:00  first");
        assert!(
            parse_points(r#"{"archive":"a.aqr","points":[]}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_points("nothing").is_err() && parse_points("{}").is_err());
        let a = Path::new("d/backup.aqr");
        assert_eq!(version_folder(a, &points[0]), Path::new("d/backup (first)"));
        assert_eq!(version_folder(a, &points[1]), Path::new("d/backup (#2)"));
        let odd = Point {
            number: 3,
            shown: String::new(),
            label: " a/b:c? 2026. ".into(),
        };
        assert_eq!(version_folder(a, &odd), Path::new("d/backup (a_b_c_ 2026)"));
        let (cmd, count) = command_at(Path::new("aqr"), a, &points[0], Path::new("d/out"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--ipc",
                "-d",
                "--at",
                "#1",
                "-o",
                "d/out",
                "--",
                "d/backup.aqr"
            ]
        );
        assert_eq!(count, 1);
        assert_eq!(
            last_line(
                b"x
aqr: not there

"
            ),
            "not there"
        );
    }

    #[test]
    fn share_of_progress() {
        let job = Job::new(Verb::Extract, 1, None);
        assert_eq!(job.share(), 0.0);
        job.state
            .lock()
            .unwrap()
            .take(r#"{"event":"progress","decompressed":[10,10],"evaluated":[5,10]}"#);
        assert!((job.share() - 0.65).abs() < 1e-9);
    }
}
