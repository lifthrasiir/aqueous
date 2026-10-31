//! The windows: task dialogs (common controls 6, which the manifest asks for), the system's own
//! look, their progress bars on the taskbar as well.
//!
//! [`progress`] shows a job as it goes, drawn again on the dialog's timer (some five times a
//! second) from what `aqr` told last; the questions `aqr` asks are asked from there too, as
//! dialogs over it.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{HWND, LPARAM, S_FALSE, S_OK, WPARAM};
use windows_sys::Win32::UI::Controls::{
    PBST_PAUSED, TASKDIALOGCONFIG, TD_ERROR_ICON, TD_INFORMATION_ICON, TD_WARNING_ICON, TDCBF_CANCEL_BUTTON,
    TDCBF_CLOSE_BUTTON, TDCBF_NO_BUTTON, TDCBF_YES_BUTTON, TDE_CONTENT, TDE_MAIN_INSTRUCTION,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER, TDF_SHOW_PROGRESS_BAR, TDM_CLICK_BUTTON, TDM_SET_ELEMENT_TEXT,
    TDM_SET_PROGRESS_BAR_POS, TDM_SET_PROGRESS_BAR_RANGE, TDM_SET_PROGRESS_BAR_STATE, TDN_BUTTON_CLICKED, TDN_CREATED,
    TDN_TIMER, TaskDialogIndirect,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{IDCANCEL, IDYES, PostMessageW, SendMessageW};
use windows_sys::core::{BOOL, HRESULT, PCWSTR};

use crate::run::{Job, Verb, file_name};

/// `s` as a string of NULs.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain([0]).collect()
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

/// A time of seconds, as `m:ss` or `h:mm:ss`.
fn time(t: f64) -> String {
    let t = t.max(0.0) as u64;
    match t / 3600 {
        0 => format!("{}:{:02}", t / 60, t % 60),
        h => format!("{h}:{:02}:{:02}", t / 60 % 60, t % 60),
    }
}

/// `s` of at most `n` characters, its start left out where it is longer (as a path's end tells
/// the most).
fn fit(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n { s.into() } else { format!("…{}", s.chars().skip(len - n + 1).collect::<String>()) }
}

pub enum Icon {
    Information,
    Warning,
    Error,
}

/// A dialog of strings that live as long as it does.
struct Config {
    c: TASKDIALOGCONFIG,
    strings: Vec<Vec<u16>>,
}

impl Config {
    fn new(parent: HWND, icon: Option<Icon>, main: &str, content: &str) -> Config {
        // SAFETY: a struct of C's, all of whose fields may be zeroes (nulls)
        let mut c: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
        c.cbSize = size_of::<TASKDIALOGCONFIG>() as u32;
        c.hwndParent = parent;
        let strings = vec![wide("Aqueous"), wide(main), wide(content)];
        c.pszWindowTitle = strings[0].as_ptr();
        c.pszMainInstruction = strings[1].as_ptr();
        c.pszContent = strings[2].as_ptr();
        c.Anonymous1.pszMainIcon = match icon {
            Some(Icon::Information) => TD_INFORMATION_ICON,
            Some(Icon::Warning) => TD_WARNING_ICON,
            Some(Icon::Error) => TD_ERROR_ICON,
            None => std::ptr::null(),
        };
        Config { c, strings }
    }

    /// `s`, kept as long as the dialog is, for a field of it.
    fn keep(&mut self, s: &str) -> PCWSTR {
        // (the string's own buffer, which stays where it is as the list grows)
        self.strings.push(wide(s));
        self.strings.last().unwrap().as_ptr()
    }

    /// Shows the dialog until it is closed: the button that closed it, and whether its
    /// checkbox was checked.
    fn show(&self) -> (i32, bool) {
        let (mut button, mut checked): (i32, BOOL) = (0, 0);
        // SAFETY: a dialog whose strings live in `self`, and the results written to locals
        unsafe { TaskDialogIndirect(&self.c, &mut button, std::ptr::null_mut(), &mut checked) };
        (button, checked != 0)
    }
}

/// Shows a message, `more` behind a button of details.
pub fn message(icon: Icon, main: &str, content: &str, more: Option<&str>) {
    let mut d = Config::new(std::ptr::null_mut(), Some(icon), main, content);
    d.c.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION;
    d.c.dwCommonButtons = TDCBF_CLOSE_BUTTON;
    if let Some(m) = more {
        d.c.pszExpandedInformation = d.keep(m);
        d.c.pszCollapsedControlText = d.keep("Show what aqr said");
        d.c.pszExpandedControlText = d.keep("Hide what aqr said");
    }
    d.show();
}

/// Lines of the content, always as many (for the dialog not to change its size).
const LINES: usize = 3;

/// What the window of a job keeps, on the thread of the dialog (its callback).
struct Window<'a> {
    job: &'a Job,
    child: RefCell<&'a mut Child>,
    /// When the stop was asked for, and whether the dialog is to close (all done).
    stopping: Cell<Option<Instant>>,
    closing: Cell<bool>,
    status: Cell<Option<ExitStatus>>,
    /// A question being asked (no other then).
    asking: Cell<bool>,
    /// The answers given to all of a kind.
    always: RefCell<Vec<(String, Value)>>,
    /// The texts shown, not set again where they are the same.
    shown: RefCell<(String, String)>,
}

/// The time given `aqr` to stop before it is killed.
const STOP: Duration = Duration::from_secs(10);

/// The time a job may take without a window.
const UNSEEN: Duration = Duration::from_millis(400);

/// Shows `job` as it goes, `child` being its `aqr`, until it is done: what it ended with
/// (`None`: unknown).
pub fn progress(job: &Job, child: &mut Child) -> Option<ExitStatus> {
    // (no window for a job of a moment)
    let start = Instant::now();
    while start.elapsed() < UNSEEN {
        {
            let s = job.state.lock().unwrap();
            if !s.questions.is_empty() {
                break;
            }
            if s.eof
                && let Ok(Some(st)) = child.try_wait()
            {
                return Some(st);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let w = Window {
        job,
        child: RefCell::new(child),
        stopping: Cell::new(None),
        closing: Cell::new(false),
        status: Cell::new(None),
        asking: Cell::new(false),
        always: RefCell::default(),
        shown: RefCell::default(),
    };
    let (main, content) = w.texts();
    let mut d = Config::new(std::ptr::null_mut(), None, &main, &content);
    d.c.dwFlags = TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER | TDF_ALLOW_DIALOG_CANCELLATION;
    d.c.dwCommonButtons = TDCBF_CANCEL_BUTTON;
    // (in dialog units: room for a path of some length)
    d.c.cxWidth = 300;
    d.c.pfCallback = Some(callback);
    d.c.lpCallbackData = &w as *const Window as isize;
    *w.shown.borrow_mut() = (main, content);
    d.show();
    w.status.get().or_else(|| {
        // (closed some other way: stopped)
        let mut c = w.child.borrow_mut();
        _ = c.kill();
        c.wait().ok()
    })
}

unsafe extern "system" fn callback(hwnd: HWND, msg: u32, wparam: WPARAM, _: LPARAM, data: isize) -> HRESULT {
    // SAFETY: the window `progress` gave the dialog, which it outlives
    let w = unsafe { &*(data as *const Window) };
    match msg as i32 {
        TDN_CREATED => {
            // SAFETY: the dialog's own messages
            unsafe { SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_RANGE as u32, 0, 1000 << 16) };
            w.tick(hwnd);
        }
        TDN_TIMER => w.tick(hwnd),
        TDN_BUTTON_CLICKED if wparam as i32 == IDCANCEL => {
            if w.closing.get() {
                return S_OK;
            }
            if w.stopping.get().is_none() {
                w.stopping.set(Some(Instant::now()));
                w.job.send(&json!({"control": "cancel"}));
                // SAFETY: the dialog's own messages
                unsafe { SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_STATE as u32, PBST_PAUSED as usize, 0) };
                w.tick(hwnd);
            }
            // (open until `aqr` is done)
            return S_FALSE;
        }
        _ => {}
    }
    S_OK
}

impl Window<'_> {
    /// Draws the window again, closes it once `aqr` is done, and asks what `aqr` asks.
    fn tick(&self, hwnd: HWND) {
        if self.asking.get() || self.closing.get() {
            return;
        }
        let eof = self.job.state.lock().unwrap().eof;
        if let Some(t) = self.stopping.get()
            && t.elapsed() > STOP
        {
            _ = self.child.borrow_mut().kill();
        }
        let done = if eof { self.child.borrow_mut().try_wait().ok().flatten() } else { None };
        if let Some(st) = done {
            self.status.set(Some(st));
            self.closing.set(true);
            // SAFETY: the dialog's own message, handled once this callback is done
            unsafe { PostMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDCANCEL as usize, 0) };
            return;
        }
        let (main, content) = self.texts();
        let mut shown = self.shown.borrow_mut();
        let (shown_main, shown_content) = &mut *shown;
        for (element, text, was) in [(TDE_MAIN_INSTRUCTION, &main, shown_main), (TDE_CONTENT, &content, shown_content)]
        {
            if *text != *was {
                let t = wide(text);
                // SAFETY: the dialog's own message, the text copied as it is taken
                unsafe { SendMessageW(hwnd, TDM_SET_ELEMENT_TEXT as u32, element as usize, t.as_ptr() as isize) };
                *was = text.clone();
            }
        }
        drop(shown);
        let f = self.share();
        // SAFETY: the dialog's own message
        unsafe { SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_POS as u32, (f * 1000.0) as usize, 0) };
        let question = self.job.state.lock().unwrap().questions.pop_front();
        if let Some(q) = question {
            self.asking.set(true);
            let answer = self.ask(hwnd, &q);
            self.asking.set(false);
            self.job.send(&json!({"answer": q["ask"], "value": answer}));
        }
    }

    /// The share of the work done, of the archive at work.
    fn share(&self) -> f64 {
        let s = self.job.state.lock().unwrap();
        let Some(p) = &s.progress else { return 0.0 };
        let of = |k: &str| {
            let (done, total) = (p[k][0].as_f64().unwrap_or(0.0), p[k][1].as_f64().unwrap_or(0.0));
            if total > 0.0 { (done / total).min(1.0) } else { 0.0 }
        };
        match self.job.verb {
            // (the analysis, recompressing, takes the most time mostly)
            Verb::Compress => {
                let probe = if p["probe"][1].as_u64() == Some(0) { 1.0 } else { of("probe") };
                0.05 * probe + 0.65 * of("analysis") + 0.3 * of("compression")
            }
            Verb::Extract | Verb::Test => 0.3 * of("decompressed") + 0.7 * of("evaluated"),
        }
    }

    /// The main instruction and the content, as of what `aqr` told last.
    fn texts(&self) -> (String, String) {
        let f = self.share();
        let s = self.job.state.lock().unwrap();
        let mut main = match s.name.as_str() {
            "" => format!("{}…", self.job.verb.doing()),
            n => format!("{} {n}", self.job.verb.doing()),
        };
        if self.job.count > 1 && s.begun > 0 {
            main += &format!(" ({} of {})", s.begun, self.job.count);
        }
        if self.stopping.get().is_some() {
            main = "Stopping…".into();
        }
        let mut lines = Vec::new();
        if let Some(p) = &s.progress {
            let pair = |k: &str| (p[k][0].as_f64().unwrap_or(0.0), p[k][1].as_f64().unwrap_or(0.0));
            let bytes = |(done, total): (f64, f64)| format!("{} of {}", size(done), size(total));
            lines.push(match self.job.verb {
                Verb::Compress => {
                    let (probe, analysis, compression) = (pair("probe"), pair("analysis"), pair("compression"));
                    if analysis.0 == 0.0 && probe.0 < probe.1 {
                        format!("Looking into files: {} of {}", probe.0, probe.1)
                    } else if analysis.0 < analysis.1 {
                        format!("Recompressing: {}", bytes(analysis))
                    } else {
                        format!("Compressing: {}", bytes(compression))
                    }
                }
                _ => format!("{}: {}", if self.job.verb == Verb::Test { "Checked" } else { "Written" }, {
                    bytes(pair("evaluated"))
                }),
            });
            let file = &p["file"];
            if let Some(path) = file["path"].as_str() {
                let mut line = format!("File: {}", fit(path, 60));
                if let (Some(done), Some(len)) = (file["done"].as_f64(), file["len"].as_f64())
                    && len > 0.0
                {
                    line += &format!(", {:.0}%", (100.0 * done / len).min(99.0));
                }
                if let Some(t) = file["seconds"].as_f64().filter(|&t| t >= 2.0) {
                    line += &format!(", {}", time(t));
                }
                lines.push(line);
            }
        }
        if let Some(since) = s.since {
            let t = since.elapsed().as_secs_f64();
            let mut line = format!("Time: {}", time(t));
            if f >= 0.02 && t >= 3.0 {
                line += &format!(", about {} left", time(t * (1.0 - f) / f));
            }
            lines.push(line);
        }
        lines.resize(LINES, String::new());
        // (an empty line keeps its height with a space)
        let content = lines.iter().map(|l| if l.is_empty() { " " } else { l }).collect::<Vec<_>>().join("\n");
        (main, content)
    }

    /// The answer to question `q`, asked over the window `hwnd` (or given before for all).
    fn ask(&self, hwnd: HWND, q: &Value) -> Value {
        let kind = q["kind"].as_str().unwrap_or("");
        if let Some((_, a)) = self.always.borrow().iter().find(|(k, _)| k == kind) {
            return a.clone();
        }
        match kind {
            "overwrite" => {
                let path = q["path"].as_str().unwrap_or("");
                let dir = std::path::Path::new(path).is_dir();
                let main = format!("{} is there already", file_name(path));
                let content = if dir {
                    format!("Extract into the folder {path} all the same?\nIts files of the same names are replaced.")
                } else {
                    format!("Replace the file {path}?")
                };
                let mut d = Config::new(hwnd, Some(Icon::Warning), &main, &content);
                d.c.dwCommonButtons = TDCBF_YES_BUTTON | TDCBF_NO_BUTTON;
                d.c.nDefaultButton = IDYES;
                if self.job.count > 1 {
                    d.c.pszVerificationText = d.keep("Do the same for the other archives");
                }
                let (button, all) = d.show();
                let answer = Value::Bool(button == IDYES);
                if all {
                    self.always.borrow_mut().push((kind.into(), answer.clone()));
                }
                answer
            }
            _ => Value::Null,
        }
    }
}
