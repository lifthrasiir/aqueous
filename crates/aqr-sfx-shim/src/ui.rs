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
    /// Of message boxes: no standard error.
    gui: bool,
}

impl Ui {
    pub fn new() -> Ui {
        Ui { title: crate::program_name(), quiet: false, gui: no_stderr() }
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
            return license_dialog(&self.title, text);
        }
        eprintln!("{}", text.trim_end());
        eprintln!();
        self.confirm("Do you accept the license?")
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

/// The dialog of a license (a template made in memory: user32 alone): its text read-only and
/// scrolled, Accept and Decline. Whether it was accepted.
#[cfg(windows)]
fn license_dialog(title: &str, text: &str) -> bool {
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let mut t: Vec<u16> = Vec::new();
    let dword = |t: &mut Vec<u16>, v: u32| t.extend_from_slice(&[v as u16, (v >> 16) as u16]);
    // DLGTEMPLATE: WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_CENTER | DS_MODALFRAME | DS_SHELLFONT
    dword(&mut t, 0x8000_0000 | 0x00c0_0000 | 0x0008_0000 | 0x800 | 0x80 | 0x48);
    dword(&mut t, 0);
    t.extend_from_slice(&[3, 0, 0, 320, 206]);
    // (no menu, the default class), the title, the font
    t.extend_from_slice(&[0, 0]);
    t.extend(wide(title));
    t.push(9);
    t.extend(wide("MS Shell Dlg"));
    // the items, each at 4 bytes: style, x, y, width, height, id, class (an atom), its text
    let child = 0x4000_0000 | 0x1000_0000 | 0x0001_0000;
    let items: [(u32, [u16; 4], u16, u16, &str); 3] = [
        // EDIT: ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY, WS_VSCROLL, WS_BORDER
        (child | 0x4 | 0x40 | 0x800 | 0x0020_0000 | 0x0080_0000, [7, 7, 306, 170], 100, 0x81, ""),
        // BUTTON: BS_DEFPUSHBUTTON
        (child | 0x1, [204, 184, 52, 15], 1, 0x80, "Accept"),
        (child, [261, 184, 52, 15], 2, 0x80, "Decline"),
    ];
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
    let text = wide(&text.replace("\r\n", "\n").replace('\n', "\r\n"));

    extern "system" fn proc(h: *mut c_void, msg: u32, wp: usize, lp: isize) -> isize {
        // SAFETY: the dialog's window; WM_INITDIALOG's parameter the text given below
        unsafe {
            match msg {
                0x110 => {
                    sys::SetDlgItemTextW(h, 100, lp as *const u16);
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
    // SAFETY: a template of the layout DialogBoxIndirectParamW reads, at 4 bytes; the text alive
    // as long as the dialog is
    let r = unsafe {
        sys::DialogBoxIndirectParamW(
            sys::GetModuleHandleW(std::ptr::null()),
            template.as_ptr().cast(),
            std::ptr::null_mut(),
            proc,
            text.as_ptr() as isize,
        )
    };
    r == 1
}

#[cfg(not(windows))]
fn license_dialog(_: &str, _: &str) -> bool {
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
