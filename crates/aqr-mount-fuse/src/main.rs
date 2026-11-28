//! `aqr-mount ARCHIVE MOUNTPOINT`: the archive as a read-only FUSE file system at MOUNTPOINT
//! (an existing directory) for as long as the program runs. SIGINT, SIGTERM and SIGHUP unmount
//! and end it; a process killed otherwise leaves the mount point unusable until `fusermount3
//! -u`. Released so named: the package's binary is aqr-mount-fuse, aqr-mount-win's stub taking
//! the name `aqr-mount` here.
//!
//! The file system is aqr-mount's tree of the archive, as an `aqr --ipc` serves it: `aqr` is
//! `AQR`, else the one beside this program, else the one on `PATH`.

#[cfg(target_os = "linux")]
mod fs;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let mut args = std::env::args_os().skip(1);
    let (Some(archive), Some(mount_point), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: aqr-mount ARCHIVE MOUNTPOINT");
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
    match fs::serve(Box::new(tree), mount_point.as_ref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-mount: {}: {e}", mount_point.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("aqr-mount: this is the program of Linux (FUSE)");
    std::process::ExitCode::from(2)
}
