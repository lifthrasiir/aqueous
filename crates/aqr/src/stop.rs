//! The output being written, removed where the process is stopped before it is done: by
//! Ctrl-C, `SIGTERM` or `SIGHUP` (Ctrl-C, Ctrl-Break or the console closed on Windows). The
//! process is then stopped as it would have been. A frontend may stop it as well ([`now`]).

use std::path::Path;

/// Where the process stops, `path` is removed, until the guard is dropped.
pub fn removing(path: &Path) -> Guard {
    imp::set(Some(path));
    Guard(())
}

pub struct Guard(());

/// Stops the process now, with exit code `code`, the output being written removed.
pub fn now(code: i32) -> ! {
    imp::remove();
    std::process::exit(code)
}

impl Drop for Guard {
    fn drop(&mut self) {
        imp::set(None);
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::path::Path;
    use std::sync::Once;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The path to remove, as the handler takes it: never freed, as the handler may be reading
    /// it while it is replaced (a path for each output, a few bytes).
    static PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    pub fn set(path: Option<&Path>) {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(install);
        use std::os::unix::ffi::OsStrExt;
        let p = path
            .and_then(|p| CString::new(p.as_os_str().as_bytes()).ok())
            .map_or(std::ptr::null_mut(), CString::into_raw);
        PATH.store(p, Ordering::SeqCst);
    }

    fn install() {
        for s in SIGNALS {
            // SAFETY: a handler doing only what is async-signal-safe, set once the disposition
            // is known not to be ignored (as `nohup` has SIGHUP)
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(s, std::ptr::null(), &mut old) != 0 || old.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                let mut a: libc::sigaction = std::mem::zeroed();
                a.sa_sigaction = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
                // (the default restored as it is taken: raised again, the signal stops the process)
                a.sa_flags = libc::SA_RESETHAND;
                libc::sigemptyset(&mut a.sa_mask);
                libc::sigaction(s, &a, std::ptr::null_mut());
            }
        }
    }

    pub fn remove() {
        let p = PATH.swap(std::ptr::null_mut(), Ordering::SeqCst);
        // SAFETY: a C string never freed
        if !p.is_null() {
            unsafe { libc::unlink(p) };
        }
    }

    extern "C" fn handler(s: libc::c_int) {
        let p = PATH.load(Ordering::SeqCst);
        // SAFETY: a C string never freed; unlink and raise are async-signal-safe
        unsafe {
            if !p.is_null() {
                libc::unlink(p);
            }
            // (delivered once the handler returns, the signal blocked until then)
            libc::raise(s);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, Once};

    use windows_sys::Win32::Foundation::FALSE;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub fn set(path: Option<&Path>) {
        static INSTALL: Once = Once::new();
        // SAFETY: a handler of the process's own, run on a thread of its own
        INSTALL.call_once(|| unsafe { _ = SetConsoleCtrlHandler(Some(handler), 1) });
        *PATH.lock().unwrap_or_else(|e| e.into_inner()) = path.map(Path::to_owned);
    }

    pub fn remove() {
        if let Some(p) = PATH.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // (open as it is, but shared for deletion: gone once the process is)
            _ = std::fs::remove_file(p);
        }
    }

    unsafe extern "system" fn handler(_: u32) -> BOOL {
        remove();
        // the next handler, the default one, ends the process
        FALSE
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn set(_: Option<&std::path::Path>) {}

    pub fn remove() {}
}
