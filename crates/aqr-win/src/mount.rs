//! `mount ARCHIVE` and `eject DRIVE`: the shell extension's "Mount as a drive" and "Eject"
//! (aqr-win-shell's `mount`), over aqr-mount (crates/aqr-mount-win), the program beside this one
//! where it is installed (`aqr-mount --install`; released apart, of WinFsp's GPL).
//!
//! - `mount`: where the archive is mounted already (a drive of the file system `aqr` whose
//!   control tells it of the same path), that drive is opened; else `aqr-mount --json ARCHIVE`
//!   is started, of no window and out of this program's job, and once it tells the drive it
//!   mounted (the last free letter), the drive is opened in Explorer. While it opens the archive
//!   (a while of a big one), a dialog tells so, its Cancel ending it. It goes on after this
//!   program ends, until the drive is ejected.
//! - `eject`: the drive's control asked to eject it; where files of it are open, the user is
//!   asked whether to all the same (then forced).
//!
//! The controls' protocol is aqr-mount's (`src/control.rs`): a line of JSON asked of
//! `\\.\pipe\Aqueous.Mount.Q`, a line answered.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use aqr_l10n::win;
use serde_json::{Value, json};

use crate::dialog::{self, Icon};

/// `CREATE_NO_WINDOW`: aqr-mount is a console program.
const NO_WINDOW: u32 = 0x0800_0000;
/// `CREATE_BREAKAWAY_FROM_JOB`: it outlives this program, whatever job this is in.
const BREAKAWAY: u32 = 0x0100_0000;
/// `ERROR_PIPE_BUSY`: every instance at work, another made in a moment.
const PIPE_BUSY: i32 = 231;

/// aqr-mount, beside this program.
fn program() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|e| e.with_file_name("aqr-mount.exe"))
}

/// The control of drive `letter` (`Q`).
fn pipe(letter: char) -> String {
    format!(r"\\.\pipe\Aqueous.Mount.{}", letter.to_ascii_uppercase())
}

/// Request `q` of the control `pipe`: its answer (tried again a while where it is busy).
fn ask(pipe: &str, q: &Value) -> std::io::Result<Value> {
    let mut tries = 0;
    let mut p = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(pipe) {
            Ok(p) => break p,
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    };
    writeln!(p, "{q}")?;
    let mut line = String::new();
    BufReader::new(p).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// The file system of the volume at `root` (`Q:\`), where it tells one.
fn file_system(root: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let root = crate::dialog::wide(root);
    let mut name = [0u16; 64];
    // SAFETY: a root ending in NUL, and a buffer of its length for the name
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    let n = name.iter().position(|&c| c == 0).unwrap_or(0);
    (ok != 0).then(|| String::from_utf16_lossy(&name[..n]))
}

/// The drives of aqr-mount's, each with its control's status.
fn mounts() -> Vec<(char, Value)> {
    // SAFETY: of no arguments
    let used = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8)
        .filter(|i| used & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
        .filter(|&l| file_system(&format!(r"{l}:\")).as_deref() == Some("aqr"))
        .filter_map(|l| ask(&pipe(l), &json!({"status": true})).ok().map(|s| (l, s)))
        .collect()
}

/// aqr-mount started, as [`start`] starts it: killed or waited for.
struct Started(windows_sys::Win32::Foundation::HANDLE);

impl Started {
    fn kill(&self) {
        use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        // SAFETY: the process's handle, of this struct's
        unsafe {
            TerminateProcess(self.0, 1);
            WaitForSingleObject(self.0, 5000);
        }
    }

    /// Its exit code, once it has ended.
    fn wait(&self) -> Option<u32> {
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
        let mut code = 0;
        // SAFETY: the process's handle, of this struct's
        let ended = unsafe { WaitForSingleObject(self.0, INFINITE) == 0 && GetExitCodeProcess(self.0, &mut code) != 0 };
        ended.then_some(code)
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        // SAFETY: the process's handle, of this struct's
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// `exe` started with `args` and `flags` (`CREATE_*`), its standard output a pipe read here,
/// its input and error `NUL`, and no other handle of this program's given it
/// (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`): std's `Command` gives a child every handle that may be
/// inherited, and aqr-mount outlives this program, holding what it was given (a pipe or a file
/// of what started this program: a test's output redirected, which its waiting then never saw
/// end).
fn start(exe: &Path, args: &[&OsStr], flags: u32) -> std::io::Result<(Started, std::fs::File)> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        SetHandleInformation,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, UpdateProcThreadAttribute,
    };
    let last = std::io::Error::last_os_error;
    // (`"exe" "arg" ...`: paths hold no quotes)
    let mut line: Vec<u16> = Vec::new();
    for a in std::iter::once(exe.as_os_str()).chain(args.iter().copied()) {
        if !line.is_empty() {
            line.push(u16::from(b' '));
        }
        line.push(u16::from(b'"'));
        line.extend(a.encode_wide());
        line.push(u16::from(b'"'));
    }
    line.push(0);
    let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    let nul_w: Vec<u16> = "NUL".encode_utf16().chain([0]).collect();
    let inherited = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: handles made here and closed here (but the pipe's end read, given to the file, and
    // the process's, to `Started`); the attribute list in a buffer of the size it asks for,
    // alive through the call
    unsafe {
        let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        if CreatePipe(&mut read, &mut write, &inherited, 0) == 0 {
            return Err(last());
        }
        SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0);
        let read = std::fs::File::from_raw_handle(read as _);
        let nul = CreateFileW(
            nul_w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &inherited,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if nul == INVALID_HANDLE_VALUE {
            let e = last();
            CloseHandle(write);
            return Err(e);
        }
        let mut size = 0;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut buffer = vec![0u64; size.div_ceil(8)];
        let list = buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let handles = [write, nul];
        let made = InitializeProcThreadAttributeList(list, 1, 0, &mut size) != 0
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of_val(&handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) != 0;
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = nul;
        si.StartupInfo.hStdOutput = write;
        si.StartupInfo.hStdError = nul;
        si.lpAttributeList = list;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = made
            && CreateProcessW(
                exe_w.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                flags | EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                std::ptr::null(),
                &si.StartupInfo,
                &mut pi,
            ) != 0;
        let e = last();
        if made {
            DeleteProcThreadAttributeList(list);
        }
        CloseHandle(write);
        CloseHandle(nul);
        if !ok {
            return Err(e);
        }
        CloseHandle(pi.hThread);
        Ok((Started(pi.hProcess), read))
    }
}

/// The drive opened in Explorer.
fn open(letter: char) {
    _ = Command::new("explorer.exe").arg(format!(r"{letter}:\")).spawn();
}

fn display_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

pub fn mount(archive: &Path) -> ExitCode {
    let archive = std::path::absolute(archive).unwrap_or(archive.to_owned());
    let name = display_name(&archive);
    let fail = |e: &str| {
        dialog::message(Icon::Error, &win::mount_failed(&name), e, None);
        ExitCode::FAILURE
    };
    let path = archive.to_string_lossy();
    if let Some((l, _)) =
        mounts().into_iter().find(|(_, s)| s["archive"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&path)))
    {
        open(l);
        return ExitCode::SUCCESS;
    }
    let Some(exe) = program().filter(|p| p.is_file()) else {
        let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.display().to_string()));
        return fail(&win::no_mount_program(dir.unwrap_or_default()));
    };
    let args = [OsStr::new("--json"), archive.as_os_str()];
    // (out of the job where it may be; in it where it may not)
    let started = start(&exe, &args, NO_WINDOW | BREAKAWAY).or_else(|_| start(&exe, &args, NO_WINDOW));
    let (child, out) = match started {
        Ok(c) => c,
        Err(e) => return fail(&format!("{}: {e}", exe.display())),
    };
    // (its line read on a thread, waited for under a dialog: a big archive takes a while to be
    // opened, and Cancel ends it)
    let told = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let told = told.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            _ = BufReader::new(out).read_line(&mut line);
            *told.lock().unwrap() = Some(line);
        });
    }
    let done = || told.lock().unwrap().is_some();
    if !dialog::waiting(&win::mounting(&name), &archive.display().to_string(), &done) {
        child.kill();
        return ExitCode::SUCCESS;
    }
    let line = told.lock().unwrap().take().unwrap_or_default();
    let told: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    match (told["mounted"].as_str(), told["error"].as_str()) {
        (Some(m), _) => {
            if let Some(l) = m.chars().next() {
                open(l);
            }
            ExitCode::SUCCESS
        }
        (None, Some(e)) => fail(e),
        // (ended before it told anything)
        (None, None) => {
            let status = child.wait().map(|c| format!("exit code {c}")).unwrap_or_default();
            fail(&format!("{}: {status}", exe.display()))
        }
    }
}

pub fn eject(drive: &str) -> ExitCode {
    let letter = drive.chars().next().filter(|c| c.is_ascii_alphabetic()).unwrap_or('?').to_ascii_uppercase();
    let shown = format!("{letter}:");
    let fail = |e: &str| {
        dialog::message(Icon::Error, &win::not_ejected(&shown), e, None);
        ExitCode::FAILURE
    };
    let control = pipe(letter);
    let status = match ask(&control, &json!({"status": true})) {
        Ok(s) => s,
        Err(_) => return fail(&win::not_a_mount(&shown)),
    };
    let mut force = false;
    loop {
        let answer = match ask(&control, &json!({"eject": true, "force": force})) {
            Ok(a) => a,
            Err(e) => return fail(&e.to_string()),
        };
        if answer["ejected"] == true {
            return ExitCode::SUCCESS;
        }
        let Some(n) = answer["open"].as_u64() else {
            return fail(answer["error"].as_str().unwrap_or(""));
        };
        let name = status["archive"].as_str().map(|a| display_name(Path::new(a))).unwrap_or_default();
        if !dialog::confirm(&win::eject_ask(&shown, &name), &win::eject_open(n)) {
            return ExitCode::SUCCESS;
        }
        force = true;
    }
}
