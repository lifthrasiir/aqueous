//! `--help -v`: the manual rather than the summary of `--help`. On Windows, which has no man(1),
//! the manual as one HTML file (`man.html` beside this program, as the installer puts it:
//! `scripts/man2html.py --single`) opened as the system opens HTML, else the site's page; on the
//! other systems `man 1 aqr`.

/// The manual on the site, where none is installed.
const URL: &str = "https://lifthrasiir.github.io/aqueous/man/aqr.html";

/// Shows the manual and exits: 0 once shown, 1 (told why) where it could not be.
pub(crate) fn open() -> ! {
    match show() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("aqr: {e}");
            std::process::exit(1)
        }
    }
}

#[cfg(not(windows))]
fn show() -> Result<(), String> {
    let status = std::process::Command::new("man")
        .args(["1", "aqr"])
        .status()
        .map_err(|e| format!("man 1 aqr: {e}; the manual is at {URL}"))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("man 1 aqr failed ({status}); the manual is at {URL}")),
    }
}

#[cfg(windows)]
fn show() -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};

    // shell32's ShellExecuteW, looked up here rather than imported: aqr.exe's imports are what its
    // sandbox workers load as they start, and shell32 (with user32 and the rest it takes) stops a
    // worker there (STATUS_DLL_INIT_FAILED; tests/dll_search.rs)
    type ShellExecute =
        unsafe extern "system" fn(*mut c_void, *const u16, *const u16, *const u16, *const u16, i32) -> *mut c_void;
    const SW_SHOWNORMAL: i32 = 1;

    let local = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("man.html"))).filter(|p| p.is_file());
    let target = local.as_ref().map_or(std::ffi::OsStr::new(URL), |p| p.as_os_str());
    let wide: Vec<u16> = target.encode_wide().chain([0]).collect();
    let open: Vec<u16> = "open".encode_utf16().chain([0]).collect();
    let lib: Vec<u16> = "shell32.dll".encode_utf16().chain([0]).collect();
    // SAFETY: of a NUL-terminated name, from the system's directory
    let m = unsafe { LoadLibraryExW(lib.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    // SAFETY: of a module loaded (or null, then none found) and a NUL-terminated name
    let f = (!m.is_null()).then(|| unsafe { GetProcAddress(m, c"ShellExecuteW".as_ptr().cast()) }).flatten();
    let Some(f) = f else {
        return Err(format!("shell32's ShellExecuteW not found; the manual is at {URL}"));
    };
    // SAFETY: the function looked up, of ShellExecuteW's type; strings ended by 0, the others
    // null as allowed
    let r = unsafe {
        std::mem::transmute::<_, ShellExecute>(f)(
            std::ptr::null_mut(),
            open.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // (a value above 32 is success, others an error code: SE_ERR_*)
    match r as usize > 32 {
        true => Ok(()),
        false => Err(format!("{} not opened (error {})", target.display(), r as usize)),
    }
}
