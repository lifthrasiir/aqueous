//! What Unix systems start and wait for alike: a worker of no environment, in the root, of its
//! standard input and output piped and its error discarded.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

pub fn spawn(program: &Path, args: &[OsString]) -> io::Result<(std::process::Child, File, File)> {
    let mut c = Command::new(program)
        .args(args)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let to = File::from(OwnedFd::from(c.stdin.take().expect("the input")));
    let from = File::from(OwnedFd::from(c.stdout.take().expect("the output")));
    Ok((c, to, from))
}

/// The exit code of a child, or the signal that ended it negated, waited for a second at most
/// as `try_wait` (`std::process::Child::try_wait`) finds it.
pub fn exit_code(mut try_wait: impl FnMut() -> io::Result<Option<ExitStatus>>) -> Option<i64> {
    use std::os::unix::process::ExitStatusExt;
    for _ in 0..100 {
        if let Some(s) = try_wait().ok()? {
            return Some(s.code().map_or_else(|| -(s.signal().unwrap_or(0) as i64), i64::from));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}
