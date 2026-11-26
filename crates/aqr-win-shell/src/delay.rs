//! The hook of the libraries delay-loaded (`build.rs`): the `windows` crate links functions of
//! Windows 8 on's libraries, which Windows 7 does not have.
//!
//! - `combase.dll` (`CoTaskMemAlloc`, `CoTaskMemFree`, `CoCreateFreeThreadedMarshaler`, of
//!   windows-core): where it is not there, `ole32.dll`, which has them.
//! - `api-ms-win-core-winrt-error-l1-1-0.dll` (`RoOriginateErrorW`, which windows-result calls
//!   as an error of a message is made, for a debugger to tell): where it is not there, a function
//!   that does nothing.

use std::ffi::c_void;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::core::{BOOL, HRESULT, PCWSTR, w};

/// `DelayLoadProc` (delayimp.h).
#[repr(C)]
pub struct Proc {
    by_name: BOOL,
    /// The name (of `by_name`), else the ordinal.
    name: *const u8,
}

/// `DelayLoadInfo` (delayimp.h).
#[repr(C)]
pub struct Info {
    size: u32,
    descriptor: *const c_void,
    address: *mut *const c_void,
    dll: *const u8,
    proc: Proc,
    module: HMODULE,
    function: *const c_void,
    error: u32,
}

const PRE_LOAD_LIBRARY: u32 = 1;
const FAIL_LOAD_LIBRARY: u32 = 3;
const FAIL_GET_PROC: u32 = 4;

pub type Hook = unsafe extern "system" fn(u32, *const Info) -> *const c_void;

/// Of the delay-load helper's notifications: before a library is loaded, and where it failed.
#[unsafe(no_mangle)]
#[used]
pub static __pfnDliNotifyHook2: Hook = hook;
#[unsafe(no_mangle)]
#[used]
pub static __pfnDliFailureHook2: Hook = hook;

/// `RoOriginateErrorW` where there is none: no error told a debugger.
unsafe extern "system" fn originate_error(_: HRESULT, _: u32, _: PCWSTR) -> BOOL {
    BOOL(0)
}

unsafe fn name(p: *const u8) -> String {
    // SAFETY: a string of the helper's, ended by 0
    unsafe { std::ffi::CStr::from_ptr(p as *const std::ffi::c_char) }.to_string_lossy().to_ascii_lowercase()
}

unsafe extern "system" fn hook(notify: u32, info: *const Info) -> *const c_void {
    // SAFETY: the helper's structure, valid for the call
    unsafe {
        let Some(info) = info.as_ref() else { return std::ptr::null() };
        let dll = name(info.dll);
        match notify {
            // (combase where there is one, else ole32)
            PRE_LOAD_LIBRARY if dll == "combase.dll" => match LoadLibraryW(w!("combase.dll")) {
                Ok(m) => m.0,
                Err(_) => LoadLibraryW(w!("ole32.dll")).map_or(std::ptr::null_mut(), |m| m.0),
            },
            // (the library of errors of WinRT not there: kernel32 in its place, whose function of
            // the name is not there either, given below)
            FAIL_LOAD_LIBRARY if dll.starts_with("api-ms-win-core-winrt-error") => {
                GetModuleHandleW(w!("kernel32.dll")).map_or(std::ptr::null_mut(), |m| m.0)
            }
            FAIL_GET_PROC if info.proc.by_name.as_bool() && name(info.proc.name) == "rooriginateerrorw" => {
                originate_error as *const c_void
            }
            _ => std::ptr::null(),
        }
    }
}
