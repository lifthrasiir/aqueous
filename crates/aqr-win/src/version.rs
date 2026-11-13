//! The versions of an archive (the points of its journal, `docs/append-design.md`, "Journaling"):
//!
//! - `name-version ARCHIVE`: a dialog asks for a name, and the state the archive is in is named
//!   by it (`aqr --tag`, which fills in `{today}` and `{now}`). The name is today's date by
//!   default.
//! - `extract-version ARCHIVE`: a dialog lists the versions named (`aqr --history --json`), the
//!   last chosen, and the one chosen is extracted as it was (`aqr -d --at`) into a folder beside
//!   the archive named after it ([`aqr_ipc::version_folder`]), with the window of how far it is
//!   ([`crate::run::run_at`]).
//!
//! The dialogs are modal ones of a template made in memory ([`crate::passphrase::Template`]): task
//! dialogs have no edit field, and a list of many versions wants a list box.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, DialogBoxIndirectParamW, ES_AUTOHSCROLL, EndDialog, GetDlgItem, IDCANCEL, IDOK,
    LB_ADDSTRING, LB_ERR, LB_GETCURSEL, LB_SETCURSEL, LBN_DBLCLK, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, SendMessageW,
    WM_COMMAND, WM_INITDIALOG, WS_BORDER, WS_TABSTOP, WS_VSCROLL,
};

use aqr_ipc::{Point, file_name};
use aqr_l10n::{common, shell, win};

use crate::dialog::{self, Icon, wide};
use crate::passphrase::{BUTTON, EDIT, STATIC, Template, text};

/// The class of a list box, as an atom of a template.
const LISTBOX: u16 = 0x0083;

/// The ids of the controls.
const MESSAGE: i32 = 100;
const FIELD: i32 = 101;

/// What the name dialog's procedure answers and the choice dialog's works on (a dialog is modal,
/// on one thread).
#[derive(Default)]
struct State {
    /// The rows of the list, the index chosen, the name given.
    rows: Vec<String>,
    chosen: Option<usize>,
    name: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// `name-version`: asks for a name for the state `archive` is in, and names it.
pub fn name(archive: PathBuf) -> ExitCode {
    let shown = file_name(&archive);
    let Some(label) = ask_name(std::ptr::null_mut(), &shown) else { return ExitCode::SUCCESS };
    match aqr_ipc::tag(&aqr_ipc::aqr(), &archive, &label) {
        Ok(told) => {
            dialog::message(Icon::Information, win::version_named(), &told, None);
            ExitCode::SUCCESS
        }
        Err(e) => {
            dialog::message(Icon::Error, win::could_not_name(), &e, None);
            ExitCode::FAILURE
        }
    }
}

/// `extract-version`: asks for a version of `archive`, and extracts it.
pub fn extract(archive: PathBuf) -> ExitCode {
    let shown = file_name(&archive);
    let points = match aqr_ipc::points(&aqr_ipc::aqr(), &archive) {
        Ok(p) => p,
        Err(e) => {
            dialog::message(Icon::Error, win::could_not_list(), &e, None);
            return ExitCode::FAILURE;
        }
    };
    if points.is_empty() {
        dialog::message(
            Icon::Information,
            &common::no_versions(&shown),
            &win::no_versions_info(shell::NAME_VERSION.text()),
            None,
        );
        return ExitCode::SUCCESS;
    }
    match choose(std::ptr::null_mut(), &shown, &points) {
        Some(k) => crate::run::run_at(&archive, &points[k]),
        None => ExitCode::SUCCESS,
    }
}

/// The dialog's template, `items` controls in a box of `w` by `h` dialog units, titled "Aqueous".
fn template(items: u16, w: i16, h: i16) -> Template {
    let mut t = Template::default();
    t.dialog(items, [w, h]);
    t
}

/// Shows the template `t` modal over `parent`, `proc` its procedure.
fn show(parent: HWND, t: Template, proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> isize) {
    let aligned = t.aligned();
    // SAFETY: a template made by `Template` (aligned, and complete), a procedure of the
    // signature, no data; the parent is a window of this thread or none
    unsafe {
        DialogBoxIndirectParamW(GetModuleHandleW(std::ptr::null()), aligned.as_ptr().cast(), parent, Some(proc), 0);
    }
}

/// Asks for the name of a version of archive `archive` (as it is shown) over `parent`: a field
/// of `{today}`, to be changed; `None` where the user cancels.
fn ask_name(parent: HWND, archive: &str) -> Option<String> {
    let mut t = template(5, 230, 81);
    t.item(0, [7, 7, 216, 24], MESSAGE, STATIC, &win::name_version_ask(archive));
    t.item(0, [7, 34, 30, 10], -1, STATIC, win::name_label());
    let style = (WS_BORDER | WS_TABSTOP) | ES_AUTOHSCROLL as u32;
    t.item(style, [40, 32, 183, 14], FIELD, EDIT, "{today}");
    t.item(WS_TABSTOP | BS_DEFPUSHBUTTON as u32, [117, 58, 50, 14], IDOK, BUTTON, common::ok());
    t.item(WS_TABSTOP | BS_PUSHBUTTON as u32, [173, 58, 50, 14], IDCANCEL, BUTTON, common::cancel());
    STATE.with(|s| *s.borrow_mut() = State::default());
    show(parent, t, name_proc);
    STATE.with(|s| s.borrow_mut().name.take())
}

unsafe extern "system" fn name_proc(hwnd: HWND, msg: u32, wp: WPARAM, _: LPARAM) -> isize {
    match msg {
        // (the focus is the first control that takes it: the field, its text selected)
        WM_INITDIALOG => 1,
        WM_COMMAND => match (wp & 0xffff) as i32 {
            IDOK => {
                STATE.with(|s| s.borrow_mut().name = Some(text(hwnd, FIELD)));
                // SAFETY: ends this dialog
                unsafe { EndDialog(hwnd, 1) };
                1
            }
            IDCANCEL => {
                // SAFETY: ends this dialog
                unsafe { EndDialog(hwnd, 0) };
                1
            }
            _ => 0,
        },
        _ => 0,
    }
}

/// Asks which of `points` (versions of archive `archive`, as it is shown) over `parent`, the last
/// chosen at first: its index; `None` where the user cancels.
fn choose(parent: HWND, archive: &str, points: &[Point]) -> Option<usize> {
    let mut t = template(4, 270, 160);
    t.item(0, [7, 7, 256, 12], MESSAGE, STATIC, &common::choose_version(archive));
    let style = (WS_BORDER | WS_TABSTOP | WS_VSCROLL) | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32;
    t.item(style, [7, 22, 256, 112], FIELD, LISTBOX, "");
    t.item(WS_TABSTOP | BS_DEFPUSHBUTTON as u32, [157, 140, 50, 14], IDOK, BUTTON, common::ok());
    t.item(WS_TABSTOP | BS_PUSHBUTTON as u32, [213, 140, 50, 14], IDCANCEL, BUTTON, common::cancel());
    STATE.with(|s| *s.borrow_mut() = State { rows: points.iter().map(Point::row).collect(), ..State::default() });
    show(parent, t, choose_proc);
    STATE.with(|s| s.borrow_mut().chosen.take())
}

unsafe extern "system" fn choose_proc(hwnd: HWND, msg: u32, wp: WPARAM, _: LPARAM) -> isize {
    // SAFETY: the dialog's own list box
    let list = || unsafe { GetDlgItem(hwnd, FIELD) };
    match msg {
        WM_INITDIALOG => {
            let rows = STATE.with(|s| s.borrow().rows.clone());
            for row in &rows {
                let w = wide(row);
                // SAFETY: a list box and a string of NULs, which it copies
                unsafe { SendMessageW(list(), LB_ADDSTRING, 0, w.as_ptr() as LPARAM) };
            }
            // SAFETY: the list box, its last row chosen
            unsafe { SendMessageW(list(), LB_SETCURSEL, rows.len().saturating_sub(1), 0) };
            1
        }
        WM_COMMAND => {
            let (id, note) = ((wp & 0xffff) as i32, ((wp >> 16) & 0xffff) as u32);
            match (id, note) {
                (IDOK, _) | (FIELD, LBN_DBLCLK) => {
                    // SAFETY: the list box
                    let at = unsafe { SendMessageW(list(), LB_GETCURSEL, 0, 0) };
                    if at != LB_ERR as isize {
                        STATE.with(|s| s.borrow_mut().chosen = Some(at as usize));
                        // SAFETY: ends this dialog
                        unsafe { EndDialog(hwnd, 1) };
                    }
                    1
                }
                (IDCANCEL, _) => {
                    // SAFETY: ends this dialog
                    unsafe { EndDialog(hwnd, 0) };
                    1
                }
                _ => 0,
            }
        }
        _ => 0,
    }
}
