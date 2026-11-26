//! The property sheet of entries of an archive (the menu's Properties, Alt+Enter): a page for
//! each entry chosen, of all that `aqr -t -v` tells of it, its formula too, a line each under
//! the columns' names ([`crate::columns`]) in the user's language. On a thread of its own, the
//! sheet a window of its own (Explorer's not held while it is open).

use std::sync::Arc;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Controls::{
    PROPSHEETHEADERW_V2, PROPSHEETHEADERW_V2_0, PROPSHEETHEADERW_V2_1, PROPSHEETHEADERW_V2_2, PROPSHEETHEADERW_V2_3,
    PROPSHEETPAGEW, PROPSHEETPAGEW_0, PSH_NOAPPLYNOW, PSH_PROPSHEETPAGE, PSH_PROPTITLE, PSP_DLGINDIRECT, PSP_USETITLE,
    PropertySheetW,
};
use windows::Win32::UI::WindowsAndMessaging::{DLGTEMPLATE, SendDlgItemMessageW, SetDlgItemTextW, WM_INITDIALOG};
use windows::core::{BOOL, HSTRING, PCWSTR};

use crate::client;
use crate::columns::{self, Cell, What};
use crate::pidl::Item;

/// The lines of the page of `item` (of `dir` of archive `path`), as the server tells it with
/// what `-t -v` tells too.
fn lines(path: &std::path::Path, dir: &str, item: &Item) -> Vec<(String, String)> {
    let at = crate::folder::joined(dir, &item.name);
    let entry = client::stat(path, &at, !item.is_dir()).ok();
    let mut out = Vec::new();
    let place = format!("{}\\{}", path.display(), at.replace('/', "\\"));
    out.push((aqr_l10n::folder::prop_location().to_owned(), place));
    for c in &columns::COLUMNS {
        let cell = columns::cell(item, entry.as_ref(), c.what);
        let shown = match &cell {
            Cell::None => continue,
            // (the bytes too, as -t -v tells them)
            Cell::Bytes(n) => format!("{} ({})", cell.shown(), aqr_l10n::folder::prop_bytes(n)),
            _ => cell.shown(),
        };
        out.push(((c.title)().to_owned(), shown));
        if c.what == What::Segment
            && let Some(e) = &entry
        {
            if let Some(f) = e["formula"].as_str() {
                out.push((aqr_l10n::folder::prop_formula().to_owned(), f.to_owned()));
            }
            if let Some(n) = e["nodes"].as_u64() {
                out.push((aqr_l10n::folder::prop_nodes().to_owned(), n.to_string()));
            }
        }
    }
    out
}

/// The text of a page: its lines, the label before each value.
fn text(lines: &[(String, String)]) -> String {
    lines.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\r\n")
}

/// The sheet of `items` of `dir` of `archive`, shown on a thread of its own (but where the
/// library is of the tests: the pages' text logged).
pub fn show(archive: Arc<client::Use>, dir: String, items: Vec<Item>) {
    let counted = crate::Counted::new();
    std::thread::spawn(move || {
        let _counted = counted;
        // SAFETY: this thread's COM, a single-threaded apartment for the sheet's window
        if unsafe { OleInitialize(None) }.is_err() {
            return;
        }
        let pages: Vec<(String, Vec<u16>)> = items
            .iter()
            .map(|i| (i.name.clone(), text(&lines(archive.path(), &dir, i)).encode_utf16().chain([0]).collect()))
            .collect();
        if crate::TESTING.load(std::sync::atomic::Ordering::Relaxed) {
            for (name, t) in &pages {
                crate::log(|| format!("properties of {name}:\n{}", String::from_utf16_lossy(&t[..t.len() - 1])));
            }
        } else {
            sheet(&pages);
        }
        drop(archive);
        // SAFETY: initialized above
        unsafe { OleUninitialize() };
    });
}

/// The id of a page's text.
const TEXT: u16 = 100;

/// The sheet of `pages` (each its title and text, ended by 0), modal on this thread.
fn sheet(pages: &[(String, Vec<u16>)]) {
    let template = page_template();
    let titles: Vec<HSTRING> = pages.iter().map(|(n, _)| HSTRING::from(n.as_str())).collect();
    let mut psp: Vec<PROPSHEETPAGEW> = pages
        .iter()
        .zip(&titles)
        .map(|((_, t), title)| PROPSHEETPAGEW {
            dwSize: std::mem::size_of::<PROPSHEETPAGEW>() as u32,
            dwFlags: PSP_DLGINDIRECT | PSP_USETITLE,
            Anonymous1: PROPSHEETPAGEW_0 { pResource: template.as_ptr() as *mut DLGTEMPLATE },
            pszTitle: PCWSTR(title.as_ptr()),
            pfnDlgProc: Some(page_proc),
            lParam: LPARAM(t.as_ptr() as isize),
            ..Default::default()
        })
        .collect();
    let caption = HSTRING::from(pages.first().map_or("", |p| p.0.as_str()));
    let mut h = PROPSHEETHEADERW_V2 {
        dwSize: std::mem::size_of::<PROPSHEETHEADERW_V2>() as u32,
        dwFlags: PSH_PROPSHEETPAGE | PSH_PROPTITLE | PSH_NOAPPLYNOW,
        Anonymous1: PROPSHEETHEADERW_V2_0::default(),
        pszCaption: PCWSTR(caption.as_ptr()),
        nPages: psp.len() as u32,
        Anonymous2: PROPSHEETHEADERW_V2_1::default(),
        Anonymous3: PROPSHEETHEADERW_V2_2 { ppsp: psp.as_mut_ptr() },
        Anonymous4: PROPSHEETHEADERW_V2_3::default(),
        ..Default::default()
    };
    // SAFETY: a header of pages, templates and strings alive for the call
    unsafe { PropertySheetW(&mut h) };
}

/// A page's procedure: its text set as it is made (the page's `lParam`, of the text's).
unsafe extern "system" fn page_proc(hwnd: HWND, msg: u32, _: WPARAM, lparam: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        // SAFETY: of WM_INITDIALOG, the page's PROPSHEETPAGEW, its lParam a text ended by 0
        unsafe {
            let page = &*(lparam.0 as *const PROPSHEETPAGEW);
            _ = SetDlgItemTextW(hwnd, TEXT as i32, PCWSTR(page.lParam.0 as *const u16));
            // (none of it selected: EM_SETSEL)
            SendDlgItemMessageW(hwnd, TEXT as i32, 0x00B1, WPARAM(0), LPARAM(0));
        }
        return BOOL(1).0 as isize;
    }
    0
}

/// The template of a page: a field of read-only text, of lines, over all of it.
fn page_template() -> Vec<u32> {
    const W: i16 = 227;
    const H: i16 = 215;
    let mut t: Vec<u16> = Vec::new();
    let dword = |t: &mut Vec<u16>, v: u32| t.extend([v as u16, (v >> 16) as u16]);
    let string = |t: &mut Vec<u16>, s: &str| t.extend(s.encode_utf16().chain([0]));
    // WS_CHILD | WS_DISABLED | WS_CAPTION | DS_SETFONT
    dword(&mut t, 0x4000_0000 | 0x0800_0000 | 0x00C0_0000 | 0x40);
    dword(&mut t, 0);
    t.push(1);
    t.extend([0, 0, W as u16, H as u16]);
    t.extend([0, 0]);
    string(&mut t, "");
    t.push(9);
    string(&mut t, "Segoe UI");
    if t.len() % 2 == 1 {
        t.push(0);
    }
    // WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_TABSTOP | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY
    dword(&mut t, 0x4000_0000 | 0x1000_0000 | 0x0020_0000 | 0x0001_0000 | 0x4 | 0x40 | 0x800);
    dword(&mut t, 0x200); // WS_EX_CLIENTEDGE
    t.extend([7, 7, (W - 14) as u16, (H - 14) as u16]);
    t.push(TEXT);
    t.extend([0xFFFF, 0x0081]); // EDIT
    string(&mut t, "");
    t.push(0);
    if t.len() % 2 == 1 {
        t.push(0);
    }
    t.chunks(2).map(|c| c[0] as u32 | (c[1] as u32) << 16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateDialogIndirectParamW, DestroyWindow, GetDesktopWindow, GetDlgItemTextW,
    };

    /// The page made of its template (hidden, a child of the desktop's), its text set by its
    /// procedure as the sheet has it.
    #[test]
    fn page() {
        let template = page_template();
        let text: Vec<u16> = "Name: a.txt\r\nSize: 1 KB".encode_utf16().chain([0]).collect();
        let page = PROPSHEETPAGEW { lParam: LPARAM(text.as_ptr() as isize), ..Default::default() };
        // SAFETY: a template and a page of their own, alive for the calls
        unsafe {
            let h = CreateDialogIndirectParamW(
                None,
                template.as_ptr() as *const DLGTEMPLATE,
                Some(GetDesktopWindow()),
                Some(page_proc),
                LPARAM(&page as *const _ as isize),
            )
            .unwrap();
            let mut got = [0u16; 64];
            let n = GetDlgItemTextW(h, TEXT as i32, &mut got) as usize;
            assert_eq!(String::from_utf16_lossy(&got[..n]), "Name: a.txt\r\nSize: 1 KB");
            DestroyWindow(h).unwrap();
        }
    }
}
