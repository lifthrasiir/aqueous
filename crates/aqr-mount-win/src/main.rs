//! `aqr-mount ARCHIVE [MOUNTPOINT]`: the archive as a read-only volume of WinFsp, mounted at
//! MOUNTPOINT (a drive letter such as `X:`, or a directory that does not exist yet; the next
//! free drive letter without one) for as long as the program runs. Ending it, however it ends,
//! unmounts.
//!
//! The volume is aqr-mount's tree of the archive, as an `aqr --serve` serves it: `aqr` is
//! `AQR`, else the one beside this program, else the one on `PATH` (where the installer puts
//! its folder).

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
    let mut args = std::env::args_os().skip(1);
    let (Some(archive), mount_point, None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: aqr-mount ARCHIVE [MOUNTPOINT]");
        return ExitCode::from(2);
    };
    let archive = std::path::PathBuf::from(archive);
    let tree = match aqr_mount::ServedTree::open(&aqr_ipc::aqr(), &archive) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("aqr-mount: {e}");
            return ExitCode::FAILURE;
        }
    };
    match fs::serve(Box::new(tree), mount_point.as_deref()) {
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
