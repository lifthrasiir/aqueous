//! The installer of Aqueous on Windows, for the user alone (no administrator needed): `aqr.exe`
//! and `aqr-win.exe` into `%LOCALAPPDATA%\Programs\Aqueous`, then `aqr-win --install`, which
//! registers the verbs in Explorer, the folder on the user's `PATH` and the uninstaller
//! (`aqr-win --uninstall`, in Settings' Apps).
//!
//! usage: aqr-win-install [--quiet]
//!
//! The two are carried in the binary (`payload`, packed by `build.rs` from `target/dist`).
//! Files there already are replaced; one running is renamed aside first (`*.old`, deleted by
//! the next install or the uninstall).

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod payload;

#[cfg(not(windows))]
fn main() {
    eprintln!("aqr-win-install: for Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    use aqr_l10n::{install as l, shell};

    aqr_l10n::init(&aqr_l10n::system_languages());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = match &args[..] {
        [] => false,
        [q] if q == "--quiet" => true,
        _ => {
            ui::message(ui::Icon::Error, l::not_option(), "usage: aqr-win-install [--quiet]");
            return ExitCode::FAILURE;
        }
    };
    let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join(r"Programs\Aqueous"))
    else {
        ui::message(ui::Icon::Error, l::failed(), l::no_local_app_data());
        return ExitCode::FAILURE;
    };
    if !quiet {
        if !ui::confirm(&l::ask(env!("AQR_VERSION")), &l::what(dir.display())) {
            return ExitCode::SUCCESS;
        }
    }
    match install(&dir) {
        Ok(()) => {
            if !quiet {
                let done = l::done(shell::compress(), shell::extract(), shell::test());
                ui::message(ui::Icon::Information, l::installed(), &done);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            ui::message(ui::Icon::Error, l::failed(), &e);
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn install(dir: &std::path::Path) -> Result<(), String> {
    use aqr_l10n::install as l;

    if PAYLOAD.is_empty() {
        return Err(l::empty().into());
    }
    let files = payload::unpack(PAYLOAD).map_err(l::damaged)?;
    std::fs::create_dir_all(dir).map_err(|e| l::not_made(dir.display(), e))?;
    for f in &files {
        let to = dir.join(&f.name);
        let old = dir.join(format!("{}.old", f.name));
        let _ = std::fs::remove_file(&old);
        let new = dir.join(format!("{}.new", f.name));
        std::fs::write(&new, &f.data).map_err(|e| l::not_written(new.display(), e))?;
        if std::fs::rename(&new, &to).is_err() {
            // (running, which a rename aside allows)
            let moved = std::fs::rename(&to, &old).and_then(|()| std::fs::rename(&new, &to));
            if let Err(e) = moved {
                let _ = std::fs::remove_file(&new);
                return Err(l::not_replaced(to.display(), e));
            }
        }
    }
    let out = std::process::Command::new(dir.join("aqr-win.exe")).arg("--install").output().map_err(l::not_run)?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// Task dialogs (common controls 6, which the manifest asks for) with this program's icon.
#[cfg(windows)]
mod ui {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, S_OK, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::{
        LIM_LARGE, LIM_SMALL, LoadIconMetric, TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TD_ERROR_ICON, TD_INFORMATION_ICON,
        TDCBF_CANCEL_BUTTON, TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDN_CREATED, TaskDialogIndirect,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{HICON, ICON_BIG, ICON_SMALL, SendMessageW, WM_SETICON};
    use windows_sys::core::{HRESULT, PCWSTR};

    pub enum Icon {
        Information,
        Error,
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
    }

    /// The id of the button "Install" (apart from those of the common buttons).
    const INSTALL: i32 = 100;

    unsafe extern "system" fn callback(hwnd: HWND, msg: u32, _: WPARAM, _: LPARAM, _: isize) -> HRESULT {
        if msg as i32 == TDN_CREATED {
            for (size, which) in [(LIM_SMALL, ICON_SMALL), (LIM_LARGE, ICON_BIG)] {
                let mut icon: HICON = std::ptr::null_mut();
                // SAFETY: this program's module, resource 1 by its number (aqr-win-install.rc), the
                // icon written to a local (kept as long as the program runs)
                unsafe {
                    if LoadIconMetric(GetModuleHandleW(std::ptr::null()), 1 as PCWSTR, size, &mut icon) == S_OK {
                        SendMessageW(hwnd, WM_SETICON, which as usize, icon as isize);
                    }
                }
            }
        }
        S_OK
    }

    /// Shows a dialog until it is closed: the button that closed it.
    fn show(icon: Option<Icon>, main: &str, content: &str, ask: bool) -> i32 {
        let (title, main, content, install) =
            (wide("Aqueous"), wide(main), wide(content), wide(aqr_l10n::install::install()));
        let buttons = [TASKDIALOG_BUTTON { nButtonID: INSTALL, pszButtonText: install.as_ptr() }];
        // SAFETY: a struct of C's, all of whose fields may be zeroes (nulls)
        let mut c: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
        c.cbSize = size_of::<TASKDIALOGCONFIG>() as u32;
        c.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION;
        c.pszWindowTitle = title.as_ptr();
        c.pszMainInstruction = main.as_ptr();
        c.pszContent = content.as_ptr();
        c.Anonymous1.pszMainIcon = match icon {
            Some(Icon::Information) => TD_INFORMATION_ICON,
            Some(Icon::Error) => TD_ERROR_ICON,
            None => std::ptr::null(),
        };
        if ask {
            c.dwCommonButtons = TDCBF_CANCEL_BUTTON;
            c.cButtons = buttons.len() as u32;
            c.pButtons = buttons.as_ptr();
            c.nDefaultButton = INSTALL;
        } else {
            c.dwCommonButtons = TDCBF_CLOSE_BUTTON;
        }
        c.pfCallback = Some(callback);
        let mut button = 0;
        // SAFETY: a dialog whose strings and buttons outlive it, the button written to a local
        unsafe { TaskDialogIndirect(&c, &mut button, std::ptr::null_mut(), std::ptr::null_mut()) };
        button
    }

    pub fn message(icon: Icon, main: &str, content: &str) {
        show(Some(icon), main, content, false);
    }

    /// Whether "Install" was chosen.
    pub fn confirm(main: &str, content: &str) -> bool {
        show(None, main, content, true) == INSTALL
    }
}
