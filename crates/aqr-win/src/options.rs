//! The compression dialog with its options (a preview, `aqr-win options-preview`): a column of
//! preset, format and level, and beside it, where "Advanced options" opens it, a pane of the
//! rest that scrolls, as the old color dialog grows. Plain Win32 windows and common controls
//! (no dialog template: the pane scrolls, so the layout is made in code, in 96-dpi units).
//!
//! The text is English and in the code for now; the estimate of size and time is not made.
//! The design: the artifact "Aqueous · Compression dialog". Pressing Compress shows the command
//! line the settings make, as nothing is run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_3DFACE, MapWindowPoints, CreateFontIndirectW, CreateSolidBrush, GetSysColor, GetSysColorBrush, HDC, LOGFONTW, SetBkColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, BST_CHECKED, BST_UNCHECKED, SetScrollInfo, TBM_SETPAGESIZE,
    TBM_SETPOS, TBM_SETRANGE, TBM_SETTICFREQ, TBS_AUTOTICKS, TBS_HORZ, TTDT_AUTOPOP, TTF_IDISHWND, TTF_SUBCLASS,
    TTM_ADDTOOLW, TTM_SETDELAYTIME, TTM_SETMAXTIPWIDTH, TTS_ALWAYSTIP, TTTOOLINFOW,
};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, GetKeyState, IsWindowEnabled, SetFocus, VK_SHIFT, VK_TAB};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON,
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT,
    CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_MULTILINE, ES_READONLY,
    GetDlgCtrlID, GetMessageW, GetScrollInfo, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW,
    GA_ROOT, SetTimer, WM_TIMER, GetAncestor, GetClientRect, GetWindowRect, IsWindowVisible, WindowFromPoint, WM_KEYDOWN, IDCANCEL, IDOK, IsChild, IsDialogMessageW, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT,
    LBS_NOTIFY, LoadCursorW, MB_ICONINFORMATION, MB_OK, MSG, MessageBoxW, PostQuitMessage, RegisterClassExW,
    SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBTRACK, SB_VERT, SCROLLINFO, SIF_ALL, SIF_PAGE, SIF_POS,
    SIF_RANGE, SPI_GETICONTITLELOGFONT, SW_ERASE, SW_HIDE,
    SW_INVALIDATE, SW_SCROLLCHILDREN, SW_SHOW, SW_SHOWNORMAL, ScrollWindowEx, SendMessageW,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_HSCROLL, WM_MOUSEWHEEL, WM_SETFONT, WM_VSCROLL, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_CLIPCHILDREN, WS_EX_TOPMOST, WS_GROUP, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};

use aqr_ipc::Job;
use aqr_l10n::options as t;

use crate::dialog::wide;

// ---------- the options ----------

const BATCH: [&str; 6] = ["64M", "128M", "256M", "512M", "1G", "2G"];
const BLOCK: [&str; 7] = ["1M", "2M", "4M", "8M", "16M", "32M", "64M"];
const SEGMENT: [&str; 7] = ["one segment", "64M", "128M", "256M", "512M", "1G", "4G"];
const APPB: [&str; 5] = ["128M", "256M", "512M", "1G", "none"];
/// The codes of `-M`, in the order of the drop-down list ([`mem_labels`]).
const MEM: [&str; 7] = ["auto", "0.25", "0.5", "1", "4", "avail", "none"];
fn mem_labels() -> [&'static str; 7] {
    [t::mem_auto(), t::mem_quarter(), t::mem_half(), t::mem_1gb(), t::mem_4gb(), t::mem_free(), t::mem_none()]
}
fn modes() -> [&'static str; 4] {
    [t::mode_ask(), t::mode_overwrite(), t::mode_append(), t::mode_sync()]
}
const THREADS: [usize; 6] = [0, 1, 2, 4, 8, 12];
/// The name of level `l` of Aqueous' own scale.
fn level_name(l: i32) -> String {
    match l {
        0 => t::level_store().into(),
        1 => t::level_fast().into(),
        2 => t::level_low().into(),
        5 => t::level_mid().into(),
        8 => t::level_high().into(),
        9 => t::level_max().into(),
        l => l.to_string(),
    }
}
const FORMATS: [(&str, &str); 16] = [
    ("aqr", "Aqueous (.aqr)"),
    ("zip", "ZIP"),
    ("7z", "7z"),
    ("alz", "ALZ"),
    ("egg", "EGG"),
    ("tar", "tar"),
    ("tar.gz", "tar.gz"),
    ("tar.bz2", "tar.bz2"),
    ("tar.xz", "tar.xz"),
    ("tar.lzma", "tar.lzma"),
    ("tar.zst", "tar.zst"),
    ("gz", "gz"),
    ("bz2", "bz2"),
    ("xz", "xz"),
    ("lzma", "lzma"),
    ("zst", "zst"),
];
const PRESET_COUNT: usize = 5;
fn presets() -> [&'static str; PRESET_COUNT] {
    [t::preset_balanced(), t::preset_fast(), t::preset_smallest(), t::preset_archival(), t::preset_zip()]
}

/// The levels of a format: lowest, highest and default; none where it has no level (tar).
fn levels(format: &str) -> Option<(i32, i32, i32)> {
    match format {
        "aqr" | "7z" => Some((0, 9, 5)),
        "zip" | "alz" | "egg" | "tar.gz" | "gz" | "tar.xz" | "tar.lzma" | "xz" | "lzma" => Some((0, 9, 6)),
        "tar.bz2" | "bz2" => Some((1, 9, 9)),
        "tar.zst" | "zst" => Some((1, 22, 3)),
        _ => None,
    }
}

#[derive(Clone, PartialEq)]
struct Opts {
    format: &'static str,
    level: i32,
    zstd_scale: bool,
    zl: i32,
    depth: i32,
    exclude: Vec<String>,
    deref: bool,
    preserve: bool,
    batch: i32,
    block: i32,
    segment: i32,
    dict: bool,
    encrypt: bool,
    archival: bool,
    check: bool,
    atomic: bool,
    mode: i32,
    journal: String,
    rm: bool,
    threads: i32,
    mem: i32,
    appb: i32,
}

impl Opts {
    fn new() -> Opts {
        Opts {
            format: "aqr",
            level: 5,
            zstd_scale: false,
            zl: 3,
            depth: 3,
            exclude: vec![".DS_Store".into(), "Thumbs.db".into()],
            deref: false,
            preserve: false,
            batch: 3,
            block: 4,
            segment: 3,
            dict: false,
            encrypt: false,
            archival: false,
            check: true,
            atomic: false,
            mode: 0,
            journal: String::new(),
            rm: false,
            threads: 0,
            mem: 0,
            appb: 1,
        }
    }

    /// The settings of preset `i` (the exclusions are kept out of presets).
    fn preset(i: usize, keep: &[String]) -> Opts {
        let mut o = Opts::new();
        o.exclude = keep.to_vec();
        match i {
            1 => (o.level, o.check) = (2, false),
            2 => (o.level, o.dict, o.batch, o.block, o.depth) = (9, true, 5, 6, 5),
            3 => (o.level, o.archival, o.atomic) = (8, true, true),
            4 => (o.format, o.level) = ("zip", 6),
            _ => {}
        }
        o
    }

    /// Whether the settings a preset names are those of `p`.
    fn same(&self, p: &Opts) -> bool {
        (self.format, self.level, self.check, self.dict, self.batch, self.block, self.depth) ==
            (p.format, p.level, p.check, p.dict, p.batch, p.block, p.depth) &&
            (self.archival, self.atomic, self.zstd_scale) == (p.archival, p.atomic, p.zstd_scale)
    }

    fn aqr(&self) -> bool {
        self.format == "aqr"
    }

    /// How many settings differ from the defaults, by section.
    fn changes(&self) -> [usize; 6] {
        let (d, a) = (Opts::new(), self.aqr());
        let n = |b: bool| b as usize;
        [
            n(a && self.zstd_scale) + n(a && self.depth != d.depth),
            n(!self.exclude.is_empty()) + n(self.deref) + n(self.preserve),
            if a { n(self.batch != d.batch) + n(self.block != d.block) + n(self.segment != d.segment) + n(self.dict) } else { 0 },
            n(self.encrypt) + n(a && self.archival) + n(a && !self.check) + n(a && self.atomic),
            n(self.mode != 0) + n(!self.journal.is_empty()) + n(self.rm),
            n(self.threads != 0) + n(self.mem != 0) + n(self.appb != d.appb),
        ]
    }

    /// The arguments of `aqr` for the settings, compressing `inputs` into `output`.
    fn args(&self, output: &str, inputs: &[String]) -> Vec<String> {
        fn two(a: &mut Vec<String>, flag: &str, value: impl Into<String>) {
            a.push(flag.into());
            a.push(value.into());
        }
        let mut a: Vec<String> = vec!["-c".into()];
        let aqr = self.aqr();
        if !aqr {
            two(&mut a, "--format", self.format);
        }
        if let Some((_, _, def)) = levels(self.format) {
            if aqr && self.zstd_scale {
                two(&mut a, "-l", format!("zstd{}", self.zl));
            } else if self.level != def {
                two(&mut a, "-l", self.level.to_string());
            }
        }
        let d = Opts::new();
        if aqr && self.depth != d.depth {
            two(&mut a, "--depth", self.depth.to_string());
        }
        for x in &self.exclude {
            two(&mut a, "-x", x.as_str());
        }
        for (on, flag) in [(self.deref, "-L"), (self.preserve, "-p")] {
            if on {
                a.push(flag.into());
            }
        }
        if aqr {
            if self.batch != d.batch {
                two(&mut a, "-B", BATCH[self.batch as usize]);
            }
            if self.block != d.block {
                two(&mut a, "-b", BLOCK[self.block as usize]);
            }
            if self.segment != d.segment {
                two(&mut a, "-S", if self.segment == 0 { "0" } else { SEGMENT[self.segment as usize] });
            }
            for (on, flag) in [(self.dict, "-D"), (self.archival, "-A"), (!self.check, "--no-check"), (self.atomic, "--atomic")]
            {
                if on {
                    a.push(flag.into());
                }
            }
        }
        if self.encrypt {
            a.push("-P".into());
        }
        if let Some(f) = [None, Some("-f"), Some("-a"), Some("-s")][self.mode as usize] {
            a.push(f.into());
        }
        if !self.journal.is_empty() && self.mode != 1 {
            two(&mut a, "-j", self.journal.as_str());
        }
        if self.rm {
            a.push("--rm".into());
        }
        if self.threads != 0 {
            two(&mut a, "-T", THREADS[self.threads as usize].to_string());
        }
        if self.mem != 0 {
            let m = MEM[self.mem as usize];
            two(
                &mut a,
                "-M",
                match m {
                    "none" => "0".into(),
                    "avail" => "avail".into(),
                    m if m.starts_with("0.") => format!("{}%", (m.parse::<f64>().unwrap() * 100.0).round()),
                    m => format!("{m}G"),
                },
            );
        }
        if self.appb != d.appb {
            two(&mut a, "--application-bound", if self.appb == 4 { "unlimited" } else { APPB[self.appb as usize] });
        }
        two(&mut a, "-o", output);
        a.push("--".into());
        a.extend(inputs.iter().cloned());
        a
    }

    /// The arguments that bear on what is gathered of `inputs` (`aqr --session`'s inventory
    /// asks by them), which `args` has the same of.
    fn gather_args(&self, inputs: &[String]) -> Vec<String> {
        let mut a: Vec<String> = vec!["-c".into()];
        for x in &self.exclude {
            a.extend(["-x".into(), x.clone()]);
        }
        for (on, flag) in [(self.deref, "-L"), (self.preserve, "-p"), (self.rm, "--rm")] {
            if on {
                a.push(flag.into());
            }
        }
        a.push("--".into());
        a.extend(inputs.iter().cloned());
        a
    }

    /// The command line of the settings, as a shell has it.
    fn cli(&self, output: &str, inputs: &[String]) -> String {
        let quote = |s: &String| {
            if s.is_empty() || s.contains([' ', '"', '*', '?', '<', '>', '|', '&']) {
                format!("\"{}\"", s.replace('"', "\\\""))
            } else {
                s.clone()
            }
        };
        let args = self.args(output, inputs);
        format!("aqr {}", args.iter().map(quote).collect::<Vec<_>>().join(" "))
    }
}

/// A size of the lists above (`64M`, `2G`) as text.
fn pretty(s: &str) -> String {
    match s.split_at(s.len() - 1) {
        _ if s == "one segment" => t::never().into(),
        (n, "M") => t::size_mb(n),
        (n, _) => t::size_gb(n),
    }
}

// ---------- controls ----------

const PRESET: i32 = 101;
const FORMAT: i32 = 102;
const LEVEL: i32 = 103;
const TOGGLE: i32 = 104;
const RESET: i32 = 105;
const SUMMARY: i32 = 106;
// (the slider whose value text is the static of id + 100)
const DEPTH: i32 = 110;
const SCALE_AQR: i32 = 111;
const SCALE_ZSTD: i32 = 112;
const ZL: i32 = 113;
const EXLIST: i32 = 114;
const EXEDIT: i32 = 115;
const EXADD: i32 = 116;
const EXDEL: i32 = 117;
const DEREF: i32 = 118;
const PRESERVE: i32 = 119;
const BATCHES: i32 = 120;
const BLOCKS: i32 = 121;
const SEGMENTS: i32 = 122;
const DICT: i32 = 123;
const ENCRYPT: i32 = 124;
const ARCHIVAL: i32 = 125;
const CHECK: i32 = 126;
const ATOMIC: i32 = 127;
const MODE: i32 = 128;
const JOURNAL: i32 = 129;
const RM: i32 = 130;
const THREAD: i32 = 131;
const MEMORY: i32 = 132;
const APPBOUND: i32 = 133;
const CLI: i32 = 134;
const WARN: i32 = 135;
const VALUE: i32 = 100;
const TBM_GETPOS: u32 = 0x0400;
const SS_LEFT: u32 = 0;
const SS_ETCHEDHORZ: u32 = 0x10;
const SS_NOTIFY: u32 = 0x100;
const SS_ENDELLIPSIS: u32 = 0x4000;

const SLIDERS: [i32; 6] = [DEPTH, BATCHES, BLOCKS, SEGMENTS, APPBOUND, ZL];
const SECTIONS: usize = 6;
fn section(i: usize) -> &'static str {
    [
        t::section_recompression(),
        t::section_contents(),
        t::section_layout(),
        t::section_safety(),
        t::section_output(),
        t::section_resources(),
    ][i]
}

/// Metrics of the window in 96-dpi units.
const MAIN_W: i32 = 350;
const PANE_W: i32 = 580;
const H: i32 = 580;

struct Cx {
    dpi: i32,
    inst: *mut c_void,
    font: *mut c_void,
    bold: *mut c_void,
}

impl Cx {
    fn s(&self, v: i32) -> i32 {
        (v as i64 * self.dpi as i64 / 96) as i32
    }
}

struct App {
    main: HWND,
    pane: HWND,
    tips: HWND,
    cx: Cx,
    o: Opts,
    ctl: HashMap<i32, HWND>,
    /// What only Aqueous archives take.
    aqr_only: Vec<HWND>,
    /// The controls of the pane that take the Tab key, in its order.
    pane_tab: Vec<HWND>,
    headers: Vec<HWND>,
    content_h: i32,
    open: bool,
    cpus: usize,
    /// With a session of `aqr`: what the dialog tells it ([`run`]); none in the preview.
    job: Option<Arc<Job>>,
    /// The inputs, as `aqr` is given them and as they are shown, and the name (without its
    /// extension) and folder of the archive made.
    inputs: Vec<String>,
    names: Vec<String>,
    stem: String,
    out_dir: PathBuf,
    /// The arguments to begin with, once Compress is pressed.
    go: Option<Vec<String>>,
    /// The inventory asked last: its id, and the arguments it was asked by.
    req: u64,
    req_args: Vec<String>,
}

impl App {
    /// The name of the archive made.
    fn out_name(&self) -> String {
        format!("{}.{}", self.stem, self.o.format)
    }

    /// What the line under the names of the inputs tells: where they go and, as `aqr` finds
    /// it out, how many files they are and how big (nothing, then the files found so far).
    fn summary(&self) -> String {
        let name = self.out_name();
        let Some(job) = &self.job else {
            return t::summary(14208, name, aqr_l10n::size(38.4e9));
        };
        let found = job.state.lock().unwrap().inventory.clone().filter(|i| i.id == self.req);
        match found {
            None => t::summary_none(name),
            Some(i) if i.error.is_some() => t::summary_error(name, i.error.unwrap_or_default()),
            Some(i) if i.done => t::summary(i.files, name, aqr_l10n::size(i.bytes as f64)),
            Some(i) => t::summary_counting(i.files, name),
        }
    }
}

/// The timer: what `aqr` has found of the inputs since is shown.
fn poll(a: &mut App) {
    let text = a.summary();
    set_text(a.ctl[&(SUMMARY + 1000)], &text);
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn text_of(h: HWND) -> String {
    // SAFETY: a window of this thread and room for its text and the NUL
    unsafe {
        let len = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) as usize;
        String::from_utf16_lossy(&buf[..n.min(len)])
    }
}

fn set_text(h: HWND, s: &str) {
    if text_of(h) != s {
        // SAFETY: a window of this thread and a string of NULs
        unsafe { SetWindowTextW(h, wide(s).as_ptr()) };
    }
}

/// A child window of `parent` at `r` (x, y, width, height in 96-dpi units).
fn make(cx: &Cx, parent: HWND, class: &str, text: &str, style: u32, r: [i32; 4], id: i32, bold: bool) -> HWND {
    // SAFETY: strings of NULs, a parent of this thread; the font outlives the windows
    unsafe {
        let h = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            style | WS_CHILD | WS_VISIBLE,
            cx.s(r[0]),
            cx.s(r[1]),
            cx.s(r[2]),
            cx.s(r[3]),
            parent,
            id as usize as *mut c_void,
            cx.inst,
            null(),
        );
        SendMessageW(h, WM_SETFONT, if bold { cx.bold } else { cx.font } as usize, 1);
        h
    }
}

fn tip(tips: HWND, owner: HWND, ctl: HWND, text: &str) {
    if text.is_empty() {
        return;
    }
    let mut t = wide(text);
    // SAFETY: a struct of C's that zeroes fit; the text is copied by the tooltip control
    unsafe {
        let mut ti: TTTOOLINFOW = std::mem::zeroed();
        ti.cbSize = size_of::<TTTOOLINFOW>() as u32;
        ti.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
        ti.hwnd = owner;
        ti.uId = ctl as usize;
        ti.lpszText = t.as_mut_ptr();
        SendMessageW(tips, TTM_ADDTOOLW, 0, &ti as *const _ as isize);
    }
}

/// Lays the pane out row by row.
struct Rows<'a> {
    a: &'a mut App,
    y: i32,
}

const LX: i32 = 16;
const LW: i32 = 216;
const CX: i32 = 240;
const CW: i32 = 196;

impl Rows<'_> {
    fn pane(&self) -> HWND {
        self.a.pane
    }

    fn reg(&mut self, id: i32, h: HWND, aqr: bool, tip_text: &str) {
        self.a.ctl.insert(id, h);
        self.a.pane_tab.push(h);
        if aqr {
            self.a.aqr_only.push(h);
        }
        tip(self.a.tips, self.a.main, h, tip_text);
    }

    fn header(&mut self, title: &str) {
        self.y += 10;
        let h = make(&self.a.cx, self.pane(), "STATIC", title, SS_LEFT as u32, [LX, self.y, 400, 18], -1, true);
        self.a.headers.push(h);
        make(&self.a.cx, self.pane(), "STATIC", "", SS_ETCHEDHORZ as u32, [LX, self.y + 21, 530, 2], -1, false);
        self.y += 30;
    }

    fn label(&mut self, text: &str, expert: bool, aqr: bool, tip_text: &str) {
        let text = if expert { t::expert(text) } else { text.into() };
        let style = (SS_LEFT | SS_NOTIFY | SS_ENDELLIPSIS) as u32;
        let h = make(&self.a.cx, self.pane(), "STATIC", &text, style, [LX, self.y + 5, LW, 18], -1, false);
        if aqr {
            self.a.aqr_only.push(h);
        }
        tip(self.a.tips, self.a.main, h, tip_text);
    }

    fn check(&mut self, id: i32, text: &str, aqr: bool, tip_text: &str) {
        let style = WS_TABSTOP | BS_AUTOCHECKBOX as u32;
        let h = make(&self.a.cx, self.pane(), "BUTTON", text, style, [LX, self.y, 530, 22], id, false);
        self.reg(id, h, aqr, tip_text);
        self.y += 26;
    }

    fn slider(&mut self, id: i32, text: &str, max: i32, expert: bool, aqr: bool, tip_text: &str) {
        self.label(text, expert, aqr, tip_text);
        let style = WS_TABSTOP | (TBS_HORZ | TBS_AUTOTICKS) as u32;
        let h = make(&self.a.cx, self.pane(), "msctls_trackbar32", "", style, [CX, self.y, CW, 28], id, false);
        // SAFETY: a trackbar made above
        unsafe {
            SendMessageW(h, TBM_SETRANGE, 1, (max << 16) as isize);
            SendMessageW(h, TBM_SETTICFREQ, 1, 0);
            SendMessageW(h, TBM_SETPAGESIZE, 0, 1);
        }
        self.reg(id, h, aqr, tip_text);
        let v = make(&self.a.cx, self.pane(), "STATIC", "", SS_LEFT as u32, [CX + CW + 6, self.y + 5, 98, 18], id + VALUE, false);
        self.a.ctl.insert(id + VALUE, v);
        if aqr {
            self.a.aqr_only.push(v);
        }
        self.y += 32;
    }

    fn combo(&mut self, id: i32, text: &str, items: &[String], expert: bool, aqr: bool, tip_text: &str) {
        self.label(text, expert, aqr, tip_text);
        let style = WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32;
        let h = make(&self.a.cx, self.pane(), "COMBOBOX", "", style, [CX, self.y, CW + 100, 220], id, false);
        for i in items {
            // SAFETY: a combo box made above
            unsafe { SendMessageW(h, CB_ADDSTRING, 0, wide(i).as_ptr() as isize) };
        }
        self.reg(id, h, aqr, tip_text);
        self.y += 32;
    }

    fn edit(&mut self, id: i32, text: &str, tip_text: &str) {
        self.label(text, false, false, tip_text);
        let style = WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32;
        let h = make(&self.a.cx, self.pane(), "EDIT", "", style, [CX, self.y, CW + 100, 22], id, false);
        self.reg(id, h, false, tip_text);
        self.y += 32;
    }
}

// ---------- building ----------

pub fn preview() -> std::process::ExitCode {
    open(None, Vec::new());
    std::process::ExitCode::SUCCESS
}

/// The name (without its extension) of the archive of `paths`, and its folder: as `aqr_ipc::command`
/// has them (one path: its name, beside it; several: the name of their folder, in it).
fn target(paths: &[PathBuf]) -> (String, PathBuf) {
    let dir = paths[0].parent().map(PathBuf::from).unwrap_or_default();
    let stem = match paths {
        [one] => aqr_ipc::file_name(one),
        _ => dir.file_name().map_or_else(|| "Archive".into(), |n| n.to_string_lossy().into_owned()),
    };
    (stem, dir)
}

/// Shows the dialog on `paths` (a preview on examples where there are none), with the session
/// of `aqr` that finds out what they are as `job` where there is one: the arguments to
/// compress with, once the user is done; none where the dialog is cancelled.
pub fn open(job: Option<Arc<Job>>, paths: Vec<PathBuf>) -> Option<Vec<String>> {
    // SAFETY: plain Win32 calls on this thread; every pointer passed lives through its call
    unsafe {
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
        });
        let inst = GetModuleHandleW(null());
        for (name, proc) in [("AqrOptions", main_proc as WNDPROC), ("AqrOptionsPane", pane_proc)] {
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(proc);
            wc.hInstance = inst;
            wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
            wc.hbrBackground =
                if name == "AqrOptionsPane" { pane_brush() } else { (COLOR_3DFACE + 1) as usize as *mut c_void };
            let class = wide(name);
            wc.lpszClassName = class.as_ptr();
            RegisterClassExW(&wc);
        }
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let main = CreateWindowExW(
            0,
            wide("AqrOptions").as_ptr(),
            wide(t::title()).as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            300,
            null_mut(),
            null_mut(),
            inst,
            null(),
        );
        let dpi = GetDpiForWindow(main).max(96) as i32;
        let mut lf: LOGFONTW = std::mem::zeroed();
        SystemParametersInfoForDpi(SPI_GETICONTITLELOGFONT, size_of::<LOGFONTW>() as u32, (&mut lf as *mut LOGFONTW).cast(), 0, dpi as u32);
        let font = CreateFontIndirectW(&lf);
        lf.lfWeight = 700;
        let bold = CreateFontIndirectW(&lf);
        let cx = Cx { dpi, inst, font, bold };

        let pane = CreateWindowExW(
            0,
            wide("AqrOptionsPane").as_ptr(),
            null(),
            WS_CHILD | WS_CLIPCHILDREN | WS_VSCROLL,
            cx.s(MAIN_W),
            0,
            cx.s(PANE_W),
            cx.s(H),
            main,
            null_mut(),
            inst,
            null(),
        );
        let tips = CreateWindowExW(
            WS_EX_TOPMOST,
            wide("tooltips_class32").as_ptr(),
            null(),
            WS_POPUP | TTS_ALWAYSTIP as u32,
            0,
            0,
            0,
            0,
            main,
            null_mut(),
            inst,
            null(),
        );
        SendMessageW(tips, TTM_SETMAXTIPWIDTH, 0, cx.s(320) as isize);
        SendMessageW(tips, TTM_SETDELAYTIME, TTDT_AUTOPOP as usize, 30000);

        let (stem, out_dir) = if paths.is_empty() { ("Photos-2026".to_string(), PathBuf::new()) } else { target(&paths) };
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let mut a = App {
            main,
            pane,
            tips,
            cx,
            o: Opts::new(),
            ctl: HashMap::new(),
            aqr_only: Vec::new(),
            pane_tab: Vec::new(),
            headers: Vec::new(),
            content_h: 0,
            open: false,
            cpus,
            job,
            inputs: if paths.is_empty() { vec!["Photos-2026".into(), "project-notes".into()] } else { paths.iter().map(|p| p.display().to_string()).collect() },
            names: if paths.is_empty() { vec!["Photos-2026".into(), "project-notes".into()] } else { paths.iter().map(|p| aqr_ipc::file_name(p)).collect() },
            stem,
            out_dir,
            go: None,
            req: 0,
            req_args: Vec::new(),
        };
        build_main(&mut a);
        build_pane(&mut a);
        let (w, h) = frame(&a, false);
        crate::dialog::set_icon(main);
        SetWindowPos(main, null_mut(), 0, 0, w, h, 0x0002 | 0x0004 | 0x0010); // NOMOVE | NOZORDER | NOACTIVATE
        center(main);
        let (pane_h, content) = (a.cx.s(H), a.content_h);
        set_scroll(pane, content, pane_h);
        refresh(&mut a);
        APP.with(|s| *s.borrow_mut() = Some(a));
        SetTimer(main, 1, 250, None);
        ShowWindow(main, SW_SHOWNORMAL);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_MOUSEWHEEL && !wheel(&mut msg, main, pane) {
                continue;
            }
            if msg.message == WM_KEYDOWN && msg.wParam == VK_TAB as usize {
                let back = GetKeyState(VK_SHIFT as i32) < 0;
                if with_app(|a| tab(a, back)) == Some(true) {
                    continue;
                }
            }
            let target = if IsChild(pane, msg.hwnd) != 0 { pane } else { main };
            if IsDialogMessageW(target, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    APP.with(|s| s.borrow_mut().take()).and_then(|a| a.go)
}

type WNDPROC = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// The outer size of the window, with the pane open or not.
fn frame(a: &App, open: bool) -> (i32, i32) {
    let w = a.cx.s(MAIN_W + if open { PANE_W } else { 0 });
    let mut r = RECT { left: 0, top: 0, right: w, bottom: a.cx.s(H) };
    // SAFETY: a rectangle of ours, the style the window was made with
    unsafe {
        AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, 0, 0, a.cx.dpi as u32);
    }
    (r.right - r.left, r.bottom - r.top)
}

fn center(h: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, GetWindowRect, SM_CXSCREEN, SM_CYSCREEN};
    // SAFETY: a window of this thread and a rectangle of ours
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        GetWindowRect(h, &mut r);
        let (x, y) = ((GetSystemMetrics(SM_CXSCREEN) - (r.right - r.left)) / 2, (GetSystemMetrics(SM_CYSCREEN) - (r.bottom - r.top)) / 3);
        SetWindowPos(h, null_mut(), x, y, 0, 0, 0x0001 | 0x0004 | 0x0010); // NOSIZE | NOZORDER | NOACTIVATE
    }
}

fn build_main(a: &mut App) {
    let (cx, m) = (&a.cx, a.main);
    let mut reg = Vec::new();
    reg.push((SUMMARY, make(cx, m, "STATIC", "", (SS_LEFT | SS_ENDELLIPSIS) as u32, [16, 14, 318, 18], -1, true)));
    reg.push((SUMMARY + 1000, make(cx, m, "STATIC", "", SS_LEFT as u32, [16, 34, 318, 34], SUMMARY + 1000, false)));
    for (i, (id, label)) in [(PRESET, t::preset()), (FORMAT, t::format()), (LEVEL, t::level())].into_iter().enumerate() {
        let y = 84 + 34 * i as i32;
        make(cx, m, "STATIC", label, SS_LEFT as u32, [16, y + 4, 64, 18], -1, false);
        reg.push((id, make(cx, m, "COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32, [84, y, 250, 260], id, false)));
    }
    reg.push((TOGGLE, make(cx, m, "BUTTON", "", WS_TABSTOP | BS_PUSHBUTTON as u32, [16, 478, 318, 28], TOGGLE, false)));
    make(cx, m, "STATIC", "", SS_ETCHEDHORZ as u32, [0, 520, MAIN_W, 2], -1, false);
    reg.push((RESET, make(cx, m, "BUTTON", t::reset(), WS_TABSTOP | BS_PUSHBUTTON as u32, [16, 534, 70, 26], RESET, false)));
    reg.push((IDOK, make(cx, m, "BUTTON", t::compress(), WS_TABSTOP | BS_DEFPUSHBUTTON as u32, [176, 534, 82, 26], IDOK, false)));
    reg.push((IDCANCEL, make(cx, m, "BUTTON", aqr_l10n::common::cancel(), WS_TABSTOP | BS_PUSHBUTTON as u32, [264, 534, 70, 26], IDCANCEL, false)));
    for (id, h) in reg {
        a.ctl.insert(id, h);
    }
    let (preset, format) = (a.ctl[&PRESET], a.ctl[&FORMAT]);
    // SAFETY: combo boxes made above
    unsafe {
        for p in presets().into_iter().chain([t::preset_custom()]) {
            SendMessageW(preset, CB_ADDSTRING, 0, wide(p).as_ptr() as isize);
        }
        for (_, f) in FORMATS {
            SendMessageW(format, CB_ADDSTRING, 0, wide(f).as_ptr() as isize);
        }
    }
    let tips = a.tips;
    let (main, preset, format, level) = (a.main, a.ctl[&PRESET], a.ctl[&FORMAT], a.ctl[&LEVEL]);
    tip(tips, main, preset, t::tip_preset());
    tip(tips, main, format, t::tip_format());
    tip(tips, main, level, t::tip_level());
}

fn build_pane(a: &mut App) {
    let cpus = a.cpus;
    let mut r = Rows { a, y: 0 };
    r.header(t::section_recompression());
    r.slider(DEPTH, t::depth(), 6, false, true, t::tip_depth());
    r.label(t::level_scale(), true, true, t::tip_level_scale());
    let tipt = t::tip_level_scale();
    let (a1, a2) = (
        make(&r.a.cx, r.pane(), "BUTTON", t::scale_aqueous(), WS_TABSTOP | WS_GROUP | BS_AUTORADIOBUTTON as u32, [CX, r.y + 1, 90, 22], SCALE_AQR, false),
        make(&r.a.cx, r.pane(), "BUTTON", t::scale_zstd(), WS_TABSTOP | BS_AUTORADIOBUTTON as u32, [CX + 96, r.y + 1, 90, 22], SCALE_ZSTD, false),
    );
    r.reg(SCALE_AQR, a1, true, tipt);
    r.reg(SCALE_ZSTD, a2, true, tipt);
    r.y += 28;
    r.slider(ZL, t::zstd_level(), 21, true, true, tipt);

    r.header(t::section_contents());
    let tx = t::tip_skip_names();
    r.label(t::skip_names(), false, false, tx);
    r.y += 24;
    let list = make(&r.a.cx, r.pane(), "LISTBOX", "", WS_TABSTOP | WS_BORDER | WS_VSCROLL | LBS_NOTIFY as u32, [LX, r.y, 530, 70], EXLIST, false);
    r.reg(EXLIST, list, false, tx);
    r.y += 76;
    let ed = make(&r.a.cx, r.pane(), "EDIT", "", WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32, [LX, r.y, 330, 22], EXEDIT, false);
    r.reg(EXEDIT, ed, false, t::tip_skip_edit());
    let add = make(&r.a.cx, r.pane(), "BUTTON", t::add(), WS_TABSTOP | BS_PUSHBUTTON as u32, [LX + 336, r.y - 1, 90, 24], EXADD, false);
    r.reg(EXADD, add, false, "");
    let del = make(&r.a.cx, r.pane(), "BUTTON", t::remove(), WS_TABSTOP | BS_PUSHBUTTON as u32, [LX + 432, r.y - 1, 98, 24], EXDEL, false);
    r.reg(EXDEL, del, false, "");
    r.y += 32;
    r.check(DEREF, t::follow_links(), false, t::tip_follow_links());
    r.check(PRESERVE, t::keep_times(), false, t::tip_keep_times());

    r.header(t::section_layout());
    r.slider(BATCHES, t::analyze_together(), 5, false, true, t::tip_analyze_together());
    r.slider(BLOCKS, t::largest_block(), 6, false, true, t::tip_largest_block());
    r.slider(SEGMENTS, t::close_segment(), 6, false, true, t::tip_close_segment());
    r.check(DICT, t::dictionaries(), true, t::tip_dictionaries());

    r.header(t::section_safety());
    r.check(ENCRYPT, t::passphrase(), false, t::tip_passphrase());
    r.check(ARCHIVAL, t::archival(), true, t::tip_archival());
    r.check(CHECK, t::verify(), true, t::tip_verify());
    r.check(ATOMIC, t::keep_nothing(), true, t::tip_keep_nothing());

    r.header(t::section_output());
    let modes: Vec<String> = modes().iter().map(|s| s.to_string()).collect();
    r.combo(MODE, t::if_exists(), &modes, false, false, t::tip_if_exists());
    r.edit(JOURNAL, t::version_name(), t::tip_version_name());
    r.check(RM, t::delete_originals(), false, t::tip_delete_originals());
    let warn = make(&r.a.cx, r.pane(), "STATIC", t::verify_warning(), SS_LEFT as u32, [LX + 20, r.y, 510, 18], WARN, false);
    r.a.ctl.insert(WARN, warn);
    r.y += 24;

    r.header(t::section_resources());
    let threads: Vec<String> = THREADS.iter().map(|&n| if n == 0 { t::threads_all(cpus) } else { n.to_string() }).collect();
    r.combo(THREAD, t::threads(), &threads, false, false, t::tip_threads());
    let mems: Vec<String> = mem_labels().iter().map(|m| m.to_string()).collect();
    r.combo(MEMORY, t::memory(), &mems, false, false, t::tip_memory());
    r.slider(APPBOUND, t::module_memory(), 4, true, false, t::tip_module_memory());

    r.header(t::section_command_line());
    let cli = make(
        &r.a.cx,
        r.pane(),
        "EDIT",
        "",
        WS_TABSTOP | WS_BORDER | (ES_MULTILINE | ES_READONLY) as u32,
        [LX, r.y, 530, 64],
        CLI,
        false,
    );
    r.a.ctl.insert(CLI, cli);
    r.a.pane_tab.push(cli);
    r.y += 64 + 16;
    r.a.content_h = r.a.cx.s(r.y);
}

// ---------- state to controls ----------

fn slider_value(o: &Opts, id: i32) -> i32 {
    match id {
        DEPTH => o.depth,
        BATCHES => o.batch,
        BLOCKS => o.block,
        SEGMENTS => o.segment,
        APPBOUND => o.appb,
        _ => o.zl - 1,
    }
}

fn slider_text(o: &Opts, id: i32) -> String {
    let d = Opts::new();
    let dflt = |text: String, same: bool| if same { t::value_default(text) } else { text };
    match id {
        DEPTH if o.depth == 0 => t::off().into(),
        DEPTH => dflt(o.depth.to_string(), o.depth == d.depth),
        BATCHES => dflt(pretty(BATCH[o.batch as usize]), o.batch == d.batch),
        BLOCKS => dflt(pretty(BLOCK[o.block as usize]), o.block == d.block),
        SEGMENTS => dflt(pretty(SEGMENT[o.segment as usize]), o.segment == d.segment),
        APPBOUND if o.appb == 4 => t::unlimited().into(),
        APPBOUND => dflt(pretty(APPB[o.appb as usize]), o.appb == d.appb),
        _ if o.zl > 3 => t::zstd_slow(o.zl),
        _ => t::zstd_value(o.zl),
    }
}

fn set_check(h: HWND, on: bool) {
    // SAFETY: a button of this thread
    unsafe { SendMessageW(h, BM_SETCHECK, if on { BST_CHECKED } else { BST_UNCHECKED } as usize, 0) };
}

fn refresh(a: &mut App) {
    let o = a.o.clone();
    // (what bears on the inventory changed: asked again, the answer to the last no more wanted)
    let gather = o.gather_args(&a.inputs);
    if let Some(job) = &a.job
        && gather != a.req_args
    {
        a.req += 1;
        job.request_inventory(a.req, &gather);
        a.req_args = gather;
    }
    let aqr = o.aqr();
    let c = |id: i32| a.ctl[&id];
    let sel = |id: i32, i: usize| {
        // SAFETY: a combo box of this thread
        unsafe { SendMessageW(c(id), CB_SETCURSEL, i, 0) };
    };
    // SAFETY: windows of this thread, strings of NULs
    unsafe {
        let preset = (0..PRESET_COUNT).find(|&i| o.same(&Opts::preset(i, &o.exclude))).unwrap_or(PRESET_COUNT);
        sel(PRESET, preset);
        sel(FORMAT, FORMATS.iter().position(|f| f.0 == o.format).unwrap());
        // the level
        let lv = c(LEVEL);
        SendMessageW(lv, CB_RESETCONTENT, 0, 0);
        let add = |s: &str| SendMessageW(lv, CB_ADDSTRING, 0, wide(s).as_ptr() as isize);
        match levels(o.format) {
            None => {
                add(t::no_level());
                EnableWindow(lv, 0);
                SendMessageW(lv, CB_SETCURSEL, 0, 0);
            }
            Some(_) if aqr && o.zstd_scale => {
                add(&t::level_zstd(o.zl));
                EnableWindow(lv, 0);
                SendMessageW(lv, CB_SETCURSEL, 0, 0);
            }
            Some((lo, hi, def)) => {
                for l in lo..=hi {
                    if aqr {
                        add(&level_name(l));
                    } else if l == def {
                        add(&t::level_default(l));
                    } else {
                        add(&l.to_string());
                    }
                }
                EnableWindow(lv, 1);
                SendMessageW(lv, CB_SETCURSEL, (o.level - lo) as usize, 0);
            }
        }
        // sliders and their values
        for id in SLIDERS {
            SendMessageW(c(id), TBM_SETPOS, 1, slider_value(&o, id) as isize);
            set_text(c(id + VALUE), &slider_text(&o, id));
        }
        // the level scale
        set_check(c(SCALE_AQR), !o.zstd_scale);
        set_check(c(SCALE_ZSTD), o.zstd_scale);
        for (id, on) in [
            (DEREF, o.deref),
            (PRESERVE, o.preserve),
            (DICT, o.dict),
            (ENCRYPT, o.encrypt),
            (ARCHIVAL, o.archival),
            (CHECK, o.check),
            (ATOMIC, o.atomic),
            (RM, o.rm),
        ] {
            set_check(c(id), on);
        }
        SendMessageW(c(MODE), CB_SETCURSEL, o.mode as usize, 0);
        SendMessageW(c(THREAD), CB_SETCURSEL, o.threads as usize, 0);
        SendMessageW(c(MEMORY), CB_SETCURSEL, o.mem as usize, 0);
        set_text(c(JOURNAL), &o.journal);
        // the exclusions
        let list = c(EXLIST);
        SendMessageW(list, LB_RESETCONTENT, 0, 0);
        for x in &o.exclude {
            SendMessageW(list, LB_ADDSTRING, 0, wide(x).as_ptr() as isize);
        }
        // what only Aqueous archives take
        for &h in &a.aqr_only {
            EnableWindow(h, aqr as i32);
        }
        EnableWindow(c(ZL), (aqr && o.zstd_scale) as i32);
        EnableWindow(c(ZL + VALUE), (aqr && o.zstd_scale) as i32);
        ShowWindow(c(WARN), if aqr && !o.check && o.rm { SW_SHOW } else { SW_HIDE });
        // texts
        set_text(c(SUMMARY), &a.names.join(", "));
        set_text(c(SUMMARY + 1000), &a.summary());
        let n = o.changes();
        for (i, h) in a.headers.iter().take(SECTIONS).enumerate() {
            set_text(*h, &if n[i] > 0 { t::section_n(section(i), n[i]) } else { section(i).into() });
        }
        let total: usize = n.iter().sum();
        let name = if total > 0 { t::advanced_n(total) } else { t::advanced().into() };
        let label = format!("{name}   {}", if a.open { "<<" } else { ">>" });
        set_text(c(TOGGLE), &label);
        set_text(c(CLI), &o.cli(&a.out_name(), &a.names));
    }
}

// ---------- messages ----------

/// A control changed: `id` told by `code` (a notification, or 0 for a slider).
fn changed(a: &mut App, id: i32, code: u32) {
    // SAFETY: windows of this thread
    unsafe {
        let get = |id: i32, msg: u32| SendMessageW(a.ctl[&id], msg, 0, 0);
        let checked = |id: i32| get(id, BM_GETCHECK) == BST_CHECKED as isize;
        let sel = |id: i32| get(id, CB_GETCURSEL).max(0) as i32;
        match id {
            PRESET if code == CBN_SELCHANGE => {
                let i = sel(PRESET) as usize;
                if i < PRESET_COUNT {
                    a.o = Opts::preset(i, &a.o.exclude);
                }
            }
            FORMAT if code == CBN_SELCHANGE => {
                a.o.format = FORMATS[sel(FORMAT) as usize].0;
                a.o.zstd_scale = false;
                if let Some((_, _, def)) = levels(a.o.format) {
                    a.o.level = def;
                }
            }
            LEVEL if code == CBN_SELCHANGE => {
                if let Some((lo, _, _)) = levels(a.o.format) {
                    a.o.level = lo + sel(LEVEL);
                }
            }
            SCALE_AQR | SCALE_ZSTD => a.o.zstd_scale = id == SCALE_ZSTD,
            DEPTH | BATCHES | BLOCKS | SEGMENTS | APPBOUND | ZL => {
                let v = get(id, TBM_GETPOS) as i32;
                match id {
                    DEPTH => a.o.depth = v,
                    BATCHES => a.o.batch = v,
                    BLOCKS => a.o.block = v,
                    SEGMENTS => a.o.segment = v,
                    APPBOUND => a.o.appb = v,
                    _ => a.o.zl = v + 1,
                }
            }
            DEREF => a.o.deref = checked(id),
            PRESERVE => a.o.preserve = checked(id),
            DICT => a.o.dict = checked(id),
            ENCRYPT => a.o.encrypt = checked(id),
            ARCHIVAL => a.o.archival = checked(id),
            CHECK => a.o.check = checked(id),
            ATOMIC => a.o.atomic = checked(id),
            RM => a.o.rm = checked(id),
            MODE if code == CBN_SELCHANGE => a.o.mode = sel(MODE),
            THREAD if code == CBN_SELCHANGE => a.o.threads = sel(THREAD),
            MEMORY if code == CBN_SELCHANGE => a.o.mem = sel(MEMORY),
            JOURNAL if code == 0x0300 => a.o.journal = text_of(a.ctl[&JOURNAL]),
            EXADD => {
                let v = text_of(a.ctl[&EXEDIT]).trim().to_string();
                if !v.is_empty() && !a.o.exclude.contains(&v) {
                    a.o.exclude.push(v);
                    set_text(a.ctl[&EXEDIT], "");
                }
            }
            EXDEL => {
                let i = get(EXLIST, LB_GETCURSEL);
                if i >= 0 && (i as usize) < a.o.exclude.len() {
                    a.o.exclude.remove(i as usize);
                }
            }
            RESET => {
                let keep = std::mem::take(&mut a.o.exclude);
                a.o = Opts::new();
                a.o.exclude = keep;
            }
            TOGGLE => {
                a.open = !a.open;
                ShowWindow(a.pane, if a.open { SW_SHOW } else { SW_HIDE });
                let (w, h) = frame(a, a.open);
                SetWindowPos(a.main, null_mut(), 0, 0, w, h, 0x0002 | 0x0004 | 0x0010);
            }
            IDOK if a.job.is_some() => {
                let out = a.out_dir.join(a.out_name());
                a.go = Some(a.o.args(&out.to_string_lossy(), &a.inputs));
                DestroyWindow(a.main);
                return;
            }
            IDOK => {
                let cli = a.o.cli(&a.out_name(), &a.names);
                MessageBoxW(a.main, wide(&cli).as_ptr(), wide(t::preview_title()).as_ptr(), MB_OK | MB_ICONINFORMATION);
            }
            IDCANCEL => {
                DestroyWindow(a.main);
                return;
            }
            _ => return,
        }
    }
    refresh(a);
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    // (a message sent while the state is being changed, as by SetWindowText, is left alone)
    APP.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn pane_color() -> u32 {
    // SAFETY: a system color
    let c = unsafe { GetSysColor(COLOR_3DFACE) };
    let half = |v: u32| (v & 0xff).midpoint(255);
    half(c) | half(c >> 8) << 8 | half(c >> 16) << 16
}

/// The brush of the options pane: the dialog's face color halfway to white, so that the pane
/// stands apart from the column beside it.
fn pane_brush() -> *mut c_void {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static BRUSH: AtomicUsize = AtomicUsize::new(0);
    if BRUSH.load(Ordering::Relaxed) == 0 {
        // SAFETY: a system color and a brush kept for the life of the program
        let b = unsafe { CreateSolidBrush(pane_color()) };
        BRUSH.store(b as usize, Ordering::Relaxed);
    }
    BRUSH.load(Ordering::Relaxed) as *mut c_void
}

/// A static control's background in the pane.
fn pane_static(wp: WPARAM) -> LRESULT {
    let brush = pane_brush();
    // SAFETY: the device context a static control gave, and the brush above
    unsafe {
        SetBkColor(wp as HDC, pane_color());
    }
    brush as LRESULT
}

fn face_brush(wp: WPARAM) -> LRESULT {
    // SAFETY: the device context a static control gave
    unsafe {
        SetBkColor(wp as HDC, GetSysColor(COLOR_3DFACE));
        GetSysColorBrush(COLOR_3DFACE) as LRESULT
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let (id, code) = ((wp & 0xffff) as i32, (wp >> 16) as u32);
            with_app(|a| changed(a, id, code));
            0
        }
        WM_CTLCOLORSTATIC => face_brush(wp),
        WM_TIMER => {
            with_app(poll);
            0
        }
        WM_CLOSE => {
            // SAFETY: this window
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: ends the loop
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: the defaults, for the message as it came
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe extern "system" fn pane_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let (id, code) = ((wp & 0xffff) as i32, (wp >> 16) as u32);
            // (the Esc key of the dialog manager comes here)
            with_app(|a| changed(a, id, code));
            0
        }
        WM_HSCROLL => {
            // SAFETY: the trackbar that told
            let id = unsafe { GetDlgCtrlID(lp as HWND) };
            with_app(|a| changed(a, id, 0));
            0
        }
        WM_VSCROLL => {
            scroll(hwnd, (wp & 0xffff) as i32);
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xffff) as u16 as i16 as i32;
            scroll(hwnd, if delta > 0 { SB_LINEUP } else { SB_LINEDOWN });
            scroll(hwnd, if delta > 0 { SB_LINEUP } else { SB_LINEDOWN });
            scroll(hwnd, if delta > 0 { SB_LINEUP } else { SB_LINEDOWN });
            0
        }
        WM_CTLCOLORSTATIC => {
            // SAFETY: the control that asked
            let id = unsafe { GetDlgCtrlID(lp as HWND) };
            if id == CLI {
                // SAFETY: the defaults
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            } else {
                pane_static(wp)
            }
        }
        // SAFETY: the defaults, for the message as it came
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn set_scroll(pane: HWND, content: i32, page: i32) {
    // SAFETY: a struct of C's that zeroes fit
    unsafe {
        let mut si: SCROLLINFO = std::mem::zeroed();
        si.cbSize = size_of::<SCROLLINFO>() as u32;
        si.fMask = SIF_RANGE | SIF_PAGE | SIF_POS;
        si.nMin = 0;
        si.nMax = content - 1;
        si.nPage = page as u32;
        si.nPos = 0;
        SetScrollInfo(pane, SB_VERT, &si, 1);
    }
}

/// Scrolls the pane by a scroll bar's request.
fn scroll(pane: HWND, request: i32) {
    scroll_to(pane, |si, _| match request {
        SB_LINEUP => si.nPos - 40,
        SB_LINEDOWN => si.nPos + 40,
        SB_PAGEUP => si.nPos - si.nPage as i32,
        SB_PAGEDOWN => si.nPos + si.nPage as i32,
        SB_THUMBTRACK => si.nTrackPos,
        _ => si.nPos,
    });
}

/// Scrolls the pane to the position `f` gives of the scroll bar's state and its greatest position.
fn scroll_to(pane: HWND, f: impl FnOnce(&SCROLLINFO, i32) -> i32) {
    // SAFETY: the pane of this thread
    unsafe {
        let mut si: SCROLLINFO = std::mem::zeroed();
        si.cbSize = size_of::<SCROLLINFO>() as u32;
        si.fMask = SIF_ALL;
        GetScrollInfo(pane, SB_VERT, &mut si);
        let max = (si.nMax + 1 - si.nPage as i32).max(0);
        let pos = f(&si, max).clamp(0, max);
        if pos != si.nPos {
            ScrollWindowEx(pane, 0, si.nPos - pos, null(), null(), null_mut(), null_mut(), SW_SCROLLCHILDREN | SW_INVALIDATE | SW_ERASE);
            si.fMask = SIF_POS;
            si.nPos = pos;
            SetScrollInfo(pane, SB_VERT, &si, 1);
        }
    }
}

/// Scrolls the pane so that `h`, a control of it, is in view (the first control: to the top).
fn show(a: &App, h: HWND) {
    // SAFETY: windows of this thread, rectangles and points of ours
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        let mut c: RECT = std::mem::zeroed();
        GetWindowRect(h, &mut r);
        GetClientRect(a.pane, &mut c);
        let mut pt = [POINT { x: r.left, y: r.top }, POINT { x: r.right, y: r.bottom }];
        MapWindowPoints(null_mut(), a.pane, pt.as_mut_ptr(), 2);
        let margin = a.cx.s(8);
        if a.pane_tab.first() == Some(&h) {
            scroll_to(a.pane, |_, _| 0);
        } else if pt[0].y < 0 {
            scroll_to(a.pane, |si, _| si.nPos + pt[0].y - margin);
        } else if pt[1].y > c.bottom {
            scroll_to(a.pane, |si, _| si.nPos + pt[1].y - c.bottom + margin);
        }
    }
}

/// The Tab key: focus to the next control (the previous with Shift) in the order of the dialog:
/// preset, format, level, the toggle, the pane's controls where it is open, Compress, Cancel,
/// Reset. Whether the key was taken.
fn tab(a: &mut App, back: bool) -> bool {
    // SAFETY: windows of this thread
    unsafe {
        let focus = GetFocus();
        // (an open drop-down list takes the key itself)
        if SendMessageW(focus, 0x0157, 0, 0) == 1 {
            return false;
        }
        let c = |id: i32| a.ctl[&id];
        let mut order = vec![c(PRESET), c(FORMAT), c(LEVEL), c(TOGGLE)];
        order.extend(a.pane_tab.iter().copied());
        order.extend([c(IDOK), c(IDCANCEL), c(RESET)]);
        // (of a group of radio buttons the checked one takes the Tab, the arrows the rest)
        let off = |h: HWND| [SCALE_AQR, SCALE_ZSTD].iter().any(|&id| c(id) == h && SendMessageW(h, BM_GETCHECK, 0, 0) == 0);
        order.retain(|&h| IsWindowVisible(h) != 0 && IsWindowEnabled(h) != 0 && !off(h));
        if order.is_empty() {
            return false;
        }
        let n = order.len();
        let next = match order.iter().position(|&h| h == focus) {
            Some(i) if back => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        let h = order[next];
        SetFocus(h);
        if a.pane_tab.contains(&h) {
            show(a, h);
        }
        if class_of(h) == "EDIT" {
            SendMessageW(h, 0x00B1, 0, -1); // EM_SETSEL: all of it
        }
        true
    }
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 32];
    // SAFETY: a window of this thread and a buffer of 32 words
    let n = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW(h, buf.as_mut_ptr(), 32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize]).to_uppercase()
}

/// A wheel turned: only the control with the focus is moved by it. Over any other control of the
/// window the pane scrolls (over the left column nothing happens). Whether the message is left to
/// be dispatched, to the window under the cursor.
fn wheel(msg: &mut MSG, main: HWND, pane: HWND) -> bool {
    // SAFETY: windows of this thread
    unsafe {
        let pt = POINT { x: (msg.lParam & 0xffff) as i16 as i32, y: ((msg.lParam >> 16) & 0xffff) as i16 as i32 };
        let hover = WindowFromPoint(pt);
        // (a drop-down list is a window of its own)
        if hover.is_null() || GetAncestor(hover, GA_ROOT) != main {
            return true;
        }
        let focus = GetFocus();
        if hover == focus || IsChild(focus, hover) != 0 {
            msg.hwnd = hover;
            return true;
        }
        if hover == pane || IsChild(pane, hover) != 0 {
            SendMessageW(pane, WM_MOUSEWHEEL, msg.wParam, msg.lParam);
        }
        false
    }
}
