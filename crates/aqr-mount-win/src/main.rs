//! `aqr-mount [--json] ARCHIVE [MOUNTPOINT]`: the archive as a read-only volume of WinFsp,
//! mounted at MOUNTPOINT (a drive letter such as `X:`, or a directory that does not exist yet;
//! without one, the last drive letter free) until it is ejected through its control
//! ([`control`]: aqr-win's "Eject") or the program ends, however it ends.
//!
//! The volume is aqr-mount's tree of the archive, as an `aqr --serve` serves it: `aqr` is
//! `AQR`, else the one beside this program, else the one on `PATH` (where the installer puts
//! its folder).
//!
//! With `--json`, what becomes of it is told on standard output as a line, for aqr-win:
//! `{"mounted":"Q:","control":PIPE}` once it is mounted (and its control there), else
//! `{"error":"..."}`.
//! Without, on standard error, for a person.
//!
//! `aqr-mount --install` copies this program (and `COPYING` beside it) into Aqueous's folder,
//! where the installer put aqr-win: its shell extension then offers "Mount as a drive".

#[cfg(windows)]
mod control;
#[cfg(windows)]
mod fs;
#[cfg(windows)]
mod load;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--install" {
        return match install() {
            Ok(to) => {
                println!("aqr-mount: installed in {}", to.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("aqr-mount: not installed: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let json = args.first().is_some_and(|a| a == "--json");
    if json {
        args.remove(0);
    }
    let fail = |code: u8, e: &dyn std::fmt::Display| {
        match json {
            true => println!("{}", serde_json::json!({"error": e.to_string()})),
            false => eprintln!("aqr-mount: {e}"),
        }
        ExitCode::from(code)
    };
    let (archive, mount_point) = match <[_; 1]>::try_from(args) {
        Ok([a]) => (a, None),
        Err(args) => match <[_; 2]>::try_from(args) {
            Ok([a, m]) => (a, Some(m)),
            Err(_) => return fail(2, &"usage: aqr-mount [--json] ARCHIVE [MOUNTPOINT] | --install"),
        },
    };
    if let Err(e) = load::winfsp() {
        return fail(2, &e);
    }
    let archive = std::path::absolute(&archive).unwrap_or(archive.into());
    let mount_point = match mount_point.or_else(free_drive) {
        Some(m) => m.to_string_lossy().into_owned(),
        None => return fail(1, &"no drive letter is free"),
    };
    let tree = match aqr_mount::ServedTree::open(&aqr_ipc::aqr(), &archive) {
        Ok(t) => t,
        Err(e) => return fail(1, &e),
    };
    let mounted = match fs::mount(Box::new(tree), mount_point.as_ref()) {
        Ok(m) => m,
        Err(e) => return fail(1, &format!("{mount_point}: {e}")),
    };
    let (eject, ejected) = std::sync::mpsc::channel();
    let control = control::Mount {
        archive: archive.to_string_lossy().into_owned(),
        mount_point: mount_point.clone(),
        open: mounted.open.clone(),
    };
    if let Err(e) = control::start(control, eject) {
        mounted.unmount();
        return fail(1, &format!("{}: {e}", control::pipe_name(&mount_point)));
    }
    match json {
        true => {
            println!("{}", serde_json::json!({"mounted": mount_point, "control": control::pipe_name(&mount_point)}))
        }
        false => eprintln!("aqr-mount: {} mounted at {mount_point}; end the program to unmount", archive.display()),
    }
    // (until ejected; else until the process is ended, the driver unmounting then)
    _ = ejected.recv();
    mounted.unmount();
    ExitCode::SUCCESS
}

/// The last drive letter free (from Z:, as WinFsp takes them), D: at the first: of no volume
/// (`GetLogicalDrives`), no device name (`QueryDosDeviceW`: a `subst`, another's of the
/// session) and no network drive remembered (`HKCU\Network\L`: one disconnected is in neither,
/// and Explorer would show its name on the volume).
#[cfg(windows)]
fn free_drive() -> Option<std::ffi::OsString> {
    use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, QueryDosDeviceW};
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW};
    // SAFETY: of no arguments
    let used = unsafe { GetLogicalDrives() };
    let taken = |l: char| {
        let device: Vec<u16> = format!("{l}:").encode_utf16().chain([0]).collect();
        let mut target = [0u16; 1024];
        // SAFETY: a name ending in NUL, a buffer of its length
        let named = unsafe { QueryDosDeviceW(device.as_ptr(), target.as_mut_ptr(), target.len() as u32) } != 0;
        let key: Vec<u16> = format!(r"Network\{l}").encode_utf16().chain([0]).collect();
        let mut k = std::ptr::null_mut();
        // SAFETY: a key's name ending in NUL, the key opened closed at once
        let remembered = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_READ, &mut k) } == 0;
        if remembered {
            // SAFETY: the key opened above
            unsafe { RegCloseKey(k) };
        }
        named || remembered
    };
    (3..26u32)
        .rev()
        .filter(|&i| used & (1 << i) == 0)
        .map(|i| (b'A' + i as u8) as char)
        .find(|&l| !taken(l))
        .map(|l| format!("{l}:").into())
}

/// This program copied into Aqueous's folder (`%LOCALAPPDATA%\Programs\Aqueous`, where
/// aqr-win.exe is), and `COPYING` where it is beside it: where.
#[cfg(windows)]
fn install() -> Result<std::path::PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    let dir = std::path::Path::new(&local).join(r"Programs\Aqueous");
    if !dir.join("aqr-win.exe").is_file() {
        return Err(format!("Aqueous is not installed ({} has no aqr-win.exe)", dir.display()));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let to = dir.join("aqr-mount.exe");
    if std::fs::canonicalize(&exe).ok() != std::fs::canonicalize(&to).ok() {
        std::fs::copy(&exe, &to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    let copying = exe.with_file_name("COPYING");
    if copying.is_file() && copying != dir.join("COPYING") {
        std::fs::copy(&copying, dir.join("COPYING")).map_err(|e| format!("COPYING: {e}"))?;
    }
    Ok(dir)
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("aqr-mount: this is the program of Windows (WinFsp)");
    std::process::ExitCode::from(2)
}
