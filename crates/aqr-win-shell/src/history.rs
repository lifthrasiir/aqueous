//! The "Versions" tab of an archive file's Properties (a property sheet handler of `.aqr` files):
//! its journal points (the named versions, `-j`, `--tag`), newest first, read once the tab is
//! shown (an encrypted archive asks for its passphrase then, not as Properties opens). "Open"
//! (or a double-click) opens the archive as of the point chosen in a window of its own, as the
//! view's menu goes to it ([`crate::versions`]); "Extract…" and "Name this version…" are
//! aqr-win's (`extract-version ARCHIVE #N`, `name-version ARCHIVE`), beside this library, the
//! list read again once a version is named.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::Value;
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{CF_HDROP, ReleaseStgMedium};
use windows::Win32::System::Registry::HKEY;
use windows::Win32::UI::Controls::{
    CreatePropertySheetPageW, LPFNSVADDPROPSHEETPAGE, NMHDR, PROPSHEETPAGEW, PROPSHEETPAGEW_0, PSN_SETACTIVE,
    PSP_DLGINDIRECT, PSP_USECALLBACK, PSP_USETITLE, PSPCB_MESSAGE, PSPCB_RELEASE,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DragQueryFileW, HDROP, ILCreateFromPathW, ILFree, IShellExtInit, IShellExtInit_Impl, IShellPropSheetExt,
    IShellPropSheetExt_Impl,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DLGTEMPLATE, GWLP_USERDATA, GetDlgItem, GetWindowLongPtrW, LB_ADDSTRING, LB_ERR, LB_GETCURSEL, LB_RESETCONTENT,
    LB_SETCURSEL, PostMessageW, SendDlgItemMessageW, SetWindowLongPtrW, WM_APP, WM_COMMAND, WM_INITDIALOG, WM_NOTIFY,
};
use windows::core::{BOOL, HSTRING, PCWSTR, Ref, implement};

use crate::client;

/// The class of the tab (registered for `.aqr` files by aqr-win).
pub const VERSIONS_PAGE: windows::core::GUID = windows::core::GUID::from_u128(0x54394c15_6f9d_4015_9d6c_5805da234236);

/// The class of the tab in tests (`.aqrtest` files), served by the test's server.
pub const TEST_VERSIONS_PAGE: windows::core::GUID =
    windows::core::GUID::from_u128(0x074fb592_0f85_4444_b827_25a209156582);

#[implement(IShellExtInit, IShellPropSheetExt)]
pub struct Page {
    /// The archive file, where one alone was chosen.
    file: Mutex<Option<PathBuf>>,
    _counted: crate::Counted,
}

impl Default for Page {
    fn default() -> Page {
        Page { file: Mutex::new(None), _counted: crate::Counted::new() }
    }
}

impl IShellExtInit_Impl for Page_Impl {
    fn Initialize(&self, _: *const ITEMIDLIST, data: Ref<IDataObject>, _: HKEY) -> windows::core::Result<()> {
        let data = data.ok()?;
        let files = dropped(data)?;
        *self.file.lock().unwrap() = match &files[..] {
            [f] => Some(f.clone()),
            _ => None,
        };
        Ok(())
    }
}

/// The format of files dropped (`CF_HDROP`).
pub(crate) fn hdrop() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// The files of data object `data` (`CF_HDROP`: of the file system's, dropped or copied).
pub(crate) fn dropped(data: &IDataObject) -> windows::core::Result<Vec<PathBuf>> {
    // SAFETY: the caller's data object, its medium let go of after
    unsafe {
        let mut m = data.GetData(&hdrop())?;
        let drop = HDROP(m.u.hGlobal.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        let files = (0..n)
            .map(|k| {
                let mut b = vec![0u16; DragQueryFileW(drop, k, None) as usize + 1];
                let got = DragQueryFileW(drop, k, Some(&mut b)) as usize;
                PathBuf::from(String::from_utf16_lossy(&b[..got]))
            })
            .collect();
        ReleaseStgMedium(&mut m);
        Ok(files)
    }
}

/// What a page holds: its archive, and its points as they were read (none yet: not read).
struct State {
    file: PathBuf,
    points: Option<Vec<Value>>,
    _counted: crate::Counted,
}

/// The template of a page, and its title (alive for as long as the library, as its pages).
static TEMPLATE: LazyLock<Vec<u32>> = LazyLock::new(template);
static TITLE: LazyLock<HSTRING> = LazyLock::new(|| HSTRING::from(aqr_l10n::folder::versions()));

impl IShellPropSheetExt_Impl for Page_Impl {
    fn AddPages(&self, add: LPFNSVADDPROPSHEETPAGE, lparam: LPARAM) -> windows::core::Result<()> {
        let Some(file) = self.file.lock().unwrap().clone() else { return Ok(()) };
        let add = add.ok_or(windows::core::Error::from(E_INVALIDARG))?;
        let state = Box::into_raw(Box::new(State { file, points: None, _counted: crate::Counted::new() }));
        let mut psp = PROPSHEETPAGEW {
            dwSize: std::mem::size_of::<PROPSHEETPAGEW>() as u32,
            dwFlags: PSP_DLGINDIRECT | PSP_USETITLE | PSP_USECALLBACK,
            Anonymous1: PROPSHEETPAGEW_0 { pResource: TEMPLATE.as_ptr() as *mut DLGTEMPLATE },
            pszTitle: PCWSTR(TITLE.as_ptr()),
            pfnDlgProc: Some(page_proc),
            lParam: LPARAM(state as isize),
            pfnCallback: Some(page_callback),
            ..Default::default()
        };
        // SAFETY: a page of a template and a title of the library's, its state freed by its
        // callback once it is released (or here, where it is not taken)
        unsafe {
            let page = CreatePropertySheetPageW(&mut psp);
            if page.is_invalid() {
                drop(Box::from_raw(state));
                return Err(E_FAIL.into());
            }
            if !add(page, lparam).as_bool() {
                _ = windows::Win32::UI::Controls::DestroyPropertySheetPage(page);
            }
        }
        Ok(())
    }

    fn ReplacePage(&self, _: u32, _: LPFNSVADDPROPSHEETPAGE, _: LPARAM) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
}

/// The page released: its state freed.
unsafe extern "system" fn page_callback(_: HWND, msg: PSPCB_MESSAGE, psp: *mut PROPSHEETPAGEW) -> u32 {
    if msg == PSPCB_RELEASE && !psp.is_null() {
        // SAFETY: the page's state, of `AddPages`, freed once
        unsafe {
            let state = (*psp).lParam.0 as *mut State;
            if !state.is_null() {
                drop(Box::from_raw(state));
                (*psp).lParam = LPARAM(0);
            }
        }
    }
    1
}

// (the ids of the controls)
const LIST: i32 = 100;
const OPEN: i32 = 101;
const EXTRACT: i32 = 102;
const NAME: i32 = 103;
/// Posted once a version is named: the list read again.
const NAMED: u32 = WM_APP + 1;
/// `LBN_DBLCLK`.
const DOUBLE_CLICK: u32 = 2;

/// The state of page `hwnd`.
///
/// # Safety
///
/// `hwnd` is a page of these, alive (its state set as it was made, freed only once it is gone).
unsafe fn state<'a>(hwnd: HWND) -> Option<&'a mut State> {
    // SAFETY: as the caller's
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State).as_mut() }
}

unsafe extern "system" fn page_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    let r = std::panic::catch_unwind(|| {
        // SAFETY: the messages of a page of these
        unsafe {
            match msg {
                WM_INITDIALOG => {
                    let psp = &*(lparam.0 as *const PROPSHEETPAGEW);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, psp.lParam.0);
                    1
                }
                WM_NOTIFY if (*(lparam.0 as *const NMHDR)).code == PSN_SETACTIVE => {
                    if let Some(s) = state(hwnd).filter(|s| s.points.is_none()) {
                        fill(hwnd, s);
                    }
                    0
                }
                NAMED => {
                    if let Some(s) = state(hwnd) {
                        fill(hwnd, s);
                    }
                    0
                }
                WM_COMMAND => {
                    let (id, code) = ((wparam.0 & 0xffff) as i32, (wparam.0 >> 16) as u32 & 0xffff);
                    let Some(s) = state(hwnd) else { return 0 };
                    let chosen = || {
                        let k = SendDlgItemMessageW(hwnd, LIST, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                        let points = s.points.as_deref().unwrap_or_default();
                        // (newest first)
                        (k != LB_ERR as isize && (k as usize) < points.len())
                            .then(|| (points.len() - 1 - k as usize, points[points.len() - 1 - k as usize].clone()))
                    };
                    let r = match id {
                        OPEN | LIST if id == OPEN || code == DOUBLE_CLICK => match chosen() {
                            Some((_, p)) => open(&s.file, &p, hwnd),
                            None => Ok(()),
                        },
                        EXTRACT => match chosen() {
                            Some((k, _)) => {
                                run(&["extract-version", &s.file.to_string_lossy(), &format!("#{}", k + 1)], None)
                            }
                            None => Ok(()),
                        },
                        NAME => run(&["name-version", &s.file.to_string_lossy()], Some(hwnd)),
                        _ => Ok(()),
                    };
                    if let Err(e) = r {
                        crate::log(|| format!("versions page: {e}"));
                    }
                    0
                }
                _ => 0,
            }
        }
    });
    r.unwrap_or(0)
}

/// The list of page `hwnd` filled with the archive's points, read now; the buttons of a point
/// chosen enabled where there is one.
fn fill(hwnd: HWND, s: &mut State) {
    let key = client::Key { path: s.file.clone(), at: None };
    // (an archive changed: its points read anew)
    client::changed(&key);
    let points = match client::journal(&key) {
        Ok(p) => p,
        Err(e) => {
            crate::log(|| format!("versions page: {e}"));
            Vec::new()
        }
    };
    // SAFETY: the controls of the page
    unsafe {
        SendDlgItemMessageW(hwnd, LIST, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for (k, p) in points.iter().enumerate().rev() {
            let row =
                format!("#{}  {}  {}", k + 1, p["shown"].as_str().unwrap_or(""), p["label"].as_str().unwrap_or(""));
            let w: Vec<u16> = row.encode_utf16().chain([0]).collect();
            SendDlgItemMessageW(hwnd, LIST, LB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
        }
        if points.is_empty() {
            let w: Vec<u16> = aqr_l10n::folder::versions_none().encode_utf16().chain([0]).collect();
            SendDlgItemMessageW(hwnd, LIST, LB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
        } else {
            SendDlgItemMessageW(hwnd, LIST, LB_SETCURSEL, WPARAM(0), LPARAM(0));
        }
        windows::core::link!("user32.dll" "system" fn EnableWindow(hwnd: HWND, enable: BOOL) -> BOOL);
        let some = BOOL(!points.is_empty() as i32);
        for id in [LIST, OPEN, EXTRACT] {
            if let Ok(c) = GetDlgItem(Some(hwnd), id) {
                _ = EnableWindow(c, some);
            }
        }
    }
    s.points = Some(points);
}

/// Archive `file` opened as of point `p` (as `aqr --serve` tells it), in a window of its own.
fn open(file: &Path, p: &Value, hwnd: HWND) -> windows::core::Result<()> {
    // SAFETY: ID lists of their own, freed after
    unsafe {
        let f = ILCreateFromPathW(&HSTRING::from(file.as_os_str()));
        if f.is_null() {
            return Err(E_FAIL.into());
        }
        let t = crate::versions::target(f, Some(p));
        ILFree(Some(f));
        let r = match crate::versions::told(|| Ok(format!("open {}", crate::versions::parsing_name(t)?))) {
            Ok(true) => Ok(()),
            Ok(false) => crate::versions::window(t, hwnd),
            Err(e) => Err(e),
        };
        ILFree(Some(t));
        r
    }
}

/// aqr-win (beside this library) run with `args`; where `then` is a page's, told once it ends.
pub(crate) fn run<S: AsRef<std::ffi::OsStr>>(args: &[S], then: Option<HWND>) -> windows::core::Result<()> {
    let shown = || args.iter().map(|a| a.as_ref().to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
    if crate::versions::told(|| Ok(format!("run {}", shown())))? {
        if let Some(h) = then {
            // SAFETY: a message of the page's
            unsafe { PostMessageW(Some(h), NAMED, WPARAM(0), LPARAM(0)) }?;
        }
        return Ok(());
    }
    let exe = crate::library_dir().ok_or(windows::core::Error::from(E_FAIL))?.join("aqr-win.exe");
    let mut child = std::process::Command::new(&exe)
        .args(args)
        .spawn()
        .map_err(|e| windows::core::Error::new(E_FAIL, format!("{}: {e}", exe.display())))?;
    if let Some(h) = then {
        let h = h.0 as isize;
        let counted = crate::Counted::new();
        std::thread::spawn(move || {
            let _counted = counted;
            _ = child.wait();
            // SAFETY: a message of the page's (none where it is gone)
            _ = unsafe { PostMessageW(Some(HWND(h as *mut _)), NAMED, WPARAM(0), LPARAM(0)) };
        });
    }
    Ok(())
}

/// The template of a page: the list over it, the buttons below.
fn template() -> Vec<u32> {
    const W: i16 = 227;
    const H: i16 = 215;
    let mut t: Vec<u16> = Vec::new();
    let dword = |t: &mut Vec<u16>, v: u32| t.extend([v as u16, (v >> 16) as u16]);
    let string = |t: &mut Vec<u16>, s: &str| t.extend(s.encode_utf16().chain([0]));
    let align = |t: &mut Vec<u16>| {
        if t.len() % 2 == 1 {
            t.push(0);
        }
    };
    // WS_CHILD | WS_DISABLED | WS_CAPTION | DS_SETFONT
    dword(&mut t, 0x4000_0000 | 0x0800_0000 | 0x00C0_0000 | 0x40);
    dword(&mut t, 0);
    t.push(4);
    t.extend([0, 0, W as u16, H as u16]);
    t.extend([0, 0]);
    string(&mut t, "");
    t.push(9);
    string(&mut t, "Segoe UI");
    let (open, extract, name) =
        (aqr_l10n::folder::open(), aqr_l10n::folder::versions_extract(), aqr_l10n::folder::versions_name());
    let by = H - 7 - 14;
    // (style, extended style, place, id, class, text)
    type Control<'a> = (u32, u32, [i16; 4], i32, u16, &'a str);
    let items: [Control; 4] = [
        // WS_VSCROLL | WS_TABSTOP | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT, WS_EX_CLIENTEDGE
        (0x0020_0000 | 0x0001_0000 | 0x1 | 0x100, 0x200, [7, 7, W - 14, by - 7 - 7], LIST, 0x0083, ""),
        // WS_TABSTOP | BS_DEFPUSHBUTTON, BS_PUSHBUTTON
        (0x0001_0000 | 0x1, 0, [7, by, 50, 14], OPEN, 0x0080, open),
        (0x0001_0000, 0, [61, by, 60, 14], EXTRACT, 0x0080, extract),
        (0x0001_0000, 0, [W - 7 - 95, by, 95, 14], NAME, 0x0080, name),
    ];
    for (style, ex, [x, y, w, h], id, class, text) in items {
        align(&mut t);
        // WS_CHILD | WS_VISIBLE
        dword(&mut t, 0x4000_0000 | 0x1000_0000 | style);
        dword(&mut t, ex);
        t.extend([x as u16, y as u16, w as u16, h as u16]);
        t.push(id as u16);
        t.extend([0xFFFF, class]);
        string(&mut t, text);
        t.push(0);
    }
    align(&mut t);
    t.chunks(2).map(|c| c[0] as u32 | (c[1] as u32) << 16).collect()
}
