//! The folder picked where the files go (the settings' `extract.ask`, its dialog's Browse): the
//! shell's `IFileOpenDialog` of folders (Vista on).

use core::ffi::c_void;

use crate::banner::slot;

type P = *mut c_void;

/// A folder picked by the user in a dialog owned by window `owner`, opened at `start` (or the
/// nearest folder there above it); `None` where none was.
pub fn pick(owner: P, start: &str) -> Option<String> {
    // SAFETY: COM of this thread (initialised here, apartment-threaded), each interface held
    // released once
    unsafe {
        let init = sys::CoInitializeEx(std::ptr::null_mut(), 0x2);
        let mut held = Vec::new();
        let r = run(owner, start, &mut held);
        for o in held.into_iter().rev() {
            slot::<unsafe extern "system" fn(P) -> u32>(o, 2)(o);
        }
        if init >= 0 {
            sys::CoUninitialize();
        }
        r
    }
}

/// [`pick`]'s work, the interfaces it makes noted in `held`.
unsafe fn run(owner: P, start: &str, held: &mut Vec<P>) -> Option<String> {
    let ok = |hr: i32| (hr >= 0).then_some(());
    let null = std::ptr::null_mut();
    // SAFETY: the interfaces' methods at the slots of their documented vtables (ShObjIdl_core.h)
    unsafe {
        let mut d = null;
        ok(sys::CoCreateInstance(&sys::CLSID_FILE_OPEN_DIALOG, null, 0x1, &sys::IID_FILE_OPEN_DIALOG, &mut d))?;
        held.push(d);
        // GetOptions, SetOptions: FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM
        let mut o = 0u32;
        ok(slot::<unsafe extern "system" fn(P, *mut u32) -> i32>(d, 10)(d, &mut o))?;
        ok(slot::<unsafe extern "system" fn(P, u32) -> i32>(d, 9)(d, o | 0x20 | 0x40))?;
        // SetFolder: the nearest folder there of `start`
        let mut at = std::path::PathBuf::from(start.trim());
        while !at.as_os_str().is_empty() && !at.is_dir() {
            if !at.pop() {
                break;
            }
        }
        if at.is_dir() {
            let wide: Vec<u16> = at.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
            let mut item = null;
            if sys::SHCreateItemFromParsingName(wide.as_ptr(), null, &sys::IID_SHELL_ITEM, &mut item) >= 0 {
                held.push(item);
                slot::<unsafe extern "system" fn(P, P) -> i32>(d, 12)(d, item);
            }
        }
        // Show (failing where it is cancelled), GetResult, its path (SIGDN_FILESYSPATH)
        ok(slot::<unsafe extern "system" fn(P, P) -> i32>(d, 3)(d, owner))?;
        let mut item = null;
        ok(slot::<unsafe extern "system" fn(P, *mut P) -> i32>(d, 20)(d, &mut item))?;
        held.push(item);
        let mut name: *mut u16 = std::ptr::null_mut();
        ok(slot::<unsafe extern "system" fn(P, u32, *mut *mut u16) -> i32>(item, 5)(item, 0x8005_8000, &mut name))?;
        let n = (0..).take_while(|&i| *name.add(i) != 0).count();
        let path = String::from_utf16_lossy(std::slice::from_raw_parts(name, n));
        sys::CoTaskMemFree(name.cast());
        Some(path)
    }
}

mod sys {
    use super::P;
    use crate::banner::Guid;

    pub const CLSID_FILE_OPEN_DIALOG: Guid =
        Guid(0xdc1c_5a9c, 0xe88a, 0x4dde, [0xa5, 0xa1, 0x60, 0xf8, 0x2a, 0x20, 0xae, 0xf7]);
    pub const IID_FILE_OPEN_DIALOG: Guid =
        Guid(0xd57c_7288, 0xd4ad, 0x4768, [0xbe, 0x02, 0x9d, 0x96, 0x95, 0x32, 0xd9, 0x60]);
    pub const IID_SHELL_ITEM: Guid =
        Guid(0x4382_6d1e, 0xe718, 0x42ee, [0xbc, 0x55, 0xa1, 0xe2, 0x61, 0xc3, 0x7b, 0xfe]);

    #[link(name = "ole32")]
    unsafe extern "system" {
        pub fn CoInitializeEx(reserved: P, model: u32) -> i32;
        pub fn CoUninitialize();
        pub fn CoCreateInstance(clsid: *const Guid, outer: P, context: u32, iid: *const Guid, out: *mut P) -> i32;
        pub fn CoTaskMemFree(p: P);
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        pub fn SHCreateItemFromParsingName(path: *const u16, bind: P, iid: *const Guid, out: *mut P) -> i32;
    }
}
