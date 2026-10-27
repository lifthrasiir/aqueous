//! Workers on macOS, locked down by Seatbelt (the sandbox of `sandbox_init_with_parameters`,
//! deprecated in name but what Chromium and Firefox still lock their content processes with):
//! of no descriptors but their standard input, output and error and the socket of arenas, and a
//! profile denying
//! everything that the system checks (files, sockets, processes, Mach services, sysctls).
//! What the profile does not check, memory and threads and the task's own Mach ports (which
//! Wasmtime's traps are taken on), goes on.
//!
//! The memory is limited by the decoder, which watches the worker's footprint (what the system
//! counts against a process for its memory, `phys_footprint`) and kills it beyond the limit.
//!
//! Arenas are POSIX shared memory, unlinked as soon as they are made (sized once, as macOS
//! sizes them: the worker cannot resize them).

use std::ffi::{CStr, CString, OsString, c_char};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// How often the footprint of a worker is looked at.
const WATCHED: Duration = Duration::from_millis(10);

pub use super::unix::{Arena, Mapping, view};

/// A worker, its footprint watched by a thread of its own as long as it is kept, and the
/// socket of its arenas.
pub struct Child(Arc<Watched>, OwnedFd);

struct Watched {
    child: Mutex<std::process::Child>,
    pid: libc::pid_t,
    /// The footprint the worker is killed beyond, in bytes.
    limit: AtomicU64,
}

impl Child {
    pub fn limit_memory(&self, bytes: u64) -> io::Result<()> {
        self.0.limit.store(bytes, Ordering::Relaxed);
        Ok(())
    }

    pub fn share(&mut self, arena: &Arena) -> io::Result<u64> {
        super::unix::share(&self.1, arena)
    }

    pub fn exit_code(&mut self) -> Option<i64> {
        // locked for each look only, so that the watcher can kill it meanwhile
        super::unix::exit_code(|| self.0.child.lock().unwrap().try_wait())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let mut c = self.0.child.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
    }
}

pub fn spawn(program: &Path, args: &[OsString]) -> io::Result<(Child, File, File)> {
    let (c, to, from, socket) = super::unix::spawn(program, args)?;
    let w = Arc::new(Watched { pid: c.id() as libc::pid_t, child: Mutex::new(c), limit: AtomicU64::new(u64::MAX) });
    let weak = Arc::downgrade(&w);
    // a worker that is not watched is not started
    let started = std::thread::Builder::new().name("aqr-sandbox-watch".into()).spawn(move || watch(weak));
    let child = Child(w, socket);
    started?;
    Ok((child, to, from))
}

/// An arena of `len` bytes.
pub fn arena(len: usize) -> io::Result<Arena> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // of a name of 31 bytes at most (PSHMNAMLEN), of no other's
    let name = format!("/aqr.{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let name = CString::new(name).expect("a name of no NUL");
    // SAFETY: of the name, terminated
    let fd =
        unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600 as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor just made, and the name it was made of
    let fd = unsafe {
        libc::shm_unlink(name.as_ptr());
        OwnedFd::from_raw_fd(fd)
    };
    // SAFETY: of the descriptor
    unsafe {
        libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        if libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Arena::of(fd, len)
}

/// The worker of `w` killed once its footprint is beyond its limit, until it is dropped.
fn watch(w: Weak<Watched>) {
    loop {
        std::thread::sleep(WATCHED);
        let Some(w) = w.upgrade() else { return };
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        // SAFETY: the buffer is of the flavor asked; the pid may be of a worker already waited
        // for (another process's, read and not acted on, as `kill` is of the child's handle)
        let r = unsafe { libc::proc_pid_rusage(w.pid, libc::RUSAGE_INFO_V2, &mut info as *mut _ as *mut _) };
        if r == 0 && info.ri_phys_footprint > w.limit.load(Ordering::Relaxed) {
            // a child waited for is not signalled again
            let _ = w.child.lock().unwrap().kill();
            return;
        }
    }
}

/// Everything denied that the system checks. The process's own memory, threads, signals and
/// Mach ports, and its descriptors already open, are not checked.
const PROFILE: &CStr = c"(version 1) (deny default)";

unsafe extern "C" {
    fn sandbox_init_with_parameters(
        profile: *const c_char,
        flags: u64,
        parameters: *const *const c_char,
        errorbuf: *mut *mut c_char,
    ) -> libc::c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

pub fn lockdown() -> io::Result<()> {
    close_from(super::unix::SOCKET + 1)?;
    let parameters = [std::ptr::null()];
    let mut error = std::ptr::null_mut();
    // SAFETY: the profile and the parameters (none) are terminated, and the error is freed
    // as it is given
    unsafe {
        if sandbox_init_with_parameters(PROFILE.as_ptr(), 0, parameters.as_ptr(), &mut error) == 0 {
            return Ok(());
        }
        let e = if error.is_null() { "unknown".into() } else { CStr::from_ptr(error).to_string_lossy().into_owned() };
        sandbox_free_error(error);
        Err(io::Error::other(format!("the sandbox: {e}")))
    }
}

/// The descriptors of this process from `low` closed, as the system lists them.
fn close_from(low: libc::c_int) -> io::Result<()> {
    let size = std::mem::size_of::<libc::proc_fdinfo>();
    let mut fds: Vec<libc::proc_fdinfo> = Vec::new();
    loop {
        // SAFETY: of no buffer, the call gives the size it would need
        let need = unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if need <= 0 {
            return Err(io::Error::last_os_error());
        }
        // room for some opened meanwhile, so that a list that fills the buffer is cut short
        let room = need as usize / size + 16;
        fds.clear();
        fds.reserve_exact(room);
        // SAFETY: the buffer is of `room` entries
        let got = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                (room * size) as libc::c_int,
            )
        };
        if got <= 0 {
            return Err(io::Error::last_os_error());
        }
        let n = got as usize / size;
        if n < room {
            // SAFETY: the system wrote `n` entries
            unsafe { fds.set_len(n) };
            break;
        }
    }
    for fd in fds.iter().map(|f| f.proc_fd).filter(|&fd| fd >= low) {
        // SAFETY: the descriptors are of nothing the worker uses (any it did not make are of
        // the decoder's process or the system's libraries, never to be reached locked down)
        unsafe { libc::close(fd) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker beyond its limit is killed, and one within it is not.
    #[test]
    fn memory_limited() {
        // 256 MB written, then held
        let args = ["-e", "$x = 'a' x 256_000_000; sleep 5"].map(OsString::from);
        let (mut within, ..) = spawn(Path::new("/usr/bin/perl"), &args).unwrap();
        let (mut beyond, ..) = spawn(Path::new("/usr/bin/perl"), &args).unwrap();
        within.limit_memory(1 << 30).unwrap();
        beyond.limit_memory(64 << 20).unwrap();
        assert_eq!(beyond.exit_code(), Some(-libc::SIGKILL as i64));
        assert_eq!(within.exit_code(), None);
    }
}
