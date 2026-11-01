//! The passphrase dialog: a modal dialog of a template made in memory (task dialogs have no edit
//! controls), with a password field, or two where a passphrase is chosen (they must match to
//! be answered), OK and Cancel.

use std::cell::RefCell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, DS_CENTER, DS_MODALFRAME, DS_SETFONT, DialogBoxIndirectParamW, ES_AUTOHSCROLL,
    ES_PASSWORD, EndDialog, GetDlgItem, GetDlgItemTextW, GetWindowTextLengthW, IDCANCEL, IDOK, SendMessageW,
    SetDlgItemTextW, WM_COMMAND, WM_INITDIALOG, WM_NEXTDLGCTL, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

use crate::dialog::wide;

/// The ids of the controls.
const MESSAGE: i32 = 100;
const FIRST: i32 = 101;
const SECOND: i32 = 102;

/// The window classes of controls, as atoms of a template.
const BUTTON: u16 = 0x0080;
const EDIT: u16 = 0x0081;
const STATIC: u16 = 0x0082;

/// What the dialog procedure works on (a dialog is modal, on one thread).
struct State {
    confirm: bool,
    /// What it answers: the passphrase, once OK is clicked with the two matching.
    answer: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// A template being made: words, each item at a double word.
#[derive(Default)]
struct Template(Vec<u16>);

impl Template {
    fn word(&mut self, w: u16) {
        self.0.push(w);
    }

    fn dword(&mut self, d: u32) {
        self.0.extend([d as u16, (d >> 16) as u16]);
    }

    fn string(&mut self, s: &str) {
        self.0.extend(s.encode_utf16());
        self.0.push(0);
    }

    /// An item: style, place (x, y, width, height in dialog units), id, class, text.
    fn item(&mut self, style: u32, [x, y, w, h]: [i16; 4], id: i32, class: u16, text: &str) {
        if !self.0.len().is_multiple_of(2) {
            self.0.push(0);
        }
        self.dword(style | (WS_CHILD | WS_VISIBLE));
        self.dword(0);
        for v in [x, y, w, h] {
            self.word(v as u16);
        }
        self.word(id as u16);
        self.word(0xffff);
        self.word(class);
        self.string(text);
        // (no creation data)
        self.word(0);
    }
}

/// Asks for a passphrase over `parent`: a message (`again` where the last was wrong, `file`
/// the archive asked about), two fields where `confirm`. `None` where the user cancels.
pub fn ask(parent: HWND, file: Option<&str>, compress: bool, again: bool, confirm: bool) -> Option<String> {
    let mut message = String::new();
    if again {
        message += "Wrong passphrase, try again.\r\n";
    }
    message += &match (file, compress) {
        (Some(f), true) => format!("Choose a passphrase for {f}."),
        (Some(f), false) => format!("{f} is encrypted. Enter its passphrase."),
        (None, true) => "Choose a passphrase for the archive.".into(),
        (None, false) => "The archive is encrypted. Enter its passphrase.".into(),
    };
    let fields = if confirm { 2 } else { 1 };
    let buttons_y = 46 + 18 * fields;
    let mut t = Template::default();
    // (the dialog: no menu, the default class, a title, the font)
    t.dword((WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE) | (DS_MODALFRAME | DS_SETFONT | DS_CENTER) as u32);
    t.dword(0);
    t.word(3 + 2 * fields as u16);
    for v in [0i16, 0, 230, buttons_y as i16 + 21] {
        t.word(v as u16);
    }
    t.word(0);
    t.word(0);
    t.string("Aqueous");
    t.word(9);
    t.string("Segoe UI");
    t.item(0, [7, 7, 216, 30], MESSAGE, STATIC, &message);
    let label = |t: &mut Template, y: i16, id: i32, text: &str| {
        t.item(0, [7, y + 2, 66, 10], -1, STATIC, text);
        let style = (WS_BORDER | WS_TABSTOP) | (ES_PASSWORD | ES_AUTOHSCROLL) as u32;
        t.item(style, [76, y, 147, 14], id, EDIT, "");
    };
    label(&mut t, 42, FIRST, "Passphrase:");
    if confirm {
        label(&mut t, 60, SECOND, "Again:");
    }
    let by = buttons_y as i16;
    t.item(WS_TABSTOP | BS_DEFPUSHBUTTON as u32, [117, by, 50, 14], IDOK, BUTTON, "OK");
    t.item(WS_TABSTOP | BS_PUSHBUTTON as u32, [173, by, 50, 14], IDCANCEL, BUTTON, "Cancel");
    // (the template at a double word)
    t.0.resize(t.0.len().next_multiple_of(2), 0);
    let aligned: Vec<u32> = t.0.chunks(2).map(|c| c[0] as u32 | (c[1] as u32) << 16).collect();

    STATE.with(|s| *s.borrow_mut() = Some(State { confirm, answer: None }));
    // SAFETY: a template made above (aligned, and complete), a procedure of the signature, no
    // data; the parent is a window of this thread or none
    unsafe {
        DialogBoxIndirectParamW(GetModuleHandleW(std::ptr::null()), aligned.as_ptr().cast(), parent, Some(proc), 0);
    }
    STATE.with(|s| s.borrow_mut().take()).and_then(|s| s.answer)
}

/// The text of control `id` of `hwnd`.
fn text(hwnd: HWND, id: i32) -> String {
    // SAFETY: a control of the dialog, and room for its text and the NUL
    unsafe {
        let len = GetWindowTextLengthW(GetDlgItem(hwnd, id)).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetDlgItemTextW(hwnd, id, buf.as_mut_ptr(), buf.len() as i32) as usize;
        let s = String::from_utf16_lossy(&buf[..n.min(len)]);
        buf.fill(0);
        s
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, _: LPARAM) -> isize {
    match msg {
        // (the focus is the first control that takes it: the first field)
        WM_INITDIALOG => 1,
        WM_COMMAND => match (wp & 0xffff) as i32 {
            IDOK => {
                let confirm = STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.confirm));
                let value = text(hwnd, FIRST);
                if confirm && value != text(hwnd, SECOND) {
                    let differ = wide("The two passphrases differ, try again.");
                    let empty = wide("");
                    // SAFETY: the dialog's own controls and strings of NULs
                    unsafe {
                        SetDlgItemTextW(hwnd, MESSAGE, differ.as_ptr());
                        SetDlgItemTextW(hwnd, FIRST, empty.as_ptr());
                        SetDlgItemTextW(hwnd, SECOND, empty.as_ptr());
                        SendMessageW(hwnd, WM_NEXTDLGCTL, GetDlgItem(hwnd, FIRST) as usize, 1);
                    }
                } else {
                    STATE.with(|s| s.borrow_mut().as_mut().unwrap().answer = Some(value));
                    // SAFETY: ends this dialog
                    unsafe { EndDialog(hwnd, 1) };
                }
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
