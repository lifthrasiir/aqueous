//! Workers on Windows, locked down as Chromium's renderers are:
//!
//! - its token restricted: every SID of the user's deny-only, of no privileges, the null SID
//!   restricting it (so that it opens nothing by name), of low integrity;
//! - in a job of no other process, no UI, killed with the job's handle (the decoder's), and of
//!   the memory the decoder limits it to;
//! - of the mitigations that keep it off win32k, fonts and images not of the system, and from
//!   starting processes.
//!
//! The worker starts impersonating a token of the user's access (restricted by all its SIDs, as
//! a restricted process impersonates only restricted tokens; of no privileges, of low
//! integrity), to load the libraries of the system, and gives it up ([`lockdown`]) once it
//! loaded them.
//! Wasmtime makes code executable as it compiles it, so dynamic code is not prohibited.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, CreateWellKnownSid, DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, GetLengthSid,
    GetTokenInformation, IsTokenRestricted, RevertToSelf, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
    SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_GROUPS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    TOKEN_USER, TokenGroups, TokenImpersonation, TokenIntegrityLevel, TokenUser, WELL_KNOWN_SID_TYPE, WinLowLabelSid,
    WinNullSid, WinRestrictedCodeSid,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_UILIMIT_ALL, SE_GROUP_INTEGRITY};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    SetThreadToken, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;

// The mitigations (winbase.h), always on.
const STRICT_HANDLE_CHECKS: u64 = 1 << 24;
const WIN32K_SYSTEM_CALL_DISABLE: u64 = 1 << 28;
const EXTENSION_POINT_DISABLE: u64 = 1 << 32;
const FONT_DISABLE: u64 = 1 << 48;
const IMAGE_LOAD_NO_REMOTE: u64 = 1 << 52;
const IMAGE_LOAD_NO_LOW_LABEL: u64 = 1 << 56;
const IMAGE_LOAD_PREFER_SYSTEM32: u64 = 1 << 60;
const MITIGATIONS: [u64; 2] = [
    STRICT_HANDLE_CHECKS
        | WIN32K_SYSTEM_CALL_DISABLE
        | EXTENSION_POINT_DISABLE
        | FONT_DISABLE
        | IMAGE_LOAD_NO_REMOTE
        | IMAGE_LOAD_NO_LOW_LABEL
        | IMAGE_LOAD_PREFER_SYSTEM32,
    0,
];

fn last_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// `r` true (nonzero), else the last error of `what`.
fn check(r: i32, what: &str) -> io::Result<()> {
    if r != 0 { Ok(()) } else { Err(last_error(what)) }
}

fn owned(h: HANDLE) -> OwnedHandle {
    // SAFETY: a handle just made, owned by no one else
    unsafe { OwnedHandle::from_raw_handle(h) }
}

/// A worker: its process, and the job it is in.
pub struct Child {
    process: OwnedHandle,
    job: OwnedHandle,
}

impl Child {
    pub fn limit_memory(&self, bytes: u64) -> io::Result<()> {
        set_limits(&self.job, Some(bytes))
    }

    pub fn exit_code(&mut self) -> Option<i64> {
        let p = self.process.as_raw_handle();
        let mut code = 0;
        // SAFETY: of the process's handle; it is waited for briefly, as it may be ending
        unsafe {
            if WaitForSingleObject(p, 1000) != WAIT_OBJECT_0 || GetExitCodeProcess(p, &mut code) == 0 {
                return None;
            }
        }
        Some(code as i64)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: of the job's handle (closing it kills the worker too)
        unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
    }
}

fn set_limits(job: &OwnedHandle, memory: Option<u64>) -> io::Result<()> {
    // SAFETY: plain data
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    info.BasicLimitInformation.ActiveProcessLimit = 1;
    if let Some(m) = memory {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        info.ProcessMemoryLimit = m.min(usize::MAX as u64) as usize;
    }
    // SAFETY: of the job's handle and the information above
    let r = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    check(r, "the limits of the job")
}

fn job() -> io::Result<OwnedHandle> {
    // SAFETY: a new job, of no name
    let h = unsafe { CreateJobObjectW(null(), null()) };
    if h.is_null() {
        return Err(last_error("a job"));
    }
    let job = owned(h);
    set_limits(&job, None)?;
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS { UIRestrictionsClass: JOB_OBJECT_UILIMIT_ALL };
    // SAFETY: of the job's handle and the restrictions above
    let r = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectBasicUIRestrictions,
            &ui as *const _ as *const c_void,
            size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
    };
    check(r, "the UI restrictions of the job")?;
    Ok(job)
}

/// A well-known SID.
fn well_known(t: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
    let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut len = sid.len() as u32;
    // SAFETY: into a buffer of `len` bytes
    let r = unsafe { CreateWellKnownSid(t, null_mut(), sid.as_mut_ptr() as *mut c_void, &mut len) };
    check(r, "a well-known SID")?;
    Ok(sid)
}

/// The information `class` of `token`, in a buffer aligned for what it holds.
fn token_info(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
    let mut len = 0;
    // SAFETY: asks the length only
    unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: into a buffer of at least `len` bytes
    let r = unsafe { GetTokenInformation(token, class, buf.as_mut_ptr() as *mut c_void, len, &mut len) };
    check(r, "the information of a token")?;
    Ok(buf)
}

fn set_low_integrity(token: &OwnedHandle) -> io::Result<()> {
    let low = well_known(WinLowLabelSid)?;
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES { Sid: low.as_ptr() as *mut c_void, Attributes: SE_GROUP_INTEGRITY as u32 },
    };
    // SAFETY: of the token's handle and the label above, of the SID it points to
    let r = unsafe {
        let len = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(low.as_ptr() as *mut c_void);
        SetTokenInformation(token.as_raw_handle(), TokenIntegrityLevel, &label as *const _ as *const c_void, len)
    };
    check(r, "the integrity of a token")
}

/// The worker's tokens: its own, locked down (primary), and the one it starts impersonating.
fn tokens() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut me = null_mut();
    // SAFETY: of this process
    check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut me) }, "the token of the decoder")?;
    let me = owned(me);
    let user = token_info(me.as_raw_handle(), TokenUser)?;
    let groups = token_info(me.as_raw_handle(), TokenGroups)?;
    // SAFETY: the buffers hold a TOKEN_USER and a TOKEN_GROUPS, of the groups they count
    let (user, groups) = unsafe {
        let user = &*(user.as_ptr() as *const TOKEN_USER);
        let g = &*(groups.as_ptr() as *const TOKEN_GROUPS);
        (user.User.Sid, std::slice::from_raw_parts(g.Groups.as_ptr(), g.GroupCount as usize))
    };
    // the SIDs of the user's, but its integrity
    let mut all = vec![SID_AND_ATTRIBUTES { Sid: user, Attributes: 0 }];
    all.extend(
        groups
            .iter()
            .filter(|g| g.Attributes & SE_GROUP_INTEGRITY as u32 == 0)
            .map(|g| SID_AND_ATTRIBUTES { Sid: g.Sid, Attributes: 0 }),
    );
    let null_sid = well_known(WinNullSid)?;
    let restrict = [SID_AND_ATTRIBUTES { Sid: null_sid.as_ptr() as *mut c_void, Attributes: 0 }];
    let mut locked = null_mut();
    // SAFETY: of the SIDs above, which outlive the call
    let r = unsafe {
        CreateRestrictedToken(
            me.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            all.len() as u32,
            all.as_ptr(),
            0,
            null(),
            restrict.len() as u32,
            restrict.as_ptr(),
            &mut locked,
        )
    };
    check(r, "the locked-down token")?;
    let locked = owned(locked);
    set_low_integrity(&locked)?;

    let restricted = well_known(WinRestrictedCodeSid)?;
    all.push(SID_AND_ATTRIBUTES { Sid: restricted.as_ptr() as *mut c_void, Attributes: 0 });
    let mut initial = null_mut();
    // SAFETY: of the SIDs above, which outlive the call
    let r = unsafe {
        CreateRestrictedToken(
            me.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            0,
            null(),
            0,
            null(),
            all.len() as u32,
            all.as_ptr(),
            &mut initial,
        )
    };
    check(r, "the initial token")?;
    let initial = owned(initial);
    let mut imp = null_mut();
    // SAFETY: of the token just made
    let r = unsafe {
        DuplicateTokenEx(
            initial.as_raw_handle(),
            TOKEN_ALL_ACCESS,
            null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut imp,
        )
    };
    check(r, "the initial token to impersonate")?;
    let imp = owned(imp);
    set_low_integrity(&imp)?;
    Ok((locked, imp))
}

/// `arg` appended to `cmd` as the C runtime parses it back.
fn quote(cmd: &mut Vec<u16>, arg: &OsStr) {
    if !cmd.is_empty() {
        cmd.push(b' ' as u16);
    }
    let arg: Vec<u16> = arg.encode_wide().collect();
    let plain = !arg.is_empty() && !arg.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'"' as u16);
    if plain {
        cmd.extend(arg);
        return;
    }
    cmd.push(b'"' as u16);
    let mut backslashes = 0;
    for c in arg {
        if c == b'\\' as u16 {
            backslashes += 1;
        } else {
            if c == b'"' as u16 {
                cmd.extend(std::iter::repeat_n(b'\\' as u16, backslashes + 1));
            }
            backslashes = 0;
        }
        cmd.push(c);
    }
    cmd.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
    cmd.push(b'"' as u16);
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// The attributes of the worker's creation: its handles, mitigations and children.
struct Attributes {
    list: Vec<u64>,
    handles: [HANDLE; 2],
    child_policy: u32,
}

impl Attributes {
    fn new(handles: [HANDLE; 2]) -> io::Result<Box<Attributes>> {
        let mut len = 0;
        // SAFETY: asks the length only
        unsafe { InitializeProcThreadAttributeList(null_mut(), 3, 0, &mut len) };
        let mut a = Box::new(Attributes {
            list: vec![0; len.div_ceil(8)],
            handles,
            child_policy: PROCESS_CREATION_CHILD_PROCESS_RESTRICTED,
        });
        let list = a.list();
        // SAFETY: into a buffer of `len` bytes; the values set are in the box, which outlives the
        // list (deleted as it is dropped)
        unsafe {
            check(InitializeProcThreadAttributeList(list, 3, 0, &mut len), "the attributes of the worker")?;
            let set = |attr: u32, v: *const c_void, n: usize| {
                check(UpdateProcThreadAttribute(list, 0, attr as usize, v, n, null_mut(), null()), "an attribute")
            };
            set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, a.handles.as_ptr() as *const c_void, size_of::<[HANDLE; 2]>())?;
            set(PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, MITIGATIONS.as_ptr() as *const c_void, size_of::<[u64; 2]>())?;
            set(PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, &a.child_policy as *const u32 as *const c_void, 4)?;
        }
        Ok(a)
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: of the list initialized
        unsafe { DeleteProcThreadAttributeList(self.list()) };
    }
}

pub fn spawn(program: &Path, args: &[OsString]) -> io::Result<(Child, File, File)> {
    let (locked, imp) = tokens()?;
    let job = job()?;
    let (child_in, to) = io::pipe()?;
    let (from, child_out) = io::pipe()?;
    let (child_in, child_out) = (OwnedHandle::from(child_in), OwnedHandle::from(child_out));
    for h in [&child_in, &child_out] {
        // SAFETY: of the pipe's handle, inherited only as the attributes list it
        check(unsafe { SetHandleInformation(h.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }, "a pipe")?;
    }
    let mut attrs = Attributes::new([child_in.as_raw_handle(), child_out.as_raw_handle()])?;

    let mut cmd = Vec::new();
    quote(&mut cmd, program.as_os_str());
    for a in args {
        quote(&mut cmd, a);
    }
    cmd.push(0);
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let mut env: Vec<u16> = OsStr::new("SystemRoot=").encode_wide().collect();
    env.extend(root.encode_wide());
    env.extend([0, 0]);

    // SAFETY: plain data
    let mut si: STARTUPINFOEXW = unsafe { zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = child_in.as_raw_handle();
    si.StartupInfo.hStdOutput = child_out.as_raw_handle();
    si.lpAttributeList = attrs.list();
    // SAFETY: plain data
    let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };
    let flags = CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT | DETACHED_PROCESS | CREATE_UNICODE_ENVIRONMENT;
    // SAFETY: of the strings and structures above, which outlive the call
    let r = unsafe {
        CreateProcessAsUserW(
            locked.as_raw_handle(),
            wide(program.as_os_str()).as_ptr(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            1,
            flags,
            env.as_ptr() as *const c_void,
            wide(&root).as_ptr(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    check(r, "the worker's process")?;
    drop(attrs);
    let (process, thread) = (owned(pi.hProcess), owned(pi.hThread));
    // SAFETY: of the process and thread just made, suspended
    let started = unsafe {
        check(AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()), "the worker in its job")
            .and_then(|_| check(SetThreadToken(&thread.as_raw_handle(), imp.as_raw_handle()), "the initial token"))
            .and_then(|_| match ResumeThread(thread.as_raw_handle()) {
                u32::MAX => Err(last_error("the worker resumed")),
                _ => Ok(()),
            })
    };
    if let Err(e) = started {
        // SAFETY: of the process just made
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        return Err(e);
    }
    Ok((Child { process, job }, File::from(OwnedHandle::from(to)), File::from(OwnedHandle::from(from))))
}

/// The impersonation [`spawn`] began given up, leaving the worker to its locked-down token.
pub fn lockdown() -> io::Result<()> {
    let mut token = null_mut();
    // SAFETY: of this process, and the token's handle
    unsafe {
        check(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token), "the token of the worker")?;
        let token = owned(token);
        if IsTokenRestricted(token.as_raw_handle()) == 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "a worker not started by a sandbox"));
        }
        check(RevertToSelf(), "the impersonation given up")?;
    }
    Ok(())
}
