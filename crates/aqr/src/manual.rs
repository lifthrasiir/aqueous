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
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let local = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("man.html"))).filter(|p| p.is_file());
    let target = local.as_ref().map_or(std::ffi::OsStr::new(URL), |p| p.as_os_str());
    let wide: Vec<u16> = target.encode_wide().chain([0]).collect();
    let open: Vec<u16> = "open".encode_utf16().chain([0]).collect();
    // SAFETY: strings ended by 0, the others null as allowed
    let r = unsafe {
        ShellExecuteW(
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
