//! What aqr-win-install leaves to this program, and the uninstall: the folder of this program
//! and `aqr.exe` (`%LOCALAPPDATA%\Programs\Aqueous`, as the installer puts them) for the user
//! alone.
//!
//! - `--install`: the verbs ([`crate::register`]), the folder on the user's `PATH`
//!   (`HKCU\Environment`), and the uninstaller in Settings' Apps
//!   (`HKCU\...\CurrentVersion\Uninstall\Aqueous`: `aqr-win --uninstall`). Errors go to standard
//!   error, which the installer shows.
//! - `--uninstall`: all that taken away, and the files: those of the folder that the installer
//!   wrote, then this program and the folder (left empty) by `cmd.exe` once this program has
//!   exited, since a running program cannot be deleted. Only the program at the place the
//!   uninstaller names (`InstallLocation`) uninstalls.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};

use crate::dialog::wide;
use crate::register::{self, Value};

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Aqueous";
const ENVIRONMENT: &str = "Environment";

/// The files the installer writes, but this program.
const FILES: [&str; 1] = ["aqr.exe"];

/// This program and its folder.
fn here() -> Result<(PathBuf, PathBuf), String> {
    let exe = std::env::current_exe().map_err(|e| format!("This program's path is not known: {e}"))?;
    let dir = exe.parent().ok_or("This program is in no folder.")?.to_path_buf();
    Ok((exe, dir))
}

pub fn install() -> Result<(), String> {
    let (exe, dir) = here()?;
    register::register()?;
    let size: u64 = std::iter::once(exe.clone())
        .chain(FILES.iter().map(|f| dir.join(f)))
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum();
    let uninstall = format!("\"{}\" --uninstall", exe.display());
    let set = |name: &str, value: Value| register::set(UNINSTALL, Some(name), value);
    set("DisplayName", Value::Sz("Aqueous"))?;
    set("DisplayVersion", Value::Sz(env!("CARGO_PKG_VERSION")))?;
    set("DisplayIcon", Value::Sz(&format!("{},0", exe.display())))?;
    set("InstallLocation", Value::Sz(&dir.display().to_string()))?;
    set("UninstallString", Value::Sz(&uninstall))?;
    set("QuietUninstallString", Value::Sz(&format!("{uninstall} --quiet")))?;
    set("NoModify", Value::Dword(1))?;
    set("NoRepair", Value::Dword(1))?;
    // (in KB)
    set("EstimatedSize", Value::Dword(size.div_ceil(1024) as u32))?;
    path(&dir, true)
}

/// The uninstall: `Ok(false)` where it was not wanted.
pub fn uninstall(quiet: bool) -> Result<bool, String> {
    let (exe, dir) = here()?;
    let installed = register::get(UNINSTALL, Some("InstallLocation")).map(|(l, _)| l);
    if !installed.as_deref().is_some_and(|l| same(Path::new(l), &dir)) {
        return Err(match installed {
            Some(l) => format!("Aqueous is installed in {l}, not where this program is ({}).", dir.display()),
            None => "Aqueous is not installed.".into(),
        });
    }
    if !quiet {
        let content = format!("aqr, the verbs of Explorer and {} are taken away.", dir.display());
        if !crate::dialog::confirm("Uninstall Aqueous?", &content) {
            return Ok(false);
        }
    }
    // (the files first: one of them running stops the uninstall before anything else is gone)
    for name in FILES.iter().flat_map(|&f| [f.to_string(), format!("{f}.old"), format!("{f}.new")]) {
        match std::fs::remove_file(dir.join(&name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("{name} is not deleted (is it running?): {e}"));
            }
            _ => {}
        }
    }
    let name = exe.file_name().unwrap().to_string_lossy();
    for leftover in [format!("{name}.old"), format!("{name}.new")] {
        let _ = std::fs::remove_file(dir.join(leftover));
    }
    register::unregister()?;
    path(&dir, false)?;
    register::delete(UNINSTALL)?;
    if !quiet {
        crate::dialog::message(crate::dialog::Icon::Information, "Aqueous is uninstalled", "", None);
    }
    delete_after_exit(&exe, &dir)?;
    Ok(true)
}

/// Whether `a` and `b` are the same path (of no case, with a trailing separator or not).
fn same(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

/// Puts `dir` on the user's `PATH` (`add`) or takes it off; the value of the variable kept as it
/// is otherwise (of `REG_EXPAND_SZ` or not, its variables not expanded).
fn path(dir: &Path, add: bool) -> Result<(), String> {
    let (old, expand) = register::get(ENVIRONMENT, Some("Path")).unwrap_or((String::new(), true));
    let has = |e: &str| !e.is_empty() && same(Path::new(e.trim()), dir);
    let new = if add {
        if old.split(';').any(has) {
            return Ok(());
        }
        // (a trailing `;` kept trailing, for the removal to give `old` back)
        match dir.display() {
            dir if old.is_empty() => dir.to_string(),
            dir if old.ends_with(';') => format!("{old}{dir};"),
            dir => format!("{old};{dir}"),
        }
    } else {
        if !old.split(';').any(has) {
            return Ok(());
        }
        old.split(';').filter(|e| !has(e)).collect::<Vec<_>>().join(";")
    };
    if new.is_empty() {
        register::delete_value(ENVIRONMENT, "Path")?;
    } else {
        register::set(ENVIRONMENT, Some("Path"), if expand { Value::ExpandSz(&new) } else { Value::Sz(&new) })?;
    }
    // (for programs started from now on, Explorer's among them, to take it)
    let what = wide(ENVIRONMENT);
    // SAFETY: a string of NULs for the message, the result not asked for
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            what.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            1000,
            std::ptr::null_mut(),
        )
    };
    Ok(())
}

/// Deletes `exe` and then `dir` where it is empty, by `cmd.exe` (of no window), once `exe` can be:
/// once this program has exited, tried each second for a minute.
fn delete_after_exit(exe: &Path, dir: &Path) -> Result<(), String> {
    let (exe, dir) = (exe.display(), dir.display());
    let script = format!(
        "/d /q /c \"for /l %i in (1,1,60) do @(del /f /q \"{exe}\" 2>nul & if not exist \"{exe}\" (rd \"{dir}\" \
         2>nul & exit) else ping -n 2 127.0.0.1 >nul)\""
    );
    let cmd = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    // (not in `dir`, which it is to remove)
    let cwd = std::env::temp_dir();
    std::process::Command::new(cmd)
        .raw_arg(script)
        .current_dir(cwd)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("{exe} is not deleted: cmd.exe does not run: {e}"))
}
