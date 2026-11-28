//! `aqr-mount [MOUNTPOINT]`: a read-only volume of WinFsp, mounted at MOUNTPOINT (a drive
//! letter such as `X:`, or a directory that does not exist yet; the next free drive letter
//! without one) for as long as the program runs. Ending it, however it ends, unmounts.
//!
//! For now the volume holds one file of fixed contents, `\hello.txt`: the skeleton that an
//! archive's listing and extraction will fill.

#[cfg(windows)]
mod fs;
#[cfg(windows)]
mod load;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    if let Err(e) = load::winfsp() {
        eprintln!("aqr-mount: {e}");
        return ExitCode::from(2);
    }
    let mount_point = std::env::args_os().nth(1);
    match fs::serve(mount_point.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-mount: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("aqr-mount: mounting needs WinFsp (Windows)");
    std::process::ExitCode::from(2)
}
