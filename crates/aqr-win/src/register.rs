//! The verbs in the registry, for the user alone (`HKCU\Software\Classes`: no administrator
//! needed):
//!
//! - `*\shell\Aqueous.compress` and `Directory\shell\Aqueous.compress`: `aqr-win compress`,
//!   with this program's icon.
//! - `.aqr` of the class `Aqueous.Archive`, whose verbs are `extract` (the default: a double
//!   click) and `test`, and whose icon is this program's (`DefaultIcon`).
//!
//! The verbs take any number of paths chosen (`MultiSelectModel` `Player`; Explorer runs a
//! process for each, [`crate::gather`]). On Windows 11 they are under "Show more options": the
//! menu above takes only `IExplorerCommand` handlers of packaged applications.

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

use crate::dialog::wide;

const CLASSES: &str = r"Software\Classes";

/// The class of `.aqr` files.
const CLASS: &str = "Aqueous.Archive";

const COMPRESS: [&str; 2] = [r"*\shell\Aqueous.compress", r"Directory\shell\Aqueous.compress"];

pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("This program's path is not known: {e}"))?;
    let command = |verb: &str| format!("\"{}\" {verb} \"%1\"", exe.display());
    // (the icon of resource 1, aqr-win.rc)
    let icon = format!("\"{}\",-1", exe.display());
    for key in COMPRESS {
        set(key, None, "Compress with Aqueous")?;
        set(key, Some("MultiSelectModel"), "Player")?;
        set(key, Some("Icon"), &icon)?;
        set(&format!(r"{key}\command"), None, &command("compress"))?;
    }
    set(".aqr", None, CLASS)?;
    set(CLASS, None, "Aqueous archive")?;
    set(&format!(r"{CLASS}\DefaultIcon"), None, &icon)?;
    set(&format!(r"{CLASS}\shell"), None, "extract")?;
    for (verb, text) in [("extract", "Extract here"), ("test", "Test")] {
        let key = format!(r"{CLASS}\shell\{verb}");
        set(&key, None, text)?;
        set(&key, Some("MultiSelectModel"), "Player")?;
        set(&format!(r"{key}\command"), None, &command(verb))?;
    }
    notify();
    Ok(())
}

pub fn unregister() -> Result<(), String> {
    for key in COMPRESS.iter().copied().chain([CLASS]) {
        delete(key)?;
    }
    // (`.aqr` left as it is where another program has it)
    if get(".aqr").as_deref() == Some(CLASS) {
        delete(".aqr")?;
    }
    notify();
    Ok(())
}

/// Tells Explorer that the verbs changed.
fn notify() {
    // SAFETY: no items with this event
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
}

fn path(key: &str) -> Vec<u16> {
    wide(format!(r"{CLASSES}\{key}"))
}

fn error(what: &str, key: &str, e: WIN32_ERROR) -> String {
    format!(r"{what} HKEY_CURRENT_USER\{CLASSES}\{key}: {}", std::io::Error::from_raw_os_error(e as i32))
}

/// Sets value `name` (`None`: the default) of `key` to `value`, the key made where it is not.
fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let mut h: HKEY = std::ptr::null_mut();
    let p = path(key);
    // SAFETY: a key of NULs, and a handle written to a local
    let e = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            p.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut h,
            std::ptr::null_mut(),
        )
    };
    if e != ERROR_SUCCESS {
        return Err(error("Not made:", key, e));
    }
    let name = name.map(wide);
    let data: Vec<u16> = std::ffi::OsStr::new(value).encode_wide().chain([0]).collect();
    // SAFETY: an open key, a name of NULs or none, and the bytes of the data with its NUL
    let e = unsafe {
        let e = RegSetValueExW(
            h,
            name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            0,
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        );
        RegCloseKey(h);
        e
    };
    if e != ERROR_SUCCESS {
        return Err(error("Not set:", key, e));
    }
    Ok(())
}

/// The default value of `key`, where there is one.
fn get(key: &str) -> Option<String> {
    let p = path(key);
    let mut buf = vec![0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: a key of NULs, room for the value and its length
    let e = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            p.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if e != ERROR_SUCCESS {
        return None;
    }
    buf.truncate((len as usize / 2).saturating_sub(1));
    Some(String::from_utf16_lossy(&buf))
}

/// Deletes `key` and all under it, where it is.
fn delete(key: &str) -> Result<(), String> {
    let p = path(key);
    // SAFETY: a key of NULs
    match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, p.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        e => Err(error("Not deleted:", key, e)),
    }
}
