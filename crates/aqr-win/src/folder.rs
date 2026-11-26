//! The folder that "Extract to…" extracts into, picked in the shell's `IFileOpenDialog` of
//! folders (Vista on): opened at the archives' folder, titled after them.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::UI::Shell::SHCreateItemFromParsingName;
use windows_sys::core::GUID;

use crate::dialog::wide;

type P = *mut c_void;

const CLSID_FILE_OPEN_DIALOG: GUID = GUID::from_u128(0xdc1c5a9c_e88a_4dde_a5a1_60f82a20aef7);
const IID_FILE_OPEN_DIALOG: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);
const IID_SHELL_ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);

/// `FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM`.
const OPTIONS: u32 = 0x20 | 0x40;

/// `SIGDN_FILESYSPATH`.
const FILE_SYSTEM_PATH: u32 = 0x8005_8000;

/// A folder picked by the user, the dialog opened at `start` (where it is a folder) with
/// `title` and an OK button of `button`; `None` where none was (cancelled, or the dialog not
/// made).
pub fn pick(start: Option<&Path>, title: &str, button: &str) -> Option<PathBuf> {
    // SAFETY: COM of this thread (initialised here, apartment-threaded, as the shell's dialogs
    // want), each interface made released once
    unsafe {
        let init = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut held = Vec::new();
        let r = run(start, title, button, &mut held);
        for o in held.into_iter().rev() {
            slot::<unsafe extern "system" fn(P) -> u32>(o, 2)(o);
        }
        if init >= 0 {
            CoUninitialize();
        }
        r
    }
}

/// Method `i` of the vtable of COM object `o`, as `F`.
///
/// # Safety
/// `o` is a live COM object whose vtable has, at `i`, a function of type `F`.
unsafe fn slot<F: Copy>(o: P, i: usize) -> F {
    // SAFETY: as the caller ensures
    unsafe { std::mem::transmute_copy(&*(*(o as *const *const P)).add(i)) }
}

/// [`pick`]'s work, the interfaces it makes noted in `held`.
unsafe fn run(start: Option<&Path>, title: &str, button: &str, held: &mut Vec<P>) -> Option<PathBuf> {
    let ok = |hr: i32| (hr >= 0).then_some(());
    let null = std::ptr::null_mut();
    // SAFETY: the interfaces' methods at the slots of their documented vtables (ShObjIdl_core.h:
    // IModalWindow's Show 3; IFileDialog's SetOptions 9, GetOptions 10, SetFolder 12, SetTitle 17,
    // SetOkButtonLabel 18, GetResult 20; IShellItem's GetDisplayName 5)
    unsafe {
        let mut d = null;
        ok(CoCreateInstance(&CLSID_FILE_OPEN_DIALOG, null, CLSCTX_INPROC_SERVER, &IID_FILE_OPEN_DIALOG, &mut d))?;
        held.push(d);
        let mut o = 0u32;
        ok(slot::<unsafe extern "system" fn(P, *mut u32) -> i32>(d, 10)(d, &mut o))?;
        ok(slot::<unsafe extern "system" fn(P, u32) -> i32>(d, 9)(d, o | OPTIONS))?;
        if let Some(at) = start.filter(|s| s.is_dir()) {
            let mut item = null;
            if SHCreateItemFromParsingName(wide(at).as_ptr(), null, &IID_SHELL_ITEM, &mut item) >= 0 {
                held.push(item);
                slot::<unsafe extern "system" fn(P, P) -> i32>(d, 12)(d, item);
            }
        }
        slot::<unsafe extern "system" fn(P, *const u16) -> i32>(d, 17)(d, wide(title).as_ptr());
        slot::<unsafe extern "system" fn(P, *const u16) -> i32>(d, 18)(d, wide(button).as_ptr());
        // (failing where it is cancelled)
        ok(slot::<unsafe extern "system" fn(P, P) -> i32>(d, 3)(d, null))?;
        let mut item = null;
        ok(slot::<unsafe extern "system" fn(P, *mut P) -> i32>(d, 20)(d, &mut item))?;
        held.push(item);
        let mut name: *mut u16 = std::ptr::null_mut();
        ok(slot::<unsafe extern "system" fn(P, u32, *mut *mut u16) -> i32>(item, 5)(
            item,
            FILE_SYSTEM_PATH,
            &mut name,
        ))?;
        let n = (0..).take_while(|&i| *name.add(i) != 0).count();
        let path = OsString::from_wide(std::slice::from_raw_parts(name, n));
        CoTaskMemFree(name.cast());
        Some(path.into())
    }
}
