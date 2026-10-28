//! What Unix systems start and wait for alike: a worker of no environment, in the root, of its
//! standard input and output piped, its error discarded and a socket at [`SOCKET`], which the
//! descriptors of arenas are sent over; and the arenas mapped.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

/// The worker's descriptor of the socket that arenas come over.
pub const SOCKET: RawFd = 3;

/// A worker started, its input, output and the decoder's end of its socket.
pub fn spawn(program: &Path, args: &[OsString]) -> io::Result<(std::process::Child, File, File, OwnedFd)> {
    let mut fds = [0; 2];
    // not inherited but as `SOCKET` below (macOS, of no `SOCK_CLOEXEC`, as soon as it can)
    #[cfg(target_os = "linux")]
    let ty = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let ty = libc::SOCK_STREAM;
    // SAFETY: of the array of two descriptors
    if unsafe { libc::socketpair(libc::AF_UNIX, ty, 0, fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptors just made
    let (mine, theirs) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    #[cfg(not(target_os = "linux"))]
    for fd in [&mine, &theirs] {
        // SAFETY: of the descriptor
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let theirs_fd = theirs.as_raw_fd();
    let mut cmd = Command::new(program);
    cmd.args(args).env_clear().current_dir("/").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // SAFETY: only calls safe after a fork
    unsafe {
        cmd.pre_exec(move || {
            let r =
                if theirs_fd == SOCKET { libc::fcntl(SOCKET, libc::F_SETFD, 0) } else { libc::dup2(theirs_fd, SOCKET) };
            if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        })
    };
    let mut c = cmd.spawn()?;
    drop(theirs);
    let to = File::from(OwnedFd::from(c.stdin.take().expect("the input")));
    let from = File::from(OwnedFd::from(c.stdout.take().expect("the output")));
    Ok((c, to, from, mine))
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

/// Memory mapped, shared, readable and writable.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is of no thread's
unsafe impl Send for Mapping {}

impl Mapping {
    /// `len` bytes of `fd` mapped.
    pub fn new(fd: &OwnedFd, len: usize) -> io::Result<Mapping> {
        // SAFETY: a new mapping, of nothing of this process's
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len.max(1),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping { ptr: ptr.cast(), len })
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping made
        unsafe { libc::munmap(self.ptr.cast(), self.len.max(1)) };
    }
}

/// Shared memory made by the decoder: its descriptor and its mapping.
pub struct Arena {
    fd: OwnedFd,
    pub map: Mapping,
}

impl Arena {
    /// `fd` of `len` bytes (sized already) mapped.
    pub fn of(fd: OwnedFd, len: usize) -> io::Result<Arena> {
        let map = Mapping::new(&fd, len)?;
        Ok(Arena { fd, map })
    }
}

/// A message of one byte and room for a descriptor, of `iov` and `control` (of
/// [`control_len`] bytes).
fn message(iov: &mut libc::iovec, control: &mut [u8]) -> libc::msghdr {
    // SAFETY: plain data
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;
    msg
}

fn control_len() -> usize {
    // SAFETY: a computation
    unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) as usize }
}

/// `arena` sent over `socket`, for the worker's [`view`].
pub fn share(socket: &OwnedFd, arena: &Arena) -> io::Result<u64> {
    let mut byte = [0u8];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut control = vec![0u8; control_len()];
    let msg = message(&mut iov, &mut control);
    // SAFETY: the control buffer is of a header and a descriptor, which the message points to
    let r = unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), arena.fd.as_raw_fd());
        libc::sendmsg(socket.as_raw_fd(), &msg, 0)
    };
    if r != 1 {
        return Err(io::Error::last_os_error());
    }
    Ok(0)
}

/// The arena the decoder sent last ([`share`]), `len` bytes of it mapped.
pub fn view(_: u64, len: usize) -> io::Result<Mapping> {
    let mut byte = [0u8];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut control = vec![0u8; control_len()];
    let mut msg = message(&mut iov, &mut control);
    // SAFETY: the message's buffers above
    let r = unsafe { libc::recvmsg(SOCKET, &mut msg, 0) };
    if r != 1 {
        return Err(if r < 0 { io::Error::last_os_error() } else { io::Error::other("no arena sent") });
    }
    // SAFETY: the control buffer as the system filled it
    let fd = unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() || (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(io::Error::other("no arena sent"));
        }
        OwnedFd::from_raw_fd(std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>()))
    };
    Mapping::new(&fd, len)
}
