//! The control of a mount: a named pipe, `\\.\pipe\Aqueous.Mount.KEY` ([`pipe_name`]: KEY the
//! drive letter, `Q`, or of a directory `dir-` and a hash of its path), of the user who mounted
//! alone (its DACL), local clients alone. A client writes a request, a JSON object on a line, and
//! reads its answer, a line too:
//!
//! ```text
//! > {"status":true}
//! < {"archive":"C:\\a\\t.aqr","mount":"Q:","open":2}
//! > {"eject":true}
//! < {"open":2}                       (files open: not ejected)
//! > {"eject":true,"force":true}
//! < {"ejected":true}                 then unmounted, the program ending
//! ```
//!
//! `open` is how many files of the volume are open (directories not counted: Explorer keeps the
//! one it shows open). Each client is served by a thread of its own, so that one that hangs
//! holds none of the others back.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The pipe of the mount at `mount_point` (`Q:`, or a directory).
pub fn pipe_name(mount_point: &str) -> String {
    let b = mount_point.as_bytes();
    if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return format!(r"\\.\pipe\Aqueous.Mount.{}", (b[0] as char).to_ascii_uppercase());
    }
    // (FNV-1a of the path lowercased)
    let h = mount_point
        .to_lowercase()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, c| (h ^ c as u64).wrapping_mul(0x100000001b3));
    format!(r"\\.\pipe\Aqueous.Mount.dir-{h:016x}")
}

/// What the clients are told, and how many files are open.
pub struct Mount {
    pub archive: String,
    pub mount_point: String,
    pub open: Arc<AtomicUsize>,
}

/// The security of the pipe: the user's alone (`D:P(A;;GA;;;SID)`), as an `SECURITY_ATTRIBUTES`'s
/// descriptor (of `LocalAlloc`, kept for the program's life).
fn users_alone() -> std::io::Result<PSECURITY_DESCRIPTOR> {
    // SAFETY: the process's token, queried into a buffer of the size it tells, then its SID as a
    // string (freed), then the descriptor of it
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), size, &mut size);
        CloseHandle(token);
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let len = (0..).take_while(|&i| *sid.add(i) != 0).count();
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid_text})").encode_utf16().chain([0]).collect();
        let mut sd = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sd)
    }
}

/// An instance of the pipe made (the first of its name where `first`: another program holding
/// the name fails it).
fn instance(name: &[u16], sd: PSECURITY_DESCRIPTOR, first: bool) -> std::io::Result<HANDLE> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let open = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    // SAFETY: a name ending in NUL, attributes for the call
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            &attributes,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(h)
}

/// The pipe of `mount` made (before the mount is told of: a client may come at once), then
/// served on a thread of its own; `eject` is sent on once a client has the volume ejected.
pub fn start(mount: Mount, eject: Sender<()>) -> std::io::Result<()> {
    let name: Vec<u16> = OsStr::new(&pipe_name(&mount.mount_point)).encode_wide().chain([0]).collect();
    let sd = users_alone()?;
    let first = instance(&name, sd, true)?;
    let mount = Arc::new(mount);
    // (as numbers: the thread takes them)
    let (sd, first) = (sd as usize, first as usize);
    std::thread::Builder::new().name("control".into()).spawn(move || {
        let mut h = first as HANDLE;
        loop {
            // SAFETY: an instance of the pipe, waited on for a client
            let connected = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) } != 0
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            // SAFETY: the instance's handle, owned by the file from now on
            let pipe = unsafe { File::from_raw_handle(h as _) };
            // (the next instance at once, so that a client coming meanwhile finds one)
            h = match instance(&name, sd as PSECURITY_DESCRIPTOR, false) {
                Ok(h) => h,
                Err(_) => return,
            };
            if !connected {
                continue;
            }
            let (mount, eject) = (mount.clone(), eject.clone());
            _ = std::thread::Builder::new().name("client".into()).spawn(move || client(pipe, &mount, &eject));
        }
    })?;
    Ok(())
}

/// A client's requests answered, until it goes.
fn client(pipe: File, mount: &Mount, eject: &Sender<()>) {
    let Ok(mut out) = pipe.try_clone() else { return };
    for line in BufReader::new(pipe).lines() {
        let Ok(line) = line else { return };
        let q: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let open = mount.open.load(Ordering::SeqCst);
        let (answer, ejected) = if q["status"] == true {
            (json!({"archive": mount.archive, "mount": mount.mount_point, "open": open}), false)
        } else if q["eject"] == true {
            match open > 0 && q["force"] != true {
                true => (json!({"open": open}), false),
                false => (json!({"ejected": true}), true),
            }
        } else {
            (json!({"error": "not a request"}), false)
        };
        if writeln!(out, "{answer}").and_then(|_| out.flush()).is_err() {
            return;
        }
        if ejected {
            _ = eject.send(());
            return;
        }
    }
}
