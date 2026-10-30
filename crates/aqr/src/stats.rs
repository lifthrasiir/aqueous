//! What `--stats` tells of each archive made or decoded: the files by kind (how many, how
//! large, archived to how much, and the time they took), and where the time went, from the
//! threads sampled as it ran ([`aqr_common::trace`]).
//!
//! - `--stats`: on standard error, a line of the whole, and what stands out: the stretches the
//!   pool was barely used in and what ran then, the files that took long by themselves, the
//!   workers waiting much for something, and the kinds of files slow for their bytes or
//!   compressed hardly at all.
//! - `--stats=FILE.json`: that too, and everything sampled, as JSON (every archive of the run
//!   one of `runs`; below).
//! - `--stats=FILE.html`: that too, and a page of the same JSON with the script that shows it
//!   (`stats.html`): the threads over time, the heap by what allocated it, the time by
//!   label and the files by kind. The page opens other files of the JSON as well, and is opened
//!   in the browser where standard error is a terminal.
//!
//! The JSON, an object of `version` (1) and `runs`, each:
//!
//! - `mode` (`compress`, `decompress` or `test`), `input`, `output`, `seconds`, `threads`,
//!   `read` and `written` (bytes), `overhead` (bytes of the archive but the files', where
//!   compressed);
//! - `contents`, by content (the item of the samples): `path`, `kind` (the format it starts
//!   like, `-` for none), `len`, and where compressed `archived` (bytes of the archive that are
//!   its), `same_as` (the content it is the same as) and `modules`;
//! - `trace`: `interval` (seconds between samples, as it ended), `workers`, `labels` (by id:
//!   name and whether of waiting), `stacks` (by id: label ids, the outermost first; 0 is none,
//!   idle), `threads` (name and whether a worker of the pool), and the samples by columns:
//!   `at` (seconds), `heap` and `rss` (bytes), `reserved` (workers reserved or running), `samples`
//!   (by sample, of each thread its stack and item, one after the other; -1 for a thread ended
//!   or no item) and `bytes` (by sample, label ids and the bytes of large blocks they hold, one
//!   after the other).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use aqr_common::trace::{self, DEAD, NO_ITEM, Trace};

use crate::{percent, size};

/// What `--stats` asked for.
pub struct Stats {
    /// Files to write (by their extension: JSON, else a page).
    outputs: Vec<PathBuf>,
    /// The runs so far, as JSON.
    runs: Mutex<Vec<String>>,
}

impl Stats {
    /// Of files `outputs` to write, each of them [`takes`].
    pub fn new(outputs: Vec<PathBuf>) -> Stats {
        Stats { outputs, runs: Mutex::default() }
    }

    /// Tells the outliers of `run` as it ends, and keeps it for the files.
    pub fn add(&self, run: Run) {
        eprint!("{}", outliers(&run));
        self.runs.lock().unwrap().push(json(&run));
    }

    /// Writes the files asked for, of the runs so far; opens a page where `open`.
    pub fn write(&self, open: bool) -> Result<(), String> {
        if self.outputs.is_empty() {
            return Ok(());
        }
        let data = format!("{{\"version\":1,\"runs\":[{}]}}", self.runs.lock().unwrap().join(","));
        for o in &self.outputs {
            let text = if is_json(o) { data.clone() } else { page(&data) };
            std::fs::write(o, text).map_err(|e| format!("{}: {e}", o.display()))?;
            eprintln!("stats written to {}", o.display());
            if open && is_html(o) {
                browse(o);
            }
        }
        Ok(())
    }
}

fn is_json(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

fn is_html(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

/// Whether `p` is a file `--stats=` writes.
pub fn takes(p: &Path) -> bool {
    is_json(p) || is_html(p)
}

/// An archive made or decoded, and what was sampled meanwhile.
pub struct Run {
    pub mode: &'static str,
    pub input: String,
    pub output: String,
    pub seconds: f64,
    pub threads: usize,
    pub read: u64,
    pub written: u64,
    pub overhead: Option<u64>,
    pub contents: Vec<Content>,
    pub trace: Trace,
}

/// A content of the archive.
#[derive(Debug, Clone, Default)]
pub struct Content {
    pub path: String,
    pub kind: &'static str,
    pub len: u64,
    pub archived: Option<f64>,
    pub same_as: Option<usize>,
    pub modules: Vec<String>,
}

/// The sampling of a run, until finished ([`trace::record`]).
pub fn record(stats: Option<&Stats>) -> Option<trace::Recording> {
    stats.and_then(|_| trace::record())
}

/// Of the contents decoded: the kind each starts like, and its bytes; nothing where `--stats`
/// was not asked for.
pub struct Seen(Vec<OnceLock<(&'static str, u64)>>);

impl Seen {
    pub fn new(contents: usize, on: bool) -> Seen {
        Seen((0..if on { contents } else { 0 }).map(|_| OnceLock::new()).collect())
    }

    /// Content `c` decoded as `d` (the start of it, of `len` bytes in all).
    pub fn saw(&self, c: usize, d: &[u8], len: u64) {
        if let Some(s) = self.0.get(c) {
            s.get_or_init(|| (aqr_compress::detect(d).map_or("-", |f| f.name()), len));
        }
    }

    /// The contents, of their `paths`.
    pub fn contents(self, paths: Vec<String>) -> Vec<Content> {
        self.0
            .into_iter()
            .zip(paths)
            .map(|(s, path)| {
                let (kind, len) = s.into_inner().unwrap_or(("-", 0));
                Content { path, kind, len, ..Content::default() }
            })
            .collect()
    }
}

// The outliers.

/// What a sample of a thread is.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Gone,
    Idle,
    /// Waiting, for the label (of the stack's top).
    Wait(u32),
    Busy,
}

struct View<'r> {
    run: &'r Run,
    t: &'r Trace,
    /// By sample, the seconds it stands for.
    dt: Vec<f64>,
}

impl<'r> View<'r> {
    fn new(run: &'r Run) -> View<'r> {
        let t = &run.trace;
        let s = &t.samples;
        let dt = (0..s.len())
            .map(|i| match s.get(i + 1) {
                Some(n) => n.at - s[i].at,
                None => t.interval.as_secs_f64().min((t.seconds - s[i].at).max(0.0)),
            })
            .collect();
        View { run, t, dt }
    }

    fn state(&self, stack: u32) -> State {
        if stack == DEAD {
            return State::Gone;
        }
        match self.t.stacks[stack as usize].last() {
            None => State::Idle,
            Some(&l) if self.t.labels[l as usize].1 => State::Wait(l),
            Some(_) => State::Busy,
        }
    }

    /// The labels of work of `stack`, outermost first.
    fn path(&self, stack: u32) -> String {
        let labels: Vec<&str> = self.t.stacks[stack as usize]
            .iter()
            .map(|&l| self.t.labels[l as usize])
            .filter(|l| !l.1)
            .map(|l| l.0)
            .collect();
        labels.join(" > ")
    }

    fn item(&self, i: u32) -> String {
        match self.run.contents.get(i as usize) {
            Some(c) => c.path.clone(),
            None => String::new(),
        }
    }

    /// The workers of the pool, by thread.
    fn workers(&self) -> Vec<bool> {
        self.t.threads.iter().map(|t| t.1).collect()
    }
}

/// The lines `--stats` tells of `run`.
fn outliers(run: &Run) -> String {
    let v = View::new(run);
    let t = &run.trace;
    let mut out = String::new();
    let workers = v.workers();
    let w = workers.iter().filter(|&&w| w).count().max(1);
    let (mut busy_time, mut wait_time, mut total) = (0.0, HashMap::<u32, f64>::new(), 0.0);
    // the peak of the heap, and when
    let (mut peak, mut peak_at, mut rss) = (0i64, None, 0u64);
    for (i, s) in t.samples.iter().enumerate() {
        total += v.dt[i] * w as f64;
        for (k, &(stack, _)) in s.threads.iter().enumerate() {
            if !workers[k] {
                continue;
            }
            match v.state(stack) {
                State::Busy => busy_time += v.dt[i],
                State::Wait(l) => *wait_time.entry(l).or_default() += v.dt[i],
                _ => {}
            }
        }
        if s.heap > peak {
            (peak, peak_at) = (s.heap, Some(i));
        }
        rss = rss.max(s.rss);
    }
    let holder = peak_at.and_then(|i| {
        let s = &t.samples[i];
        let &(l, n) = s.bytes.iter().max_by_key(|b| b.1)?;
        (n > 0).then(|| format!(", {} of it by {}", size(n as f64), t.labels[l as usize].0))
    });
    _ = writeln!(
        out,
        "stats: {:.2} s, {w} workers {} busy; heap peak {}{} at {:.1} s{}",
        run.seconds,
        percent(busy_time, total),
        size(peak.max(0) as f64),
        holder.unwrap_or_default(),
        peak_at.map_or(0.0, |i| t.samples[i].at),
        if rss > 0 { format!(", resident peak {}", size(rss as f64)) } else { String::new() }
    );
    let mut said = 0;
    let mut say = |out: &mut String, line: String| {
        said += 1;
        _ = writeln!(out, "  {line}");
    };

    // the pool barely used: a stretch of a quarter of the workers busy at most (one of a few)
    if w >= 2 {
        let narrow = (w / 4).max(1);
        let long = (0.05 * run.seconds).max(0.5);
        let mut stretches: Vec<(usize, usize)> = Vec::new();
        let mut from = None;
        let busy = |s: &trace::Sample| {
            s.threads.iter().enumerate().filter(|&(k, &(st, _))| workers[k] && v.state(st) == State::Busy).count()
        };
        for (i, s) in t.samples.iter().enumerate() {
            match (busy(s) <= narrow, from) {
                (true, None) => from = Some(i),
                (false, Some(f)) => {
                    stretches.push((f, i));
                    from = None;
                }
                _ => {}
            }
        }
        if let Some(f) = from {
            stretches.push((f, t.samples.len()));
        }
        let seconds = |&(a, b): &(usize, usize)| v.dt[a..b].iter().sum::<f64>();
        let mut stretches: Vec<_> = stretches.into_iter().filter(|r| seconds(r) >= long).collect();
        stretches.sort_by(|a, b| seconds(b).total_cmp(&seconds(a)));
        for r in stretches.iter().take(5) {
            // what ran then, the longest
            let mut what: HashMap<(String, u32), f64> = HashMap::new();
            let mut busy_sum = 0.0;
            for i in r.0..r.1 {
                let s = &t.samples[i];
                for (k, &(st, item)) in s.threads.iter().enumerate() {
                    if v.state(st) == State::Busy {
                        *what.entry((v.path(st), item)).or_default() += v.dt[i];
                        busy_sum += if workers[k] { v.dt[i] } else { 0.0 };
                    }
                }
            }
            let secs = seconds(r);
            let ran = match what.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
                Some(((path, item), _)) => {
                    let of = if item == NO_ITEM { String::new() } else { format!(" of {}", v.item(item)) };
                    format!("mostly {path}{of}")
                }
                None => "nothing on the pool".into(),
            };
            let end = t.samples.get(r.1).map_or(t.seconds, |s| s.at);
            say(
                &mut out,
                format!(
                    "{:.1}–{:.1} s ({secs:.1} s): {:.1} of {w} workers busy, {ran}",
                    t.samples[r.0].at,
                    end,
                    busy_sum / secs.max(1e-9)
                ),
            );
        }
    }

    // files that took long by themselves: worked on (by any thread) a fifth of the time at least
    // (and half a second); by item, the seconds it was worked on and its thread-seconds
    let mut items: HashMap<u32, (f64, f64)> = HashMap::new();
    let mut of_sample = Vec::new();
    for (i, s) in t.samples.iter().enumerate() {
        of_sample.clear();
        for &(st, item) in &s.threads {
            if item != NO_ITEM && v.state(st) == State::Busy {
                items.entry(item).or_default().1 += v.dt[i];
                of_sample.push(item);
            }
        }
        of_sample.sort_unstable();
        of_sample.dedup();
        of_sample.iter().for_each(|it| items.get_mut(it).unwrap().0 += v.dt[i]);
    }
    let mut long: Vec<_> = items.iter().filter(|(_, e)| e.0 >= (0.2 * run.seconds).max(0.5) && e.1 >= 0.5).collect();
    long.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    for &(&item, &(worked, busy)) in long.iter().take(3) {
        let c = run.contents.get(item as usize);
        say(
            &mut out,
            format!(
                "{} ({}, {}): worked on for {worked:.1} s, {busy:.1} thread-seconds ({:.1} threads on average)",
                v.item(item),
                c.map_or("-", |c| c.kind),
                size(c.map_or(0, |c| c.len) as f64),
                busy / worked.max(1e-9)
            ),
        );
    }

    // the workers waiting much for something
    let mut waits: Vec<_> = wait_time.into_iter().filter(|&(_, s)| s >= 0.15 * total && s >= 0.5).collect();
    waits.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (l, s) in waits {
        say(&mut out, format!("workers waiting for {} {} of the time", t.labels[l as usize].0, percent(s, total)));
    }

    // the kinds: slow for their bytes, or barely compressed
    let mut kinds: HashMap<&str, (u64, f64, f64)> = HashMap::new();
    for c in run.contents.iter().filter(|c| c.same_as.is_none()) {
        let k = kinds.entry(c.kind).or_default();
        k.0 += c.len;
        k.1 += c.archived.unwrap_or(0.0);
    }
    for (&item, e) in &items {
        if let Some(c) = run.contents.get(item as usize) {
            kinds.entry(c.kind).or_default().2 += e.1;
        }
    }
    let bytes: u64 = kinds.values().map(|k| k.0).sum();
    let time: f64 = kinds.values().map(|k| k.2).sum();
    let mut kinds: Vec<_> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.2.total_cmp(&a.1.2));
    for (kind, (len, archived, secs)) in kinds {
        let (share, of_time) = (len as f64 / bytes.max(1) as f64, secs / time.max(1e-9));
        if of_time >= 0.1 && of_time >= 2.0 * share && secs >= 1.0 {
            say(
                &mut out,
                format!(
                    "{kind}: {} of the bytes took {} of the time ({:.1} MB/s a thread)",
                    percent(share, 1.0),
                    percent(of_time, 1.0),
                    len as f64 / 1e6 / secs.max(1e-9)
                ),
            );
        }
        if run.mode == "compress" && share >= 0.05 && archived >= 0.98 * len as f64 && len >= 1 << 20 {
            say(
                &mut out,
                format!(
                    "{kind}: {} of the bytes, compressed hardly at all ({} -> {})",
                    percent(share, 1.0),
                    size(len as f64),
                    size(archived)
                ),
            );
        }
    }
    if said == 0 {
        out += "  nothing stands out\n";
    }
    out
}

// The files.

/// `s` as a JSON string, `<` escaped (for a page to hold it in a script).
fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => _ = write!(out, "\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `x` as a JSON number (none for what is not finite).
fn number(out: &mut String, x: f64) {
    if x.is_finite() {
        _ = write!(out, "{}", (x * 1e6).round() / 1e6);
    } else {
        out.push_str("null");
    }
}

/// The items of `xs` as a JSON array, each written by `f`.
fn array<T>(out: &mut String, xs: impl IntoIterator<Item = T>, mut f: impl FnMut(&mut String, T)) {
    out.push('[');
    for (i, x) in xs.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        f(out, x);
    }
    out.push(']');
}

fn json(run: &Run) -> String {
    let mut o = String::new();
    o += "{\"mode\":";
    string(&mut o, run.mode);
    o += ",\"input\":";
    string(&mut o, &run.input);
    o += ",\"output\":";
    string(&mut o, &run.output);
    _ = write!(o, ",\"seconds\":");
    number(&mut o, run.seconds);
    _ = write!(o, ",\"threads\":{},\"read\":{},\"written\":{},\"overhead\":", run.threads, run.read, run.written);
    match run.overhead {
        Some(n) => _ = write!(o, "{n}"),
        None => o += "null",
    }
    o += ",\"contents\":";
    array(&mut o, &run.contents, |o, c| {
        *o += "{\"path\":";
        string(o, &c.path);
        *o += ",\"kind\":";
        string(o, c.kind);
        _ = write!(o, ",\"len\":{}", c.len);
        if let Some(a) = c.archived {
            *o += ",\"archived\":";
            number(o, a);
        }
        if let Some(s) = c.same_as {
            _ = write!(o, ",\"same_as\":{s}");
        }
        if !c.modules.is_empty() {
            *o += ",\"modules\":";
            array(o, &c.modules, |o, m| string(o, m));
        }
        o.push('}');
    });
    let t = &run.trace;
    o += ",\"trace\":{\"interval\":";
    number(&mut o, t.interval.as_secs_f64());
    _ = write!(o, ",\"workers\":{},\"labels\":", t.workers);
    array(&mut o, &t.labels, |o, &(name, wait)| {
        o.push('[');
        string(o, name);
        _ = write!(o, ",{wait}]");
    });
    o += ",\"stacks\":";
    array(&mut o, &t.stacks, |o, s| array(o, s, |o, l| _ = write!(o, "{l}")));
    o += ",\"threads\":";
    array(&mut o, &t.threads, |o, (name, worker)| {
        o.push('[');
        string(o, name);
        _ = write!(o, ",{worker}]");
    });
    let id = |x: u32| if x == DEAD || x == NO_ITEM { -1 } else { x as i64 };
    o += ",\"at\":";
    array(&mut o, &t.samples, |o, s| number(o, s.at));
    o += ",\"heap\":";
    array(&mut o, &t.samples, |o, s| _ = write!(o, "{}", s.heap.max(0)));
    o += ",\"rss\":";
    array(&mut o, &t.samples, |o, s| _ = write!(o, "{}", s.rss));
    o += ",\"reserved\":";
    array(&mut o, &t.samples, |o, s| _ = write!(o, "{}", s.reserved));
    o += ",\"samples\":";
    array(&mut o, &t.samples, |o, s| {
        array(o, s.threads.iter().flat_map(|&(st, it)| [id(st), id(it)]), |o, x| _ = write!(o, "{x}"))
    });
    o += ",\"bytes\":";
    array(&mut o, &t.samples, |o, s| {
        array(o, s.bytes.iter().flat_map(|&(l, n)| [l as i64, n]), |o, x| _ = write!(o, "{x}"))
    });
    o += "}}";
    o
}

/// The page of `data` (the JSON).
fn page(data: &str) -> String {
    const PAGE: &str = include_str!("stats.html");
    const HOLE: &str = "/*DATA*/null";
    assert!(PAGE.contains(HOLE), "the page has a place for the data");
    PAGE.replacen(HOLE, data, 1)
}

/// Opens page `p` in the browser, not waiting for it.
fn browse(p: &Path) {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    // (not as a verbatim path on Windows, which the shell does not take)
    let p = match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(s) => PathBuf::from(s),
        None => p,
    };
    let tool = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_vendor = "apple") {
        "open"
    } else {
        "xdg-open"
    };
    let opened = std::process::Command::new(tool)
        .arg(&p)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = opened {
        eprintln!("{} not opened: {e}", p.display());
    }
}
