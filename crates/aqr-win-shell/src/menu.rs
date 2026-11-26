//! The context menu of entries of an archive: the shell's own (`SHCreateDefaultContextMenu`:
//! Copy, Properties, and of folders the verbs of folders), with "Open" first of a file chosen
//! alone, the default: the file written out by the server (`Materialize`) and opened as the
//! system opens a file of its type, on a thread of its own (Explorer's window not held while a
//! big file is decoded).

use std::path::PathBuf;
use std::sync::Arc;

use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, S_FALSE, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, IDataObject};
use windows::Win32::UI::Shell::{
    DFM_GETDEFSTATICID, DFM_GETVERBA, DFM_GETVERBW, DFM_INVOKECOMMAND, DFM_INVOKECOMMANDEX, DFM_MERGECONTEXTMENU,
    IContextMenuCB, IContextMenuCB_Impl, IShellFolder, QCMINFO, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    InsertMenuW, MB_ICONERROR, MB_OK, MF_BYPOSITION, MF_STRING, MessageBoxW, SW_SHOWNORMAL, SetMenuDefaultItem,
};
use windows::core::{HSTRING, PCWSTR, Ref, implement};

use crate::client;

/// The callback of the menu: of a file chosen alone, its archive and path ("Open" added).
#[implement(IContextMenuCB)]
pub struct Menu {
    pub open: Option<(Arc<client::Use>, String)>,
    pub _counted: crate::Counted,
}

/// Our command's offset: "Open".
const OPEN: usize = 0;

impl IContextMenuCB_Impl for Menu_Impl {
    fn CallBack(
        &self,
        _: Ref<IShellFolder>,
        hwnd: HWND,
        _: Ref<IDataObject>,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::core::Result<()> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.call(hwnd, msg, wparam, lparam)));
        r.unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()))
    }
}

impl Menu {
    fn call(&self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> windows::core::Result<()> {
        let msg = msg as i32;
        match msg {
            m if m == DFM_MERGECONTEXTMENU.0 => {
                if self.open.is_some() {
                    // SAFETY: the shell's QCMINFO, of its menu
                    unsafe {
                        let q = &mut *(lparam.0 as *mut QCMINFO);
                        let label = HSTRING::from(aqr_l10n::folder::open());
                        InsertMenuW(
                            q.hmenu,
                            q.indexMenu,
                            MF_BYPOSITION | MF_STRING,
                            (q.idCmdFirst as usize) + OPEN,
                            &label,
                        )?;
                        _ = SetMenuDefaultItem(q.hmenu, q.idCmdFirst + OPEN as u32, 0);
                        q.idCmdFirst += 1;
                        q.indexMenu += 1;
                    }
                }
                Ok(())
            }
            m if (m == DFM_INVOKECOMMAND.0 || m == DFM_INVOKECOMMANDEX.0) && wparam.0 == OPEN => match &self.open {
                Some((archive, path)) => {
                    open(archive.clone(), path.clone(), hwnd);
                    Ok(())
                }
                None => Err(S_FALSE.into()),
            },
            m if m == DFM_GETVERBW.0 || m == DFM_GETVERBA.0 => {
                if (wparam.0 & 0xffff) != OPEN || self.open.is_none() {
                    return Err(E_NOTIMPL.into());
                }
                let n = (wparam.0 >> 16) & 0xffff;
                // SAFETY: the shell's buffer of `n` characters
                unsafe {
                    match m == DFM_GETVERBW.0 {
                        true => {
                            let b = std::slice::from_raw_parts_mut(lparam.0 as *mut u16, n);
                            for (k, c) in "open\0".encode_utf16().enumerate().take(n) {
                                b[k] = c;
                            }
                        }
                        false => {
                            let b = std::slice::from_raw_parts_mut(lparam.0 as *mut u8, n);
                            for (k, c) in b"open\0".iter().enumerate().take(n) {
                                b[k] = *c;
                            }
                        }
                    }
                }
                Ok(())
            }
            // (the shell's commands, its own: copy, properties, ...)
            m if m == DFM_INVOKECOMMAND.0 || m == DFM_INVOKECOMMANDEX.0 => Err(S_FALSE.into()),
            m if m == DFM_GETDEFSTATICID.0 => Err(E_NOTIMPL.into()),
            _ => Err(E_NOTIMPL.into()),
        }
    }
}

/// File `path` of `archive` written out and opened, on a thread of its own; a failure told in a
/// box over `hwnd`.
fn open(archive: Arc<client::Use>, path: String, hwnd: HWND) {
    let hwnd = hwnd.0 as isize;
    let counted = crate::Counted::new();
    std::thread::spawn(move || {
        let _counted = counted;
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: this thread's COM
        if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
            return;
        }
        let r = client::materialize(archive.path(), &path).and_then(|file| execute(&file, hwnd));
        if let Err(e) = r {
            crate::log(|| format!("open {path}: {e}"));
            let name = path.rsplit('/').next().unwrap_or(&path);
            let text = HSTRING::from(format!("{}\n\n{}", aqr_l10n::folder::not_opened(name), e.message()));
            // SAFETY: strings of their own
            unsafe { MessageBoxW(Some(hwnd), &text, &HSTRING::from("Aqueous"), MB_OK | MB_ICONERROR) };
        }
        drop(archive);
        // SAFETY: initialized above, its objects let go of
        unsafe { CoUninitialize() };
    });
}

/// `file` opened as the system opens a file of its type.
fn execute(file: &std::path::Path, hwnd: HWND) -> windows::core::Result<()> {
    let f = HSTRING::from(file.as_os_str());
    let dir = HSTRING::from(file.parent().map(PathBuf::from).unwrap_or_default().as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        hwnd,
        lpFile: PCWSTR(f.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: a structure of strings of its own
    unsafe { ShellExecuteExW(&mut info) }
}
