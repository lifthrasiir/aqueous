//! The verbs in the registry, for the user alone (`HKCU\Software\Classes`: no administrator
//! needed):
//!
//! - `*\shell\Aqueous.compress` and `Directory\shell\Aqueous.compress`: `aqr-win compress`,
//!   with this program's icon; and beside it `Aqueous.more`, the menu "Aqueous" of the other
//!   verbs ([`more`]: "Compress with passphrase…", `aqr -P`; then, after a separator, "Compress
//!   into .zip", ".7z" and the other formats `aqr` makes, [`OTHER_FORMATS`]). The menu is made
//!   by the registry alone: `SubCommands` (empty) on the key has its own `shell` key taken as
//!   the submenu's verbs, in the order of their keys' names; `CommandFlags` of
//!   `ECF_SEPARATORBEFORE` puts the separator.
//! - `.aqr` of the class `Aqueous.Archive`, whose verbs are `extract` (the default: a double
//!   click) and `test`, and `name-version` and `extract-version` ([`crate::version`]: one file
//!   at a time, `MultiSelectModel` `Single`), and whose icon (`DefaultIcon`) is resource 2: this program's drop with
//!   a label, "AQR" (`scripts/icons.py`).
//! - The other formats `aqr` extracts ([`FORMATS`]), each of a class of its own
//!   (`Aqueous.zip`, ...) with the same verbs and its own icon (the drop in gray, labeled).
//!   None is taken from another program: the class goes in the extension's `OpenWithProgids`,
//!   and is its default only where the extension has none (no class in `HKEY_CLASSES_ROOT`, no
//!   `UserChoice`). Elsewhere Windows (8 on) asks which program to open the next such file
//!   with, Aqueous being new; Windows 7 only lists it under "Open with".
//! - Aqueous's capabilities (`Software\Aqueous\Capabilities`, named in
//!   `Software\RegisteredApplications`): those extensions and their classes, so that Settings'
//!   default apps lists Aqueous for them.
//!
//! The texts that Explorer shows (the verbs, the types' names, the description in Settings) are
//! indirect strings, `@aqr-win.exe,-ID`: Windows' string table of aqr-win.rc (aqr-l10n's `RC`),
//! read by Explorer in the user's language as it is now; the default values beside them are in
//! the language of the registration.
//!
//! The verbs take any number of paths chosen (`MultiSelectModel` `Player`; Explorer runs a
//! process for each, [`crate::gather`]). On Windows 11 they are under "Show more options": the
//! menu above takes only `IExplorerCommand` handlers of packaged applications.
//!
//! The values of `HKEY_CURRENT_USER` are set, read and deleted here for [`crate::install`] too.

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

use aqr_l10n::{Msg, shell, types, win};

use crate::dialog::wide;

/// The class of `.aqr` files.
const CLASS: &str = "Aqueous.Archive";

/// The formats `aqr` extracts besides its own (aqr-unpack's, by the last extension, as Explorer
/// tells a file's type): the extension, the resource of its icon (aqr-win.rc) and the name of
/// its type. Their classes are `Aqueous.` and the extension.
const FORMATS: [(&str, u16, Msg); 14] = [
    ("zip", 3, types::ZIP),
    ("7z", 4, types::SEVEN_Z),
    ("rar", 5, types::RAR),
    ("alz", 6, types::ALZ),
    ("egg", 7, types::EGG),
    ("tar", 8, types::TAR),
    ("tgz", 9, types::TGZ),
    ("tbz2", 10, types::TBZ2),
    ("txz", 11, types::TXZ),
    ("gz", 12, types::GZ),
    ("bz2", 13, types::BZ2),
    ("xz", 14, types::XZ),
    ("lzma", 15, types::LZMA),
    ("zst", 16, types::ZST),
];

const CAPABILITIES: &str = r"Software\Aqueous\Capabilities";
const APPLICATIONS: &str = r"Software\RegisteredApplications";
/// Aqueous's name in [`APPLICATIONS`].
const APPLICATION: &str = "Aqueous";
/// Where Explorer keeps the program the user chose for an extension (`{}`: `.zip`, ...).
const USER_CHOICE: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{}\UserChoice";

const COMPRESS: [&str; 2] = [r"*\shell\Aqueous.compress", r"Directory\shell\Aqueous.compress"];
/// The menu "Aqueous", of the verbs of [`more`].
const MENU: [&str; 2] = [r"*\shell\Aqueous.more", r"Directory\shell\Aqueous.more"];
/// Where "Compress with passphrase…" was before it went into [`MENU`]: taken away.
const OLD: [&str; 2] = [r"*\shell\Aqueous.compress-passphrase", r"Directory\shell\Aqueous.compress-passphrase"];

/// The formats other than Aqueous's that `aqr -c --format` makes, each a verb
/// `compress-FORMAT` in [`MENU`] ("Compress into .FORMAT", its text of `shell` as Explorer reads
/// it from the string table), after the separator.
pub const OTHER_FORMATS: [(&str, Msg); 2] = [("zip", shell::COMPRESS_ZIP), ("7z", shell::COMPRESS_7Z)];

/// `ECF_SEPARATORBEFORE` of `CommandFlags`.
const SEPARATOR_BEFORE: u32 = 0x20;

/// The verbs of [`MENU`], in order: the key's name (which orders them), the text and the verb.
fn more() -> Vec<(String, Msg, String)> {
    let mut v = vec![("1-passphrase".into(), shell::COMPRESS_PASSPHRASE, "compress-passphrase".into())];
    for (i, (f, text)) in OTHER_FORMATS.iter().enumerate() {
        v.push((format!("{}-{f}", i + 2), *text, format!("compress-{f}")));
    }
    v
}

/// What [`register`] made, as the user is told: the verbs (those of [`MENU`] by [`more`]) and
/// the formats (of [`FORMATS`]).
pub fn told() -> String {
    let menu: Vec<String> = more().into_iter().map(|(_, text, _)| win::quoted(text.text())).collect();
    let formats: Vec<String> = FORMATS.iter().map(|(ext, ..)| format!(".{ext}")).collect();
    win::told(shell::compress(), menu.join(", "), shell::extract(), shell::test(), formats.join(" "))
}

pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(win::unknown_path)?;
    let command = |verb: &str| format!("\"{}\" {verb} \"%1\"", exe.display());
    // (a text of the string table, as Explorer reads it in the user's language)
    let indirect = |m: Msg| format!("@{},-{}", exe.display(), m.resource());
    // (the icon of resource 1, this program's; files' are 2 on, aqr-win.rc)
    let icon = format!("\"{}\",-1", exe.display());
    let set = |key: &str, name: Option<&str>, value: &str| set(&classes(key), name, Value::Sz(value));
    for key in OLD {
        delete(&classes(key))?;
    }
    let verb = |key: &str, text: Msg, verb: &str| -> Result<(), String> {
        set(key, None, text.text())?;
        set(key, Some("MUIVerb"), &indirect(text))?;
        set(key, Some("MultiSelectModel"), "Player")?;
        set(&format!(r"{key}\command"), None, &command(verb))
    };
    for (key, menu) in COMPRESS.into_iter().zip(MENU) {
        verb(key, shell::COMPRESS, "compress")?;
        set(key, Some("Icon"), &icon)?;
        // (made anew: no verbs left of another version)
        delete(&classes(menu))?;
        set(menu, Some("MUIVerb"), "Aqueous")?;
        set(menu, Some("SubCommands"), "")?;
        set(menu, Some("Icon"), &icon)?;
        for (i, (name, text, v)) in more().into_iter().enumerate() {
            let key = format!(r"{menu}\shell\{name}");
            verb(&key, text, &v)?;
            if i == 1 {
                self::set(&classes(&key), Some("CommandFlags"), Value::Dword(SEPARATOR_BEFORE))?;
            }
        }
    }
    let class = |class: &str, name: Msg, icon: u16| -> Result<(), String> {
        set(class, None, name.text())?;
        set(class, Some("FriendlyTypeName"), &indirect(name))?;
        set(&format!(r"{class}\DefaultIcon"), None, &format!("\"{}\",-{icon}", exe.display()))?;
        set(&format!(r"{class}\shell"), None, "extract")?;
        for (v, text) in [("extract", shell::EXTRACT), ("test", shell::TEST)] {
            verb(&format!(r"{class}\shell\{v}"), text, v)?;
        }
        Ok(())
    };
    let capability =
        |ext: &str, class: &str| self::set(&format!(r"{CAPABILITIES}\FileAssociations"), Some(ext), Value::Sz(class));
    set(".aqr", None, CLASS)?;
    class(CLASS, types::AQR, 2)?;
    // (the versions of an archive: one archive at a time, `.aqr` files alone)
    for (v, text) in [("extract-version", shell::EXTRACT_VERSION), ("name-version", shell::NAME_VERSION)] {
        let key = format!(r"{CLASS}\shell\{v}");
        verb(&key, text, v)?;
        set(&key, Some("MultiSelectModel"), "Single")?;
    }
    capability(".aqr", CLASS)?;
    for (ext, icon, name) in FORMATS {
        let (ext, progid) = (format!(".{ext}"), format!("Aqueous.{ext}"));
        class(&progid, name, icon)?;
        set(&format!(r"{ext}\OpenWithProgids"), Some(&progid), "")?;
        if !has_default(&ext) {
            set(&ext, None, &progid)?;
        }
        capability(&ext, &progid)?;
    }
    for (name, value) in [
        ("ApplicationName", "Aqueous"),
        ("ApplicationDescription", &indirect(shell::DESCRIPTION)),
        ("ApplicationIcon", &icon),
    ] {
        self::set(CAPABILITIES, Some(name), Value::Sz(value))?;
    }
    self::set(APPLICATIONS, Some(APPLICATION), Value::Sz(CAPABILITIES))?;
    notify();
    Ok(())
}

pub fn unregister() -> Result<(), String> {
    for key in COMPRESS.iter().chain(&MENU).chain(&OLD).copied().chain([CLASS]) {
        delete(&classes(key))?;
    }
    // (`.aqr` left as it is where another program has it)
    if get(&classes(".aqr"), None).is_some_and(|(v, _)| v == CLASS) {
        delete(&classes(".aqr"))?;
    }
    for (ext, ..) in FORMATS {
        let (ext, progid) = (format!(".{ext}"), format!("Aqueous.{ext}"));
        delete(&classes(&progid))?;
        delete_value(&classes(&format!(r"{ext}\OpenWithProgids")), &progid)?;
        // (the default only where it is still Aqueous's)
        if get(&classes(&ext), None).is_some_and(|(v, _)| v == progid) {
            delete_value(&classes(&ext), "")?;
        }
    }
    delete_value(APPLICATIONS, APPLICATION)?;
    delete(r"Software\Aqueous")?;
    notify();
    Ok(())
}

/// Whether extension `ext` (`.zip`) opens with a program already: a class in
/// `HKEY_CLASSES_ROOT` (the machine's and the user's), or the user's choice.
fn has_default(ext: &str) -> bool {
    get_from(HKEY_CLASSES_ROOT, ext, None).is_some_and(|(v, _)| !v.is_empty())
        || get(&USER_CHOICE.replace("{}", ext), Some("ProgId")).is_some_and(|(v, _)| !v.is_empty())
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

/// The error `e` of the system's, as told.
fn os(e: WIN32_ERROR) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
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
        return Err(win::registry_not_made(key, os(e)));
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
        return Err(win::registry_not_set(key, os(e)));
    }
    Ok(())
}

/// The string `name` (`None`: the default) of `key` (of `HKEY_CURRENT_USER`), its variables not
/// expanded, and whether it is of `REG_EXPAND_SZ`; `None` where there is none.
pub fn get(key: &str, name: Option<&str>) -> Option<(String, bool)> {
    get_from(HKEY_CURRENT_USER, key, name)
}

/// [`get`] of `root`.
fn get_from(root: HKEY, key: &str, name: Option<&str>) -> Option<(String, bool)> {
    let p = wide(key);
    let name = name.map(wide);
    let mut buf = vec![0u16; 256];
    loop {
        let mut len = (buf.len() * 2) as u32;
        let mut kind: REG_VALUE_TYPE = 0;
        // SAFETY: a key of NULs, a name of NULs or none, room for the value and its length
        let e = unsafe {
            RegGetValueW(
                root,
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
        e => Err(win::registry_not_deleted(key, os(e))),
    }
}

/// Deletes value `name` of `key` (of `HKEY_CURRENT_USER`), where it is.
pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let (p, n) = (wide(key), wide(name));
    // SAFETY: a key and a name of NULs
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        e => Err(win::registry_not_deleted(key, os(e))),
    }
}

#[cfg(test)]
mod tests {
    use super::{FORMATS, OTHER_FORMATS, told};

    /// The message of `--register` tells the menu's verbs and the formats as they are.
    #[test]
    fn told_as_registered() {
        let t = told();
        assert!(t.contains("\"Compress with passphrase…\", \"Compress into .zip\", \"Compress into .7z\""), "{t}");
        assert!(t.contains("(.zip .7z .rar") && t.contains(".zst)"), "{t}");
    }

    /// `FORMATS`' icons are the resources of aqr-win.rc by those numbers, and are there.
    #[test]
    fn icons_of_formats() {
        let rc = include_str!("../aqr-win.rc");
        let icon = |n: u16| rc.lines().find_map(|l| l.strip_prefix(&format!("{n} ICON ")).map(|f| f.trim_matches('"')));
        assert_eq!(icon(2), Some("icons/aqr.ico"));
        for (ext, n, _) in FORMATS {
            assert_eq!(icon(n), Some(format!("icons/{ext}.ico").as_str()), "{ext}");
            assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("icons/{ext}.ico")).is_file());
        }
        assert_eq!(rc.lines().filter(|l| l.contains(" ICON ")).count(), 2 + FORMATS.len());
    }

    /// Each format of `OTHER_FORMATS` has its own verb's text.
    #[test]
    fn other_formats_told() {
        for (f, text) in OTHER_FORMATS {
            assert!(text.text().ends_with(&format!(".{f}")), "{f}");
        }
    }
}
