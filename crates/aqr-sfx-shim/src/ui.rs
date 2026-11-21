//! What the program tells and asks: on standard error and input, or in message boxes where it
//! has neither (of the subsystem of windows, or of the console run from no console with the
//! manifest's `consoleAllocationPolicy`: docs/encrypted-sfx-design.md, 4).

use std::io::Write;
use std::path::Path;

use aqr_sfx::Answer;

pub struct Ui {
    /// The title of the message boxes.
    pub title: String,
    /// Nothing asked (`-y`, the settings' `quiet`).
    pub quiet: bool,
    /// The image above the prompt and the license in their dialogs (the settings' banner).
    pub banner: Option<Vec<u8>>,
    /// Of message boxes: no standard error.
    gui: bool,
}

impl Ui {
    pub fn new() -> Ui {
        Ui { title: crate::program_name(), quiet: false, banner: None, gui: no_stderr() }
    }

    /// Tells failure `e`.
    pub fn error(&self, e: &str) {
        if self.gui {
            message(&self.title, e, MB_ICONERROR);
        } else {
            eprintln!("aqr-sfx: {e}");
        }
    }

    /// Tells `m` (the settings' `finish`).
    pub fn tell(&self, m: &str) {
        if self.gui {
            message(&self.title, m, MB_ICONINFORMATION);
        } else {
            eprintln!("{m}");
        }
    }

    /// Notes `m` on standard error, where there is one (nothing in a box).
    pub fn note(&self, m: &str) {
        if !self.gui {
            eprintln!("aqr-sfx: {m}");
        }
    }

    /// Asks whether license `text` is agreed to: shown and asked on standard error and input
    /// (no where it ends), else in a dialog of its text.
    pub fn license(&mut self, text: &str) -> bool {
        if self.gui {
            return dialog(&self.title, text, Asking::License, self.banner.as_deref());
        }
        eprintln!("{}", text.trim_end());
        eprintln!();
        self.confirm("Do you accept the license?")
    }

    /// Asks the settings' prompt `q` as [`Ui::confirm`], in a dialog under the banner where there
    /// is one.
    pub fn prompt(&mut self, q: &str) -> bool {
        match &self.banner {
            Some(b) if self.gui => dialog(&self.title, q, Asking::Question, Some(b)),
            _ => self.confirm(q),
        }
    }

    /// Whether what is asked is asked in windows (no standard error).
    pub fn windowed(&self) -> bool {
        self.gui
    }

    /// Asks `q`, yes or no (no where standard input ends).
    pub fn confirm(&mut self, q: &str) -> bool {
        if self.gui {
            return message(&self.title, q, MB_YESNO | MB_ICONQUESTION) == IDYES;
        }
        loop {
            match line(&format!("{q} [y/n] ")).as_deref() {
                Some("y" | "yes") => return true,
                Some("n" | "no") | None => return false,
                _ => {}
            }
        }
    }

    /// Asks whether `path`, there already, is written over (quit where standard input ends).
    pub fn overwrite(&mut self, path: &Path) -> Answer {
        let p = path.display();
        if self.gui {
            let q = format!("{p}\n\nThere already: write it over?\n(Cancel stops.)");
            return match message(&self.title, &q, MB_YESNOCANCEL | MB_ICONQUESTION) {
                IDYES => Answer::Yes,
                IDNO => Answer::No,
                _ => Answer::Quit,
            };
        }
        loop {
            match line(&format!("{p}: there already. Write it over? [y]es, [n]o, [a]lways, ne[v]er, [q]uit "))
                .as_deref()
            {
                Some("y" | "yes") => return Answer::Yes,
                Some("n" | "no") => return Answer::No,
                Some("a" | "always") => return Answer::All,
                Some("v" | "never") => return Answer::None,
                Some("q" | "quit") | None => return Answer::Quit,
                _ => {}
            }
        }
    }
}

/// A line of standard input after prompt `p` on standard error, trimmed and in lower case;
/// `None` where it ends.
fn line(p: &str) -> Option<String> {
    eprint!("{p}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    match std::io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim().to_lowercase()),
    }
}

/// The language of the user's interface as a tag (`ko-KR`): `AQR_SFX_LANG` where it is set,
/// else the system's (Windows: the user's language of its interface; elsewhere `LANG`).
pub fn language() -> String {
    if let Some(l) = std::env::var_os("AQR_SFX_LANG") {
        return l.to_string_lossy().into_owned();
    }
    system_language()
}

#[cfg(windows)]
fn system_language() -> String {
    let mut buf = [0u16; 85];
    // SAFETY: a buffer of LOCALE_NAME_MAX_LENGTH
    let n = unsafe { sys::LCIDToLocaleName(sys::GetUserDefaultUILanguage() as u32, buf.as_mut_ptr(), 85, 0) };
    String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize])
}

#[cfg(not(windows))]
fn system_language() -> String {
    let l = std::env::var("LANG").unwrap_or_default();
    l.split(['.', '@']).next().unwrap_or("").replace('_', "-")
}

/// What a [`dialog`] asks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asking {
    /// A license agreed to: its text read-only and scrolled, Accept and Decline.
    License,
    /// A question: Yes and No.
    Question,
}

/// What a [`dialog`]'s procedure takes.
#[cfg(windows)]
struct Shown<'a> {
    text: Vec<u16>,
    asking: Asking,
    banner: Option<&'a [u8]>,
    /// The banner's bitmap made, freed once the dialog ends.
    bitmap: std::cell::Cell<*mut c_void>,
}

/// A dialog of `text` asking `asking` (a template made in memory: user32 alone, and WIC for the
/// banner), under `banner` where there is one: laid out once its banner and text are measured.
/// Whether it was agreed to (Accept, Yes).
#[cfg(windows)]
fn dialog(title: &str, text: &str, asking: Asking, banner: Option<&[u8]>) -> bool {
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let mut t: Vec<u16> = Vec::new();
    let dword = |t: &mut Vec<u16>, v: u32| t.extend_from_slice(&[v as u16, (v >> 16) as u16]);
    let child = 0x4000_0000 | 0x1000_0000;
    let tab = 0x0001_0000;
    let (yes, no) = if asking == Asking::License { ("Accept", "Decline") } else { ("Yes", "No") };
    let mut items: Vec<(u32, [u16; 4], u16, u16, &str)> = vec![
        match asking {
            // EDIT: ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY, WS_VSCROLL, WS_BORDER
            Asking::License => {
                (child | tab | 0x4 | 0x40 | 0x800 | 0x0020_0000 | 0x0080_0000, [7, 7, 306, 170], 100, 0x81, "")
            }
            // STATIC: SS_NOPREFIX (its height as its text measures)
            Asking::Question => (child | 0x80, [7, 7, 266, 8], 100, 0x82, ""),
        },
        // BUTTON: BS_DEFPUSHBUTTON (where they go laid out)
        (child | tab | 0x1, [0, 0, 52, 15], 1, 0x80, yes),
        (child | tab, [0, 0, 52, 15], 2, 0x80, no),
    ];
    if banner.is_some() {
        // STATIC: SS_BITMAP | SS_CENTERIMAGE
        items.push((child | 0xe | 0x200, [0, 0, 1, 1], 101, 0x82, ""));
    }
    // DLGTEMPLATE: WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_CENTER | DS_MODALFRAME | DS_SETFONT
    dword(&mut t, 0x8000_0000 | 0x00c0_0000 | 0x0008_0000 | 0x800 | 0x80 | 0x40);
    dword(&mut t, 0);
    let width = if asking == Asking::License { 320 } else { 280 };
    t.extend_from_slice(&[items.len() as u16, 0, 0, width, 100]);
    // (no menu, the default class), the title, the font: the message boxes'
    t.extend_from_slice(&[0, 0]);
    t.extend(wide(title));
    let (points, face) = message_font();
    t.push(points);
    t.extend(face);
    // the items, each at 4 bytes: style, x, y, width, height, id, class (an atom), its text
    for (style, [x, y, w, h], id, class, label) in items {
        if t.len() % 2 == 1 {
            t.push(0);
        }
        dword(&mut t, style);
        dword(&mut t, 0);
        t.extend_from_slice(&[x, y, w, h, id, 0xffff, class]);
        t.extend(wide(label));
        t.push(0);
    }
    // (the template at 4 bytes)
    let template: Vec<u32> = t.chunks(2).map(|c| c[0] as u32 | (*c.get(1).unwrap_or(&0) as u32) << 16).collect();
    let shown = Shown {
        text: wide(&text.replace("\r\n", "\n").replace('\n', "\r\n")),
        asking,
        banner,
        bitmap: std::cell::Cell::new(std::ptr::null_mut()),
    };

    extern "system" fn proc(h: *mut c_void, msg: u32, wp: usize, lp: isize) -> isize {
        // SAFETY: the dialog's window; WM_INITDIALOG's parameter the `Shown` given below
        unsafe {
            match msg {
                0x110 => {
                    let s = &*(lp as *const Shown);
                    sys::SetDlgItemTextW(h, 100, s.text.as_ptr());
                    lay_out(h, s);
                    sys::SetFocus(sys::GetDlgItem(h, 1));
                    0
                }
                0x111 if matches!(wp & 0xffff, 1 | 2) => {
                    sys::EndDialog(h, (wp & 0xffff) as isize);
                    1
                }
                _ => 0,
            }
        }
    }
    // SAFETY: a template of the layout DialogBoxIndirectParamW reads, at 4 bytes; what is shown
    // alive as long as the dialog is
    let r = unsafe {
        sys::DialogBoxIndirectParamW(
            sys::GetModuleHandleW(std::ptr::null()),
            template.as_ptr().cast(),
            std::ptr::null_mut(),
            proc,
            (&raw const shown) as isize,
        )
    };
    if !shown.bitmap.get().is_null() {
        crate::banner::free(shown.bitmap.get());
    }
    r == 1
}

/// The font of the message boxes (`NONCLIENTMETRICS`' `lfMessageFont`: Segoe UI, its Hangul
/// and the like of fonts linked to it, as Malgun Gothic), its size in points and its face
/// (NUL-terminated); `MS Shell Dlg` at 9 where it is not told (whose Hangul is Gulim's
/// bitmaps at that size).
#[cfg(windows)]
fn message_font() -> (u16, Vec<u16>) {
    // NONCLIENTMETRICSW of Vista on (504 bytes): lfMessageFont, a LOGFONTW, at 408 (its height
    // first, its face at 28, of 32 characters)
    let mut m = [0u32; 126];
    m[0] = 504;
    // SAFETY: SPI_GETNONCLIENTMETRICS into a buffer of the size its first field tells; a DC of
    // the screen released
    let (got, per_inch) = unsafe {
        let got = sys::SystemParametersInfoW(0x29, 504, m.as_mut_ptr().cast(), 0);
        let dc = sys::GetDC(std::ptr::null_mut());
        let d = sys::GetDeviceCaps(dc, 90); // LOGPIXELSY
        sys::ReleaseDC(std::ptr::null_mut(), dc);
        (got, d)
    };
    let font = &m[102..125];
    let height = font[0] as i32;
    let face: Vec<u16> = font[7..].iter().flat_map(|&w| [w as u16, (w >> 16) as u16]).take_while(|&c| c != 0).collect();
    if got == 0 || face.is_empty() || height == 0 || per_inch <= 0 {
        return (9, "MS Shell Dlg".encode_utf16().chain([0]).collect());
    }
    let points = (height.unsigned_abs() * 72 + per_inch as u32 / 2) / per_inch as u32;
    (points.clamp(6, 24) as u16, face.into_iter().chain([0]).collect())
}

/// Lays dialog `h` out (in pixels): the banner across its top (centred, the rest of its first
/// pixel's colour), the text under it (a question's as high as it measures), the buttons at
/// the bottom right; the dialog as wide as the banner where that is wider, about where it was.
#[cfg(windows)]
unsafe fn lay_out(h: *mut c_void, s: &Shown) {
    use sys::Rect;
    // SAFETY: the dialog's window and its items of the template
    unsafe {
        let units = |x: i32, y: i32| {
            let mut r = Rect { left: 0, top: 0, right: x, bottom: y };
            sys::MapDialogRect(h, &mut r);
            (r.right, r.bottom)
        };
        let (mx, my) = units(7, 7);
        let (bw, bh) = units(52, 15);
        let (gap, _) = units(5, 0);
        let mut client = Rect::default();
        sys::GetClientRect(h, &mut client);
        let mut width = client.right;
        let mut top = 0;
        if let Some(d) = s.banner
            && let Some((b, w, bh)) = crate::banner::bitmap(d, crate::banner::dpi(h), sys::GetSysColor(15))
        {
            s.bitmap.set(b);
            let at = sys::GetDlgItem(h, 101);
            sys::SendMessageW(at, 0x172, 0, b as isize); // STM_SETIMAGE, IMAGE_BITMAP
            width = width.max(w);
            sys::MoveWindow(at, 0, 0, width, bh, 0);
            top = bh;
        }
        let body = sys::GetDlgItem(h, 100);
        let height = match s.asking {
            Asking::License => units(0, 170).1,
            Asking::Question => {
                // DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX | DT_EXPANDTABS, in the control's font
                let dc = sys::GetDC(body);
                let old = sys::SelectObject(dc, sys::SendMessageW(body, 0x31, 0, 0) as *mut c_void);
                let mut r = Rect { left: 0, top: 0, right: width - 2 * mx, bottom: 0 };
                sys::DrawTextW(dc, s.text.as_ptr(), -1, &mut r, 0x400 | 0x10 | 0x800 | 0x40);
                sys::SelectObject(dc, old);
                sys::ReleaseDC(body, dc);
                r.bottom.max(units(0, 8).1)
            }
        };
        sys::MoveWindow(body, mx, top + my, width - 2 * mx, height, 0);
        let y = top + my + height + my;
        sys::MoveWindow(sys::GetDlgItem(h, 2), width - mx - bw, y, bw, bh, 0);
        sys::MoveWindow(sys::GetDlgItem(h, 1), width - mx - 2 * bw - gap, y, bw, bh, 0);
        // the window of that client, of the same centre
        let mut r = Rect { left: 0, top: 0, right: width, bottom: y + bh + my };
        sys::AdjustWindowRectEx(&mut r, sys::GetWindowLongW(h, -16) as u32, 0, sys::GetWindowLongW(h, -20) as u32);
        let mut was = Rect::default();
        sys::GetWindowRect(h, &mut was);
        let (w, ht) = (r.right - r.left, r.bottom - r.top);
        let (cx, cy) = ((was.left + was.right) / 2, (was.top + was.bottom) / 2);
        // SWP_NOZORDER | SWP_NOACTIVATE
        sys::SetWindowPos(h, std::ptr::null_mut(), cx - w / 2, (cy - ht / 2).max(0), w, ht, 0x4 | 0x10);
    }
}

#[cfg(not(windows))]
fn dialog(_: &str, _: &str, _: Asking, _: Option<&[u8]>) -> bool {
    false
}

#[cfg(windows)]
use core::ffi::c_void;

const MB_YESNOCANCEL: u32 = 0x3;
const MB_YESNO: u32 = 0x4;
const MB_ICONERROR: u32 = 0x10;
const MB_ICONQUESTION: u32 = 0x20;
const MB_ICONINFORMATION: u32 = 0x40;
const IDYES: i32 = 6;
const IDNO: i32 = 7;

#[cfg(windows)]
mod sys {
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn MessageBoxW(window: *mut core::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
        pub fn DialogBoxIndirectParamW(
            instance: *mut core::ffi::c_void,
            template: *const core::ffi::c_void,
            parent: *mut core::ffi::c_void,
            proc: extern "system" fn(*mut core::ffi::c_void, u32, usize, isize) -> isize,
            param: isize,
        ) -> isize;
        pub fn SetDlgItemTextW(dialog: *mut core::ffi::c_void, id: i32, text: *const u16) -> i32;
        pub fn GetDlgItem(dialog: *mut core::ffi::c_void, id: i32) -> *mut core::ffi::c_void;
        pub fn SetFocus(window: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        pub fn EndDialog(dialog: *mut core::ffi::c_void, result: isize) -> i32;
        pub fn MapDialogRect(dialog: *mut core::ffi::c_void, r: *mut Rect) -> i32;
        pub fn GetClientRect(window: *mut core::ffi::c_void, r: *mut Rect) -> i32;
        pub fn GetWindowRect(window: *mut core::ffi::c_void, r: *mut Rect) -> i32;
        pub fn MoveWindow(window: *mut core::ffi::c_void, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
        pub fn SetWindowPos(
            window: *mut core::ffi::c_void,
            after: *mut core::ffi::c_void,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
        pub fn AdjustWindowRectEx(r: *mut Rect, style: u32, menu: i32, ex_style: u32) -> i32;
        pub fn GetWindowLongW(window: *mut core::ffi::c_void, index: i32) -> i32;
        pub fn SendMessageW(window: *mut core::ffi::c_void, msg: u32, wp: usize, lp: isize) -> isize;
        pub fn GetSysColor(index: i32) -> u32;
        pub fn GetDC(window: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        pub fn ReleaseDC(window: *mut core::ffi::c_void, dc: *mut core::ffi::c_void) -> i32;
        pub fn DrawTextW(dc: *mut core::ffi::c_void, text: *const u16, n: i32, r: *mut Rect, format: u32) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        pub fn SelectObject(dc: *mut core::ffi::c_void, o: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        pub fn GetDeviceCaps(dc: *mut core::ffi::c_void, index: i32) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn SystemParametersInfoW(action: u32, param: u32, data: *mut core::ffi::c_void, flags: u32) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetStdHandle(which: u32) -> *mut core::ffi::c_void;
        pub fn GetModuleHandleW(name: *const u16) -> *mut core::ffi::c_void;
        pub fn GetUserDefaultUILanguage() -> u16;
        pub fn LCIDToLocaleName(lcid: u32, name: *mut u16, len: i32, flags: u32) -> i32;
    }
}

/// A message box of `text`, of buttons and icon `kind`; the button pressed.
#[cfg(windows)]
fn message(title: &str, text: &str, kind: u32) -> i32 {
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide(title));
    // (MB_SETFOREGROUND: in front of the window it was run from)
    // SAFETY: NUL-terminated strings, no owner window
    unsafe { sys::MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), kind | 0x10000) }
}

/// Whether the program has no standard error: run as a program of windows, or of the console
/// with no console made for it.
#[cfg(windows)]
fn no_stderr() -> bool {
    // SAFETY: STD_ERROR_HANDLE
    let h = unsafe { sys::GetStdHandle(-12i32 as u32) };
    h.is_null() || h as isize == -1
}

#[cfg(not(windows))]
fn message(_: &str, text: &str, _: u32) -> i32 {
    eprintln!("{text}");
    IDNO
}

#[cfg(not(windows))]
fn no_stderr() -> bool {
    false
}
