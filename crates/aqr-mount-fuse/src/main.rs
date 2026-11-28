//! `aqr-mount-fuse MOUNTPOINT`: a read-only FUSE file system at MOUNTPOINT (an existing
//! directory) for as long as the program runs. SIGINT, SIGTERM and SIGHUP unmount and end it;
//! a process killed otherwise leaves the mount point unusable until `fusermount3 -u`.
//!
//! The file system is an aqr-mount tree; for now `OneFile`, `/hello.txt` of fixed contents,
//! until archives are served.

#[cfg(target_os = "linux")]
mod fs;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let mut args = std::env::args_os().skip(1);
    let (Some(mount_point), None) = (args.next(), args.next()) else {
        eprintln!("usage: aqr-mount-fuse MOUNTPOINT");
        return ExitCode::from(2);
    };
    match fs::serve(Box::new(aqr_mount::OneFile::hello()), mount_point.as_ref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-mount-fuse: {}: {e}", mount_point.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("aqr-mount-fuse: this is the program of Linux (FUSE)");
    std::process::ExitCode::from(2)
}
