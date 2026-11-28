//! Loading WinFsp's DLL by the full path its installer records.
//!
//! The DLL is delay-loaded (build.rs). `winfsp::winfsp_init` would first load it by its bare
//! name, that is from beside the program before anywhere else: a program run from a folder of
//! downloads would take one planted there. Loaded here first by its full path, the bare name
//! then finds the module already loaded.

use std::ffi::OsString;
use std::fmt;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows_sys::Win32::System::LibraryLoader::{LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

#[cfg(target_arch = "x86_64")]
const DLL: &str = "winfsp-x64.dll";
#[cfg(target_arch = "x86")]
const DLL: &str = "winfsp-x86.dll";
#[cfg(target_arch = "aarch64")]
const DLL: &str = "winfsp-a64.dll";

#[derive(Debug)]
pub enum Error {
    /// No `InstallDir` in the registry: WinFsp is not installed.
    NotInstalled,
    /// The DLL at that path did not load.
    Load(OsString, std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NotInstalled => write!(f, "WinFsp is not installed (https://winfsp.dev)"),
            Error::Load(path, e) => write!(f, "cannot load {}: {e}", path.display()),
        }
    }
}

impl std::error::Error for Error {}

/// The installation directory: the 32-bit installer's key (WOW6432Node) on x64, the key itself
/// on ARM64 (its installer is native).
fn install_dir() -> Option<OsString> {
    ["SOFTWARE\\WOW6432Node\\WinFsp", "SOFTWARE\\WinFsp"].iter().find_map(|key| {
        let key: Vec<u16> = key.encode_utf16().chain([0]).collect();
        let value: Vec<u16> = "InstallDir".encode_utf16().chain([0]).collect();
        let mut buf = [0u16; 1024];
        let mut size = size_of_val(&buf) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1); // without the terminating NUL
        Some(OsString::from_wide(&buf[..len]))
    })
}

/// Loads the DLL, then lets the bindings take it.
pub fn winfsp() -> Result<(), Error> {
    let mut path = install_dir().ok_or(Error::NotInstalled)?;
    if !path.to_string_lossy().ends_with('\\') {
        path.push("\\");
    }
    path.push("bin\\");
    path.push(DLL);
    let wide: Vec<u16> = path.encode_wide().chain([0]).collect();
    // (LOAD_WITH_ALTERED_SEARCH_PATH: what the DLL imports is looked for beside it)
    let module = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
    if module.is_null() {
        return Err(Error::Load(path, std::io::Error::last_os_error()));
    }
    winfsp::winfsp_init().map_err(|e| Error::Load(path, std::io::Error::other(format!("{e:?}"))))?;
    Ok(())
}
