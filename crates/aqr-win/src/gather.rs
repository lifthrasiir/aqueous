//! The paths chosen in Explorer gathered into one process: Explorer runs a process for each,
//! at about the same time.
//!
//! The processes of a verb in a folder (of a user) share the name of a pipe. The first to make
//! it gathers: it takes the paths the others write to it, until none has come for a while, and
//! works on them all. The others write theirs (UTF-16, ended by a NUL), wait for a byte back
//! that tells it was taken, and are done. The gatherer makes the next instance of the pipe
//! before it lets the one taken go, so that the name is never free while it gathers; once it
//! closes the last, a process coming late gathers anew.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WIN32_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

use crate::dialog::wide;

/// How long the gatherer waits for another path, and for a path being written.
const QUIET: u32 = 500;
const WRITING: u32 = 2000;

/// `path` of `verb`, with those of the other processes of the verb in its folder: `None` where
/// another process gathers it.
pub fn gather(verb: &str, path: PathBuf) -> Option<Vec<PathBuf>> {
    let name = wide(pipe(verb, &path));
    let mut bytes: Vec<u8> = path.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect();
    bytes.extend([0, 0]);
    for _ in 0..100 {
        if let Some(pipe) = make(&name, true) {
            return Some(serve(&name, pipe, path));
        }
        // SAFETY: a name of NULs
        let h = unsafe {
            let access = GENERIC_READ | GENERIC_WRITE;
            CreateFileW(name.as_ptr(), access, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut())
        };
        if h != INVALID_HANDLE_VALUE {
            let (mut n, mut ack) = (0, 0u8);
            // SAFETY: an open pipe, the bytes to write and room for the byte back
            let taken = unsafe {
                let w = WriteFile(h, bytes.as_ptr(), bytes.len() as u32, &mut n, std::ptr::null_mut());
                let taken = w != 0 && ReadFile(h, &mut ack, 1, &mut n, std::ptr::null_mut()) != 0 && n == 1;
                CloseHandle(h);
                taken
            };
            if taken {
                return None;
            }
            // (the gatherer gone meanwhile: one more round)
            // SAFETY: of this thread
        } else if unsafe { GetLastError() } == ERROR_PIPE_BUSY {
            // SAFETY: a name of NULs
            unsafe { WaitNamedPipeW(name.as_ptr(), 200) };
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // (alone, where nothing else works)
    Some(vec![path])
}

/// The pipe's name for `verb` and the folder of `path`, of this user.
fn pipe(verb: &str, path: &std::path::Path) -> String {
    let dir = path.parent().unwrap_or(path).as_os_str().to_string_lossy().to_lowercase();
    // FNV-1a
    let hash = dir.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let user = std::env::var("USERNAME").unwrap_or_default().replace('\\', "_");
    format!(r"\\.\pipe\aqr-win.{user}.{verb}.{hash:016x}")
}

/// An instance of the pipe `name`, the first of the name where `first`.
fn make(name: &[u16], first: bool) -> Option<HANDLE> {
    let mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let kind = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: a name of NULs, and the default security (others than this user only reading)
    let h = unsafe {
        CreateNamedPipeW(name.as_ptr(), mode, kind, PIPE_UNLIMITED_INSTANCES, 4096, 4096, 0, std::ptr::null())
    };
    (h != INVALID_HANDLE_VALUE).then_some(h)
}

/// Overlapped work `start` on `h` with `event`, waited for `ms` at most (cancelled then): the
/// bytes it moved, or what failed.
fn overlapped(
    h: HANDLE,
    event: HANDLE,
    ms: u32,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> Result<u32, WIN32_ERROR> {
    // SAFETY: `o` outlives the work, which is over (done, or cancelled and waited for) before
    // this returns; the work waited for only where it was started
    unsafe {
        let mut o: OVERLAPPED = std::mem::zeroed();
        o.hEvent = event;
        ResetEvent(event);
        if start(&mut o) == 0 {
            match GetLastError() {
                ERROR_IO_PENDING => {}
                e => return Err(e),
            }
        }
        if WaitForSingleObject(event, ms) != WAIT_OBJECT_0 {
            CancelIoEx(h, &o);
        }
        let mut n = 0;
        match GetOverlappedResult(h, &o, &mut n, 1) {
            0 => Err(GetLastError()),
            _ => Ok(n),
        }
    }
}

/// Gathers the paths written to instance `pipe` of pipe `name` (and the instances after it)
/// after `first`, until none is for [`QUIET`].
fn serve(name: &[u16], mut pipe: HANDLE, first: PathBuf) -> Vec<PathBuf> {
    let mut paths = vec![first];
    // SAFETY: an event of no name, reset by hand
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    loop {
        // SAFETY: an instance of the pipe opened for overlapped work
        let connected = match overlapped(pipe, event, QUIET, |o| unsafe { ConnectNamedPipe(pipe, o) }) {
            Ok(_) | Err(ERROR_PIPE_CONNECTED) => true,
            // (none for a while; or one gone already, which tries again)
            Err(_) => false,
        };
        if !connected {
            break;
        }
        let Some(next) = make(name, false) else { break };
        if let Some(p) = take(pipe, event) {
            paths.push(p);
        }
        // SAFETY: the instance taken, of this function's
        unsafe { CloseHandle(pipe) };
        pipe = next;
    }
    // SAFETY: of this function's
    unsafe {
        CloseHandle(pipe);
        CloseHandle(event);
    }
    paths
}

/// The path written to `pipe`, connected, to its NUL; the writer told it was taken.
fn take(pipe: HANDLE, event: HANDLE) -> Option<PathBuf> {
    let mut w: Vec<u16> = Vec::new();
    let mut odd = None;
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: room for the bytes, which outlives the work
        let n = overlapped(pipe, event, WRITING, |o| unsafe {
            ReadFile(pipe, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), o)
        })
        .ok()
        .filter(|&n| n > 0)?;
        for &b in &buf[..n as usize] {
            match odd.take() {
                None => odd = Some(b),
                Some(lo) => w.push(u16::from_le_bytes([lo, b])),
            }
        }
        if w.last() == Some(&0) {
            break;
        }
    }
    w.pop();
    let ack = [1u8];
    // SAFETY: the byte to write, which outlives the work
    overlapped(pipe, event, WRITING, |o| unsafe { WriteFile(pipe, ack.as_ptr(), 1, std::ptr::null_mut(), o) }).ok()?;
    (!w.is_empty()).then(|| OsString::from_wide(&w).into())
}
