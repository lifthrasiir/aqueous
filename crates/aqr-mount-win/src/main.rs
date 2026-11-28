//! `aqr-mount [MOUNTPOINT]`: a read-only volume of WinFsp, mounted at MOUNTPOINT (a drive
//! letter such as `X:`, or a directory that does not exist yet; the next free drive letter
//! without one) for as long as the program runs. Ending it, however it ends, unmounts.
//!
//! The volume is an aqr-mount tree; for now `OneFile`, `\hello.txt` of fixed contents, until
//! archives are served.

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
    match fs::serve(Box::new(aqr_mount::OneFile::hello()), mount_point.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-mount: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("aqr-mount: this is the program of Windows (WinFsp)");
    std::process::ExitCode::from(2)
}
