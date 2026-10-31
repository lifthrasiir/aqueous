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
//!
//! The values of `HKEY_CURRENT_USER` are set, read and deleted here for [`crate::install`] too.

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
    RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW,
    RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

use crate::dialog::wide;

/// The class of `.aqr` files.
const CLASS: &str = "Aqueous.Archive";

const COMPRESS: [&str; 2] = [r"*\shell\Aqueous.compress", r"Directory\shell\Aqueous.compress"];

pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("This program's path is not known: {e}"))?;
    let command = |verb: &str| format!("\"{}\" {verb} \"%1\"", exe.display());
    // (the icon of resource 1, aqr-win.rc)
    let icon = format!("\"{}\",-1", exe.display());
    let set = |key: &str, name: Option<&str>, value: &str| set(&classes(key), name, Value::Sz(value));
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
        delete(&classes(key))?;
    }
    // (`.aqr` left as it is where another program has it)
    if get(&classes(".aqr"), None).is_some_and(|(v, _)| v == CLASS) {
        delete(&classes(".aqr"))?;
    }
    notify();
    Ok(())
}

/// Tells Explorer that the verbs changed.
fn notify() {
    // SAFETY: no items with this event
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
}

/// `key` under `Software\Classes`.
fn classes(key: &str) -> String {
    format!(r"Software\Classes\{key}")
}

fn error(what: &str, key: &str, e: WIN32_ERROR) -> String {
    format!(r"{what} HKEY_CURRENT_USER\{key}: {}", std::io::Error::from_raw_os_error(e as i32))
}

/// A value of the registry.
pub enum Value<'a> {
    Sz(&'a str),
    ExpandSz(&'a str),
    Dword(u32),
}

/// Sets value `name` (`None`: the default) of `key` (of `HKEY_CURRENT_USER`) to `value`, the key
/// made where it is not.
pub fn set(key: &str, name: Option<&str>, value: Value) -> Result<(), String> {
    let mut h: HKEY = std::ptr::null_mut();
    let p = wide(key);
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
    let string = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain([0]).flat_map(u16::to_le_bytes).collect();
    let (kind, data): (REG_VALUE_TYPE, Vec<u8>) = match value {
        Value::Sz(s) => (REG_SZ, string(s)),
        Value::ExpandSz(s) => (REG_EXPAND_SZ, string(s)),
        Value::Dword(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
    };
    // SAFETY: an open key, a name of NULs or none, and the bytes of the data (a string's with its
    // NUL)
    let e = unsafe {
        let e = RegSetValueExW(
            h,
            name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            0,
            kind,
            data.as_ptr(),
            data.len() as u32,
        );
        RegCloseKey(h);
        e
    };
    if e != ERROR_SUCCESS {
        return Err(error("Not set:", key, e));
    }
    Ok(())
}

/// The string `name` (`None`: the default) of `key` (of `HKEY_CURRENT_USER`), its variables not
/// expanded, and whether it is of `REG_EXPAND_SZ`; `None` where there is none.
pub fn get(key: &str, name: Option<&str>) -> Option<(String, bool)> {
    let p = wide(key);
    let name = name.map(wide);
    let mut buf = vec![0u16; 256];
    loop {
        let mut len = (buf.len() * 2) as u32;
        let mut kind: REG_VALUE_TYPE = 0;
        // SAFETY: a key of NULs, a name of NULs or none, room for the value and its length
        let e = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                p.as_ptr(),
                name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                &mut kind,
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        match e {
            ERROR_SUCCESS => {
                buf.truncate((len as usize / 2).saturating_sub(1));
                return Some((String::from_utf16_lossy(&buf), kind == REG_EXPAND_SZ));
            }
            ERROR_MORE_DATA => buf.resize((len as usize).div_ceil(2) + 1, 0),
            _ => return None,
        }
    }
}

/// Deletes `key` (of `HKEY_CURRENT_USER`) and all under it, where it is.
pub fn delete(key: &str) -> Result<(), String> {
    let p = wide(key);
    // SAFETY: a key of NULs
    match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, p.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        e => Err(error("Not deleted:", key, e)),
    }
}

/// Deletes value `name` of `key` (of `HKEY_CURRENT_USER`), where it is.
pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let (p, n) = (wide(key), wide(name));
    // SAFETY: a key and a name of NULs
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        e => Err(error("Not deleted:", key, e)),
    }
}
