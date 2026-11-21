//! The shortcuts the settings make once the files are written ([`aqr_sfx::config::Shortcut`]):
//! `NAME.lnk` on the desktop or among the programs of the start menu, the user's or all users'
//! (the program run as an administrator), made by the shell's `IShellLinkW` and saved by its
//! `IPersistFile`.

use std::path::{Path, PathBuf};

use aqr_sfx::config::{DEST, Shortcut, ShortcutAt};

/// Makes shortcut `s` to its target under `dest` (the files written there), its name in
/// language `lang`; where it was made.
pub fn make(dest: &Path, s: &Shortcut, lang: &str) -> Result<PathBuf, String> {
    let mut target = dest.to_path_buf();
    s.target.split('/').for_each(|p| target.push(aqr_sfx::sanitized(p.as_bytes())));
    let mut at = folder(s.at, s.all_users)?;
    if let Some(f) = &s.folder {
        f.split('/').for_each(|p| at.push(aqr_sfx::sanitized(p.as_bytes())));
    }
    std::fs::create_dir_all(&at).map_err(|e| format!("{}: {e}", at.display()))?;
    at.push(aqr_sfx::sanitized(format!("{}.lnk", s.name.pick(lang)).as_bytes()));
    let d = dest.display().to_string();
    let args: Vec<String> = s.args.iter().map(|a| a.replace(DEST, &d)).collect();
    let description = s.description.as_ref().map(|t| t.pick(lang).to_string());
    let dir = target.parent().unwrap_or(dest).to_path_buf();
    save(&at, &target, &dir, &args, description.as_deref()).map_err(|e| format!("{}: {e}", at.display()))?;
    Ok(at)
}

/// The folder of shortcuts `at`, all users' where `all`: under `AQR_SFX_SHORTCUTS` where it is
/// set (`desktop`, `start-menu`, `all-users-` before them: for tests), else the system's.
fn folder(at: ShortcutAt, all: bool) -> Result<PathBuf, String> {
    match std::env::var_os("AQR_SFX_SHORTCUTS") {
        Some(d) => {
            let place = if at == ShortcutAt::Desktop { "desktop" } else { "start-menu" };
            Ok(PathBuf::from(d).join(if all { format!("all-users-{place}") } else { place.into() }))
        }
        None => system_folder(at, all),
    }
}

#[cfg(windows)]
fn system_folder(at: ShortcutAt, all: bool) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    // CSIDL_DESKTOPDIRECTORY, CSIDL_PROGRAMS; CSIDL_COMMON_DESKTOPDIRECTORY, CSIDL_COMMON_PROGRAMS
    let csidl = match (at, all) {
        (ShortcutAt::Desktop, false) => 0x10,
        (ShortcutAt::StartMenu, false) => 0x02,
        (ShortcutAt::Desktop, true) => 0x19,
        (ShortcutAt::StartMenu, true) => 0x17,
    };
    let mut buf = [0u16; 260];
    // SAFETY: a buffer of MAX_PATH
    let hr = unsafe { sys::SHGetFolderPathW(std::ptr::null_mut(), csidl, std::ptr::null_mut(), 0, buf.as_mut_ptr()) };
    if hr < 0 {
        return Err(format!("the folder of shortcuts not found ({hr:#x})"));
    }
    let n = buf.iter().position(|&w| w == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])))
}

#[cfg(not(windows))]
fn system_folder(_: ShortcutAt, _: bool) -> Result<PathBuf, String> {
    Err("shortcuts are made on Windows alone".into())
}

/// Saves a shortcut at `at` to `target`, of working directory `dir`, `args` and `description`.
#[cfg(windows)]
fn save(at: &Path, target: &Path, dir: &Path, args: &[String], description: Option<&str>) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let text = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let line: Vec<String> = args.iter().map(|a| crate::run::quote(a)).collect();
    let check = |hr: i32, what: &str| if hr < 0 { Err(format!("{what} ({hr:#x})")) } else { Ok(()) };
    // SAFETY: COM of this thread (initialised here, apartment-threaded), the interfaces' vtables
    // of their documented layouts, each released once
    unsafe {
        let init = sys::CoInitializeEx(std::ptr::null_mut(), 0x2);
        let mut link: *mut *const sys::ShellLinkVtbl = std::ptr::null_mut();
        let hr = sys::CoCreateInstance(
            &sys::CLSID_SHELL_LINK,
            std::ptr::null_mut(),
            0x1,
            &sys::IID_SHELL_LINK_W,
            (&raw mut link).cast(),
        );
        let r = check(hr, "no shell link").and_then(|()| {
            let v = &**link;
            check((v.set_path)(link, wide(target.as_os_str()).as_ptr()), "its target not set")?;
            check((v.set_working_directory)(link, wide(dir.as_os_str()).as_ptr()), "its directory not set")?;
            check((v.set_arguments)(link, text(&line.join(" ")).as_ptr()), "its arguments not set")?;
            if let Some(d) = description {
                check((v.set_description)(link, text(d).as_ptr()), "its description not set")?;
            }
            let mut file: *mut *const sys::PersistFileVtbl = std::ptr::null_mut();
            check(
                (v.unknown.query_interface)(link.cast(), &sys::IID_PERSIST_FILE, (&raw mut file).cast()),
                "not saved",
            )?;
            let r = check(((**file).save)(file, wide(at.as_os_str()).as_ptr(), 1), "not saved");
            ((**file).unknown.release)(file.cast());
            r
        });
        if !link.is_null() {
            ((**link).unknown.release)(link.cast());
        }
        if init >= 0 {
            sys::CoUninitialize();
        }
        r
    }
}

#[cfg(not(windows))]
fn save(_: &Path, _: &Path, _: &Path, _: &[String], _: Option<&str>) -> Result<(), String> {
    Err("shortcuts are made on Windows alone".into())
}

#[cfg(windows)]
mod sys {
    use core::ffi::c_void;

    #[repr(C)]
    pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

    pub const CLSID_SHELL_LINK: Guid = Guid(0x0002_1401, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    pub const IID_SHELL_LINK_W: Guid = Guid(0x0002_14f9, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    pub const IID_PERSIST_FILE: Guid = Guid(0x0000_010b, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);

    type Unused = usize;

    #[repr(C)]
    pub struct UnknownVtbl {
        pub query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
        pub add_ref: Unused,
        pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    /// `IShellLinkW`, in its order.
    #[repr(C)]
    pub struct ShellLinkVtbl {
        pub unknown: UnknownVtbl,
        pub get_path: Unused,
        pub get_id_list: Unused,
        pub set_id_list: Unused,
        pub get_description: Unused,
        pub set_description: unsafe extern "system" fn(*mut *const ShellLinkVtbl, *const u16) -> i32,
        pub get_working_directory: Unused,
        pub set_working_directory: unsafe extern "system" fn(*mut *const ShellLinkVtbl, *const u16) -> i32,
        pub get_arguments: Unused,
        pub set_arguments: unsafe extern "system" fn(*mut *const ShellLinkVtbl, *const u16) -> i32,
        pub get_hotkey: Unused,
        pub set_hotkey: Unused,
        pub get_show_cmd: Unused,
        pub set_show_cmd: Unused,
        pub get_icon_location: Unused,
        pub set_icon_location: Unused,
        pub set_relative_path: Unused,
        pub resolve: Unused,
        pub set_path: unsafe extern "system" fn(*mut *const ShellLinkVtbl, *const u16) -> i32,
    }

    /// `IPersistFile`, in its order.
    #[repr(C)]
    pub struct PersistFileVtbl {
        pub unknown: UnknownVtbl,
        pub get_class_id: Unused,
        pub is_dirty: Unused,
        pub load: Unused,
        pub save: unsafe extern "system" fn(*mut *const PersistFileVtbl, *const u16, i32) -> i32,
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        pub fn CoInitializeEx(reserved: *mut c_void, model: u32) -> i32;
        pub fn CoUninitialize();
        pub fn CoCreateInstance(
            clsid: *const Guid,
            outer: *mut c_void,
            context: u32,
            iid: *const Guid,
            out: *mut *mut c_void,
        ) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        pub fn SHGetFolderPathW(owner: *mut c_void, csidl: i32, token: *mut c_void, flags: u32, path: *mut u16) -> i32;
    }
}
