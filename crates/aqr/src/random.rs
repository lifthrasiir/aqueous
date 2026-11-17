//! std's random of the win7 target (`RtlGenRandom`, advapi32's `SystemFunction036`) taken as
//! it runs, not imported: the import is forwarded to cryptbase.dll, which is not a known DLL,
//! and a sandbox worker (aqr.exe itself, aqr-wasmrt's) cannot load such a library as it starts.
//! The loader of Windows 10 on maps a process's libraries on threads of its own, of the primary
//! token, which is the worker's locked-down one (it opens no file; only the main thread
//! impersonates the token that does): the worker ended 0xc0000135 at times (6 starts of 20).
//!
//! This defines the slot std's calls go through (`__imp_SystemFunction036`), so that the
//! linker takes none of advapi32.lib's; the function is looked up on the first call, by the
//! thread making it, which in a worker is its main thread (aqr-wasmrt's `worker_main` makes it
//! first). The delay-loading of advapi32 would do the same, but aqr.exe imports it twice
//! (std of the SDK's import library, windows-sys of raw-dylib, as `ADVAPI32.dll` and
//! `advapi32.dll`), and link.exe's delay-load stub of the two took one's thunk for the other's.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

type GenRandom = unsafe extern "system" fn(*mut c_void, u32) -> u8;

#[unsafe(no_mangle)]
#[used]
static __imp_SystemFunction036: GenRandom = gen_random;

static FUNCTION: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// `RtlGenRandom(buffer, len)`, looked up once (false where it is not found).
unsafe extern "system" fn gen_random(buffer: *mut c_void, len: u32) -> u8 {
    let mut f = FUNCTION.load(Ordering::Acquire);
    if f.is_null() {
        let name: Vec<u16> = "advapi32.dll\0".encode_utf16().collect();
        // SAFETY: of a NUL-terminated name; advapi32 is loaded already (aqr imports it), else
        // loaded from the system's directory
        unsafe {
            let mut m = GetModuleHandleW(name.as_ptr());
            if m.is_null() {
                m = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            }
            match GetProcAddress(m, c"SystemFunction036".as_ptr().cast()) {
                Some(p) => f = p as *mut c_void,
                None => return 0,
            }
        }
        FUNCTION.store(f, Ordering::Release);
    }
    // SAFETY: the function looked up, of RtlGenRandom's type; into `buffer`, of `len` bytes, as
    // std calls it
    unsafe { std::mem::transmute::<*mut c_void, GenRandom>(f)(buffer, len) }
}
