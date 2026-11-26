//! The menu of the background of an archive's view: "Versions", its journal points (the named
//! versions, `--tag` and `-j`), newest first, after "Latest" (all of it), the one shown checked.
//! A point chosen is browsed to in the same window: the archive file's ID list with the point's
//! item after it ([`crate::pidl::Kind::Version`]). Of an archive of no point, nothing is added.

use std::sync::Mutex;

use serde_json::Value;
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::IServiceProvider;
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMF_DEFAULTONLY, CMINVOKECOMMANDINFO, IContextMenu, IContextMenu_Impl, ILCombine, ILFree, IShellBrowser,
    SBSP_ABSOLUTE, SBSP_SAMEBROWSER, SEE_MASK_CLASSNAME, SEE_MASK_IDLIST, SHELLEXECUTEINFOW, SHGetNameFromIDList,
    SID_STopLevelBrowser, SIGDN_DESKTOPABSOLUTEPARSING, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, InsertMenuW, MF_BYPOSITION, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    SW_SHOWNORMAL, SendMessageW, WM_USER,
};
use windows::core::{HRESULT, HSTRING, IUnknown, Interface, PCWSTR, PSTR, Ref, implement, w};

use crate::client;
use crate::folder::Pidl;
use crate::pidl::Item;

#[implement(IContextMenu, IObjectWithSite)]
pub struct Versions {
    key: client::Key,
    /// The archive file's ID list (absolute).
    file: Pidl,
    /// The window of the view's (given as it was made).
    hwnd: isize,
    site: Mutex<Option<IUnknown>>,
    /// The points, as the menu was made of them, in their order.
    points: Mutex<Vec<Value>>,
    _counted: crate::Counted,
}

impl Versions {
    pub fn new(key: client::Key, file: Pidl, hwnd: HWND) -> Versions {
        Versions {
            key,
            file,
            hwnd: hwnd.0 as isize,
            site: Mutex::new(None),
            points: Mutex::new(Vec::new()),
            _counted: crate::Counted::new(),
        }
    }

    /// The menu `hmenu` given ours at `index`, of ids from `first` up to `last`: how many ids.
    fn query(
        &self,
        hmenu: windows::Win32::UI::WindowsAndMessaging::HMENU,
        index: u32,
        first: u32,
        last: u32,
    ) -> windows::core::Result<u32> {
        // (of all of the archive: as of a point, it knows of none after)
        let points = client::journal(&client::Key { path: self.key.path.clone(), at: None })?;
        if points.is_empty() || (last - first) < points.len() as u32 {
            return Ok(0);
        }
        // SAFETY: a menu of its own, made the shell's menu's submenu
        unsafe {
            let sub = CreatePopupMenu()?;
            let checked = |c: bool| if c { MF_CHECKED } else { Default::default() };
            AppendMenuW(
                sub,
                MF_STRING | checked(self.key.at.is_none()),
                first as usize,
                &HSTRING::from(aqr_l10n::folder::latest()),
            )?;
            AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null())?;
            for (k, p) in points.iter().enumerate().rev() {
                let this = self.key.at.is_some() && p["metadata"].as_u64() == self.key.at;
                let shown = HSTRING::from(crate::folder::version_shown(p));
                AppendMenuW(sub, MF_STRING | checked(this), (first + 1 + k as u32) as usize, &shown)?;
            }
            let label = HSTRING::from(aqr_l10n::folder::versions());
            InsertMenuW(hmenu, index, MF_BYPOSITION | MF_POPUP | MF_STRING, sub.0 as usize, &label)?;
        }
        let n = 1 + points.len() as u32;
        *self.points.lock().unwrap() = points;
        Ok(n)
    }
}

/// The ID list of archive `file` (its file's) as of point `p` (as `aqr --serve` tells it; none: all
/// of it), the caller's.
pub fn target(file: *const ITEMIDLIST, p: Option<&Value>) -> *mut ITEMIDLIST {
    let Some(p) = p else {
        // SAFETY: an ID list of the caller's, copied
        return unsafe { windows::Win32::UI::Shell::ILClone(file) };
    };
    let v = crate::pidl::of_items(&[Item::version(p, crate::folder::version_shown(p))]);
    // SAFETY: two ID lists, combined into one of COM's memory; the second freed
    unsafe {
        let t = ILCombine(Some(file), Some(v));
        ILFree(Some(v));
        t
    }
}

/// Browsed to `pidl` in the window of a view: by the browser of `site`, or of window `hwnd`, else
/// in a window of its own.
pub fn browse(pidl: *const ITEMIDLIST, site: Option<IUnknown>, hwnd: HWND) -> windows::core::Result<()> {
    // (for tests: the place told, not browsed to)
    if told(|| Ok(format!("browse {}", parsing_name(pidl)?)))? {
        return Ok(());
    }
    let by_site = site
        .and_then(|s| s.cast::<IServiceProvider>().ok())
        // SAFETY: a service of the site's
        .and_then(|sp| unsafe { sp.QueryService::<IShellBrowser>(&SID_STopLevelBrowser) }.ok());
    // (`CWM_GETISHELLBROWSER`: the browser of the window, not referenced for the caller)
    let by_window = || {
        if hwnd.0.is_null() {
            return None;
        }
        // SAFETY: a message of the shell's windows
        let p = unsafe { SendMessageW(hwnd, WM_USER + 7, Some(WPARAM(0)), Some(LPARAM(0))) };
        let raw = p.0 as *mut core::ffi::c_void;
        // SAFETY: the window's browser, where it is one
        (!raw.is_null()).then(|| unsafe { IShellBrowser::from_raw_borrowed(&raw) }.cloned()).flatten()
    };
    if let Some(b) = by_site.or_else(by_window) {
        // SAFETY: an ID list of the caller's
        return unsafe { b.BrowseObject(pidl, SBSP_SAMEBROWSER | SBSP_ABSOLUTE) };
    }
    window(pidl, hwnd)
}

/// For tests (`AQR_SHELL_TEST_OPENED`, the file told): the line of `what` told, not done; whether
/// it was.
pub fn told(what: impl FnOnce() -> windows::core::Result<String>) -> windows::core::Result<bool> {
    use std::io::Write;
    let Some(file) = std::env::var_os("AQR_SHELL_TEST_OPENED") else { return Ok(false) };
    let line = what()?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
    _ = writeln!(f, "{line}");
    Ok(true)
}

/// The name ID list `pidl` is parsed by, from the desktop.
pub fn parsing_name(pidl: *const ITEMIDLIST) -> windows::core::Result<String> {
    // SAFETY: an ID list of the caller's
    let name = unsafe { SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING) }?;
    // SAFETY: a string of COM's memory, freed after
    unsafe {
        let t = name.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
        Ok(t)
    }
}

/// Folder `pidl` opened in a window of its own.
pub fn window(pidl: *const ITEMIDLIST, hwnd: HWND) -> windows::core::Result<()> {
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_IDLIST | SEE_MASK_CLASSNAME,
        hwnd,
        lpVerb: w!("open"),
        lpClass: w!("folder"),
        lpIDList: pidl as *mut _,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: a structure of its own, of strings of their own
    unsafe { ShellExecuteExW(&mut info) }
}

impl IContextMenu_Impl for Versions_Impl {
    fn QueryContextMenu(
        &self,
        hmenu: windows::Win32::UI::WindowsAndMessaging::HMENU,
        index: u32,
        first: u32,
        last: u32,
        flags: u32,
    ) -> HRESULT {
        if flags & CMF_DEFAULTONLY != 0 {
            return HRESULT(0);
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.query(hmenu, index, first, last)));
        match r {
            Ok(Ok(n)) => HRESULT(n as i32),
            Ok(Err(e)) => {
                crate::log(|| format!("versions: {e}"));
                HRESULT(0)
            }
            Err(_) => HRESULT(0),
        }
    }

    fn InvokeCommand(&self, pici: *const CMINVOKECOMMANDINFO) -> windows::core::Result<()> {
        if pici.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: the caller's structure
        let (verb, hwnd) = unsafe { ((*pici).lpVerb.0 as usize, (*pici).hwnd) };
        // (by offset alone: of no verb's name)
        if verb >> 16 != 0 {
            return Err(E_INVALIDARG.into());
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let p = match verb {
                0 => None,
                k => Some(
                    self.points.lock().unwrap().get(k - 1).cloned().ok_or(windows::core::Error::from(E_INVALIDARG))?,
                ),
            };
            let t = target(self.file.as_ptr(), p.as_ref());
            let hwnd = if hwnd.0.is_null() { HWND(self.hwnd as *mut _) } else { hwnd };
            let r = browse(t, self.site.lock().unwrap().clone(), hwnd);
            // SAFETY: of its own
            unsafe { ILFree(Some(t)) };
            r
        }));
        let r = r.unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()));
        if let Err(e) = &r {
            crate::log(|| format!("versions: {e}"));
        }
        r
    }

    fn GetCommandString(&self, _: usize, _: u32, _: *const u32, _: PSTR, _: u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IObjectWithSite_Impl for Versions_Impl {
    fn SetSite(&self, site: Ref<IUnknown>) -> windows::core::Result<()> {
        *self.site.lock().unwrap() = site.cloned();
        Ok(())
    }

    fn GetSite(&self, riid: *const windows::core::GUID, ppv: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        match self.site.lock().unwrap().as_ref() {
            // SAFETY: the caller's pointers
            Some(s) => unsafe { s.query(riid, ppv).ok() },
            None => Err(E_NOINTERFACE.into()),
        }
    }
}
