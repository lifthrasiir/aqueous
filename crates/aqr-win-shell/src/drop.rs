//! Files added to an archive as they are dropped or pasted on it: on the view of a folder of it
//! (`CreateViewObject(IDropTarget)`), on a folder in it (`GetUIObjectOf(IDropTarget)`), or on
//! the archive file itself (its type's `DropHandler`, given the file by `IPersistFile`). The
//! files of the file system's (`CF_HDROP`) alone are taken, and always copied, never moved. They
//! are added by aqr-win (`add ARCHIVE DIR PATH...`: `aqr -a`, each at its name under the folder,
//! with the window of how far it is); where some of the names are in the folder already, the
//! user is asked first (those changed are replaced: the earlier stay in the archive's earlier
//! segments, and its journal points). The archive as of a journal point takes nothing.

use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, HWND, POINTL, S_FALSE};
use windows::Win32::System::Com::{IDataObject, IPersist_Impl, IPersistFile, IPersistFile_Impl, STGM};
use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, PCWSTR, PWSTR, Ref, implement};

use crate::client;

/// The class of the archive files' drop handler (registered for `.aqr` files by aqr-win).
pub const DROP_HANDLER: GUID = GUID::from_u128(0x6a3e2f84_1c5d_4b7a_9e08_d2f4c6b1a937);

/// The class of the drop handler in tests (`.aqrtest` files).
pub const TEST_DROP_HANDLER: GUID = GUID::from_u128(0x9d17c3a2_5e48_4f06_b3a1_7c2e9f0d4b58);

#[implement(IDropTarget, IPersistFile)]
pub struct Target {
    clsid: GUID,
    /// The archive file and the folder in it (names between `/`; empty: its root).
    place: Mutex<Option<(PathBuf, String)>>,
    /// The window to ask over.
    hwnd: isize,
    /// Whether what is dragged over it is taken.
    taken: Mutex<bool>,
    _counted: crate::Counted,
}

impl Target {
    /// The drop target of folder `dir` of archive `file`.
    pub fn new(file: PathBuf, dir: String, hwnd: HWND) -> Target {
        Target {
            clsid: GUID::zeroed(),
            place: Mutex::new(Some((file, dir))),
            hwnd: hwnd.0 as isize,
            taken: Mutex::new(false),
            _counted: crate::Counted::new(),
        }
    }

    /// The drop handler of the archive files (given the file by [`IPersistFile`]).
    pub fn handler(clsid: GUID) -> Target {
        Target { clsid, place: Mutex::new(None), hwnd: 0, taken: Mutex::new(false), _counted: crate::Counted::new() }
    }
}

/// Of the effects `allowed`, the one of files taken (`taken`): a copy.
fn effect(taken: bool, allowed: *mut DROPEFFECT) {
    if allowed.is_null() {
        return;
    }
    // SAFETY: the caller's pointer
    unsafe {
        *allowed = match taken && (*allowed).0 & DROPEFFECT_COPY.0 != 0 {
            true => DROPEFFECT_COPY,
            false => DROPEFFECT_NONE,
        }
    };
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        allowed: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // SAFETY: the caller's data object
        let taken = data.as_ref().is_some_and(|d| unsafe { d.QueryGetData(&crate::history::hdrop()) }.is_ok());
        *self.taken.lock().unwrap() = taken && self.place.lock().unwrap().is_some();
        effect(*self.taken.lock().unwrap(), allowed);
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, allowed: *mut DROPEFFECT) -> windows::core::Result<()> {
        effect(*self.taken.lock().unwrap(), allowed);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        *self.taken.lock().unwrap() = false;
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        allowed: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let place = self.place.lock().unwrap().clone();
            let (Some((file, dir)), Some(data)) = (place, data.as_ref()) else { return Ok(false) };
            let hwnd = match self.hwnd {
                0 => HWND(client::OWNER.load(std::sync::atomic::Ordering::Relaxed) as *mut _),
                h => HWND(h as *mut _),
            };
            add(&file, &dir, crate::history::dropped(data)?, hwnd)
        }));
        let r = r.unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()));
        if let Err(e) = &r {
            crate::log(|| format!("drop: {e}"));
        }
        effect(matches!(r, Ok(true)), allowed);
        r.map(|_| ())
    }
}

/// Files `paths` added to folder `dir` of archive `file` (by aqr-win), once the user says so
/// where some are there already (asked over `hwnd`): whether they are.
pub fn add(file: &std::path::Path, dir: &str, paths: Vec<PathBuf>, hwnd: HWND) -> windows::core::Result<bool> {
    // (not the archive itself, nor a folder holding it)
    let paths: Vec<PathBuf> = paths.into_iter().filter(|p| !file.starts_with(p)).collect();
    if paths.is_empty() {
        return Ok(false);
    }
    let key = client::Key { path: file.to_owned(), at: None };
    let there: std::collections::HashSet<String> = client::list(&key, dir, false)
        .map(|l| l.iter().filter_map(|e| e["name"].as_str().map(str::to_lowercase)).collect())
        .unwrap_or_default();
    let again = paths
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| there.contains(&n.to_string_lossy().to_lowercase())))
        .count();
    if again > 0 {
        let name = file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let ask = aqr_l10n::folder::add_again(&name, again);
        crate::log(|| format!("drop: asked: {ask}"));
        if !crate::TESTING.load(std::sync::atomic::Ordering::Relaxed) {
            let text = HSTRING::from(ask);
            // SAFETY: strings of their own
            let r = unsafe { MessageBoxW(Some(hwnd), &text, &HSTRING::from("Aqueous"), MB_YESNO | MB_ICONQUESTION) };
            if r != IDYES {
                return Ok(false);
            }
        }
    }
    let mut args: Vec<std::ffi::OsString> = vec!["add".into(), file.into(), dir.into()];
    args.extend(paths.into_iter().map(|p| p.into_os_string()));
    crate::history::run(&args, None)?;
    Ok(true)
}

impl IPersist_Impl for Target_Impl {
    fn GetClassID(&self) -> windows::core::Result<GUID> {
        Ok(self.clsid)
    }
}

impl IPersistFile_Impl for Target_Impl {
    fn IsDirty(&self) -> HRESULT {
        S_FALSE
    }

    fn Load(&self, name: &PCWSTR, _: STGM) -> windows::core::Result<()> {
        if name.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: the caller's string, ended by 0
        let file = unsafe { name.to_string() }.map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
        *self.place.lock().unwrap() = Some((PathBuf::from(file), String::new()));
        Ok(())
    }

    fn Save(&self, _: &PCWSTR, _: BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn SaveCompleted(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetCurFile(&self) -> windows::core::Result<PWSTR> {
        let place = self.place.lock().unwrap();
        let (file, _) = place.as_ref().ok_or(windows::core::Error::from(E_FAIL))?;
        // SAFETY: a string of its own, copied into COM's memory
        unsafe { windows::Win32::UI::Shell::SHStrDupW(&HSTRING::from(file.as_os_str())) }
    }
}

/// The drop target as an interface.
pub fn target(file: PathBuf, dir: String, hwnd: HWND) -> IDropTarget {
    Target::new(file, dir, hwnd).into()
}

/// The drop handler as an interface.
pub fn handler(clsid: GUID) -> IPersistFile {
    Target::handler(clsid).into()
}
