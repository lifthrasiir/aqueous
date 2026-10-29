//! What an archive being made shows on standard error: on a terminal a bar of how far it is,
//! and where asked, above it the log of the files, a batch at a time as they are written.
//!
//! The bar overlays the three phases, each of which is never ahead of the one before: the probe
//! (`-`, files looked at), the analysis (`=`, bytes recompressed) and the compression (`#`, bytes
//! compressed and verified). Below it, the file recompressed the longest, where that is long,
//! and how far it is where its recompressors tell.
//!
//! An archive being decoded has a bar of two ([`Decoding`]): the decompression (`=`, bytes of
//! the data blocks as they are in the archive) and the evaluation (`#`, bytes of the leaves'
//! outputs: the files).

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aqr_compress::{Event, Report};
use aqr_formula::aqr::Reached;

use crate::{Gathered, percent, size};

/// Columns that the bar's lines take at most (no more than a terminal has, mostly).
const WIDTH: usize = 79;

/// Cells of the bar.
const CELLS: usize = 32;

/// A file recompressed this long is shown below the bar.
const SLOW: Duration = Duration::from_secs(2);

pub struct View<'g> {
    g: &'g Gathered,
    /// Whether the bar is drawn (standard error a terminal), and the files logged.
    bar: bool,
    log: bool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    probe: (u64, u64),
    analysis: (u64, u64),
    compression: (u64, u64),
    /// The contents being recompressed, since when, and the bytes reached of each.
    working: HashMap<usize, Instant>,
    reached: HashMap<usize, u64>,
    /// Lines of the bar drawn last.
    drawn: usize,
    /// The solid blocks, sets and contents kept raw logged so far.
    logged: (usize, usize, usize),
}

impl<'g> View<'g> {
    pub fn new(g: &'g Gathered, bar: bool, log: bool) -> View<'g> {
        View { g, bar, log, state: Mutex::default() }
    }

    /// Whether the bar is drawn.
    pub fn bars(&self) -> bool {
        self.bar
    }

    pub fn tell(&self, e: Event) {
        let mut s = self.state.lock().unwrap();
        match e {
            Event::Probe { done, total } => s.probe = (done, total),
            Event::Analysis { done, total } => s.analysis = (done, total),
            Event::Compression { done, total } => s.compression = (done, total),
            Event::Started(c) => _ = s.working.entry(c).or_insert_with(Instant::now),
            Event::Ended(c) => {
                s.working.remove(&c);
                s.reached.remove(&c);
            }
            Event::Reached { content, done } => _ = s.reached.insert(content, done),
            Event::Written { contents, report } => {
                let lines = if self.log { self.log(contents, report, &mut s.logged) } else { Vec::new() };
                self.draw(&mut s, &lines);
            }
            _ => {}
        }
    }

    /// Draws the bar again (for the time a file has taken).
    pub fn tick(&self) {
        let mut s = self.state.lock().unwrap();
        self.draw(&mut s, &[]);
    }

    /// Writes `lines` of the log, then the bar in place of the one drawn last.
    fn draw(&self, s: &mut State, lines: &[String]) {
        let mut out = std::io::stderr().lock();
        if !self.bar {
            lines.iter().for_each(|l| _ = writeln!(out, "{l}"));
            return;
        }
        let mut text = String::new();
        if s.drawn > 0 {
            // (to the start of the bar's first line, and all after it cleared)
            text += &format!("\x1b[{}F\x1b[J", s.drawn);
        }
        lines.iter().for_each(|l| text += &format!("{l}\n"));
        let bar = bar(s, self.slowest(s));
        s.drawn = bar.len();
        bar.iter().for_each(|l| text += &format!("{l}\n"));
        _ = out.write_all(text.as_bytes());
        _ = out.flush();
    }

    /// The lines of the log of `contents`, written as of `r`, and the solid blocks, sets and
    /// contents kept raw that `r` has past those `logged`.
    fn log(&self, contents: &[usize], r: &Report, logged: &mut (usize, usize, usize)) -> Vec<String> {
        let path = |c: usize| self.g.contents[c].0.display().to_string();
        let (solid, sets, raw) = *logged;
        let mut solid_of: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, s) in r.solid.iter().enumerate().skip(solid) {
            s.contents.iter().for_each(|&c| solid_of.entry(c).or_default().push(i + 1));
        }
        let mut set_of = HashMap::new();
        for (i, s) in r.sets.iter().enumerate().skip(sets) {
            s.iter().for_each(|&c| _ = set_of.insert(c, i + 1));
        }
        let mut lines = Vec::new();
        for &c in contents {
            let f = &r.files[c];
            let len = f.len as f64;
            let what = match f.same_as {
                Some(s) => format!("same as {}", path(s)),
                None => {
                    let modules: Vec<String> = f.modules.iter().map(|m| m.to_string()).collect();
                    let mut what = format!(
                        "{}, {}, {} -> {} ({})",
                        f.format.unwrap_or("-"),
                        if modules.is_empty() { "kept as it is".into() } else { modules.join(" ") },
                        size(len),
                        size(f.archived),
                        percent(f.archived, len)
                    );
                    if let Some(s) = set_of.get(&c) {
                        what += &format!(", set #{s}");
                    }
                    if let Some(ids) = solid_of.get(&c) {
                        let ids: Vec<String> = ids.iter().map(|i| format!("#{i}")).collect();
                        what += &format!(", solid {}", ids.join(" "));
                    }
                    what
                }
            };
            lines.push(format!("{}: {what}", path(c)));
        }
        for (i, s) in r.solid.iter().enumerate().skip(solid) {
            let files: Vec<String> = s.contents.iter().map(|&c| path(c)).collect();
            lines.push(format!(
                "solid #{}: {} ({}), {} -> {} ({}), {} files: {}",
                i + 1,
                s.class,
                s.kind,
                size(s.len as f64),
                size(s.archived as f64),
                percent(s.archived as f64, s.len as f64),
                files.len(),
                files.join(" ")
            ));
        }
        for (i, s) in r.sets.iter().enumerate().skip(sets) {
            let files: Vec<String> = s.iter().map(|&c| path(c)).collect();
            lines.push(format!("set #{}: {} files coded against each other: {}", i + 1, files.len(), files.join(" ")));
        }
        for &c in &r.unverified[raw..] {
            lines.push(format!("{}: kept as it is, its formula not giving it back", path(c)));
        }
        *logged = (r.solid.len(), r.sets.len(), r.unverified.len());
        lines
    }

    /// The file recompressed the longest, how long and how far it is, where it is [`SLOW`].
    fn slowest(&self, s: &State) -> Option<String> {
        let (&c, since) = s.working.iter().min_by_key(|(_, t)| **t)?;
        let t = since.elapsed();
        let (path, len) = (self.g.contents[c].0.display(), self.g.contents[c].2);
        let far = match s.reached.get(&c) {
            Some(&n) if len > 0 => format!(", {}%", (100 * n / len).min(99)),
            _ => String::new(),
        };
        (t >= SLOW).then(|| format!("{path}, {} s{far}", t.as_secs()))
    }
}

/// Whether standard error, a terminal, takes the escape sequences that the bar is drawn with:
/// on Windows, a console once it is told to (Windows 10 on), or a pseudo-terminal such as
/// mintty's (not a console).
pub fn takes_escapes() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, SetConsoleMode,
        };
        // SAFETY: a handle of the process's own, and a mode written to a local.
        unsafe {
            let h = GetStdHandle(STD_ERROR_HANDLE);
            let mut mode = 0;
            if GetConsoleMode(h, &mut mode) == 0 {
                return true;
            }
            mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                || SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
    #[cfg(not(windows))]
    true
}

/// What an archive being decoded shows on standard error: on a terminal, a bar of how far it is.
pub struct Decoding {
    bar: bool,
    /// How far it is (the most told of each), and whether the bar is drawn.
    state: Mutex<(Option<Reached>, bool)>,
}

impl Decoding {
    pub fn new(bar: bool) -> Decoding {
        Decoding { bar, state: Mutex::default() }
    }

    pub fn tell(&self, r: Reached) {
        let mut s = self.state.lock().unwrap();
        // (told from several threads, not always in order)
        let most = |a: (u64, u64), b: (u64, u64)| (a.0.max(b.0), b.1);
        s.0 = Some(match s.0 {
            Some(o) => Reached {
                decompressed: most(o.decompressed, r.decompressed),
                evaluated: most(o.evaluated, r.evaluated),
            },
            None => r,
        });
    }
}

impl Ticking for Decoding {
    fn bars(&self) -> bool {
        self.bar
    }

    /// Draws the bar again, in place of the one drawn last (none until anything is told).
    fn tick(&self) {
        let mut s = self.state.lock().unwrap();
        let Some(r) = s.0 else { return };
        // (the totals known from the start: none, nothing to do)
        let share = |(done, total)| if total == 0 { 1.0 } else { share((done, total)) };
        let (d, e) = (share(r.decompressed), share(r.evaluated));
        let what = format!("{}% of {}, {}% decompressed", pc(e), size(r.evaluated.1 as f64), pc(d));
        let line = fit(&format!("[{}] {what}", cells(&[('=', d), ('#', e)])), WIDTH);
        // (to the start of the line drawn last, and all after it cleared)
        let text = format!("{}{line}\n", if s.1 { "\x1b[1F\x1b[J" } else { "" });
        s.1 = true;
        let mut out = std::io::stderr().lock();
        _ = out.write_all(text.as_bytes());
        _ = out.flush();
    }
}

/// What has a bar drawn again now and then, as long as it is at work.
pub trait Ticking: Sync {
    /// Whether the bar is drawn.
    fn bars(&self) -> bool;
    fn tick(&self);
}

impl Ticking for View<'_> {
    fn bars(&self) -> bool {
        self.bars()
    }

    fn tick(&self) {
        self.tick()
    }
}

/// The share `done` is of `total` (none of nothing), at most all.
fn share((done, total): (u64, u64)) -> f64 {
    if total == 0 { 0.0 } else { (done as f64 / total as f64).min(1.0) }
}

/// A share as a percentage, 100% only where it is all.
fn pc(x: f64) -> f64 {
    (100.0 * x).floor()
}

/// The cells of a bar of `phases` overlaid, each a character and its share: each phase shown as
/// far as it or any after it is, those after it over it.
fn cells(phases: &[(char, f64)]) -> String {
    let cells = |x: f64| ((x * CELLS as f64).floor() as usize).min(CELLS);
    let ends: Vec<usize> =
        (0..phases.len()).map(|i| cells(phases[i..].iter().map(|p| p.1).fold(0.0, f64::max))).collect();
    (0..CELLS).map(|i| (0..phases.len()).rev().find(|&k| i < ends[k]).map_or(' ', |k| phases[k].0)).collect()
}

/// The lines of the bar of `s`, and the file taking long, if any.
fn bar(s: &State, slow: Option<String>) -> Vec<String> {
    let (p, a, c) = (share(s.probe), share(s.analysis), share(s.compression));
    // (nothing to look at once a batch is begun, as for a file alone: the probe done)
    let p = if s.probe.1 == 0 && (s.analysis.1 > 0 || s.compression.1 > 0) { 1.0 } else { p };
    let b = cells(&[('-', p), ('=', a), ('#', c)]);
    let what = if p < 1.0 {
        format!("probing {}/{} files", s.probe.0, s.probe.1)
    } else {
        format!("{}% of {}, {}% analyzed", pc(c), size(s.compression.1.max(s.analysis.1) as f64), pc(a))
    };
    let mut lines = vec![fit(&format!("[{b}] {what}"), WIDTH)];
    if let Some(slow) = slow {
        lines.push(format!("  {}", fit(&slow, WIDTH - 2)));
    }
    lines
}

/// `s` cut to `width` columns from its start (the end of a path telling it most), as a
/// terminal shows it (East Asian characters two columns wide, about).
fn fit(s: &str, width: usize) -> String {
    let cols = |c: char| if ('\u{1100}'..='\u{ffdc}').contains(&c) || c >= '\u{20000}' { 2 } else { 1 };
    if s.chars().map(cols).sum::<usize>() <= width {
        return s.into();
    }
    let mut w = 1;
    let mut tail: Vec<char> = s
        .chars()
        .rev()
        .take_while(|&c| {
            w += cols(c);
            w <= width
        })
        .collect();
    tail.push('…');
    tail.into_iter().rev().collect()
}
