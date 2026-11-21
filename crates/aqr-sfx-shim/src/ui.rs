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
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetStdHandle(which: u32) -> *mut core::ffi::c_void;
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
