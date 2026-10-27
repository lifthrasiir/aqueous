//! What each system does for the sandbox. A system gives:
//!
//! - `spawn(program, args)`: a worker started (a `Child`), and its standard input and output.
//!   The worker has no other handle of the decoder's, and none of its environment.
//! - `Child::limit_memory(bytes)`: the memory of the worker limited, as the system can, before
//!   each request (the memory limit of the application, beyond what the worker itself takes).
//! - `Child::exit_code()`: how the worker ended, once it did. A `Child` dropped kills it.
//! - `lockdown()`: the worker, once it made all it would make and loaded all it would load,
//!   made unable to reach anything but its standard input and output, before it reads anything
//!   of an archive.
//!
//! Systems without these have the worker refuse to start (`unsupported`), as a worker that is
//! not locked down would protect from nothing.

use std::fs::File;
use std::io;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Child, lockdown, spawn};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Child, lockdown, spawn};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Child, lockdown, spawn};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub use unsupported::{Child, lockdown, spawn};

/// The standard input and output of the worker, for the messages.
pub fn channel() -> io::Result<(File, File)> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: the descriptors are the process's own, which nothing else of the worker uses
        Ok(unsafe { (File::from_raw_fd(0), File::from_raw_fd(1)) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsHandle, OwnedHandle};
        let input: OwnedHandle = io::stdin().as_handle().try_clone_to_owned()?;
        let output: OwnedHandle = io::stdout().as_handle().try_clone_to_owned()?;
        Ok((File::from(input), File::from(output)))
    }
}
