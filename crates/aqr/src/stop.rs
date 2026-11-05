//! The output being written, and what is done where the process is stopped before it is done:
//! by Ctrl-C, `SIGTERM` or `SIGHUP` (Ctrl-C, Ctrl-Break or the console closed on Windows), or by
//! a frontend ([`now`], [`request`]).
//!
//! An output made whole or not at all ([`create`]) is removed and the process stopped as it
//! would have been. An archive committed as it is written ([`graceful`]: spec, "Appending to an
//! archive") is stopped in two steps: the first signal only requests that it be ended as it is
//! ([`requested`]), its writer committing what it wrote; a second one stops the process at once,
//! leaving the archive of its last commit (the output removed where nothing was committed yet,
//! as it is still to be placed: [`placed`]).
//!
//! An extraction is stopped in two steps as well ([`extracting`]): the files written whole kept,
//! the others removed ([`crate::extract::Extractor::stop`]).
//!
//! A frontend cancelling ([`now`]) undoes what was done to the output instead: an output made
//! is removed, an archive appended to cut back to where it ended before ([`undo_by`]).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Held as an output is made and set to be removed ([`create`]), and by [`now`] as the process
/// stops (never released then): so that it never stops between the two, the output left.
static GATE: Mutex<()> = Mutex::new(());

/// What [`now`] undoes of the output being written.
#[derive(Clone)]
pub enum Undo {
    /// Removed, as it was made.
    Remove(PathBuf),
    /// Cut back to its length before (an archive appended to).
    Truncate(PathBuf, u64),
}

static UNDO: Mutex<Option<Undo>> = Mutex::new(None);

/// Whether the output being written is ended in two steps ([`graceful`]), and whether its end
/// was requested.
static GRACEFUL: AtomicBool = AtomicBool::new(false);
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Makes output `path` by `make` (creating it) and has it removed where the process stops,
/// until the guard is dropped; where it is stopping already, waits for it to.
pub fn create<T>(path: &Path, make: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<(T, Guard)> {
    let _gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    let t = make()?;
    imp::set(Some(path));
    *UNDO.lock().unwrap_or_else(|e| e.into_inner()) = Some(Undo::Remove(path.to_owned()));
    Ok((t, Guard(())))
}

/// Has the output being written ended in two steps (module docs) until the guard of
/// [`create`] (or [`undo_by`]) is dropped: the flag a first signal sets.
pub fn graceful() -> &'static AtomicBool {
    imp::install();
    GRACEFUL.store(true, Ordering::SeqCst);
    &REQUESTED
}

/// Has an extraction ended in two steps (module docs: the first signal requesting its end, the
/// files written whole kept) until the guard is dropped: the flag a first signal sets.
pub fn extracting() -> (Guard, &'static AtomicBool) {
    imp::install();
    GRACEFUL.store(true, Ordering::SeqCst);
    (Guard(()), &REQUESTED)
}

/// Requests that the output being written be ended as it is (a frontend's `finish`): where it
/// is not ended in two steps, the process is stopped now ([`now`]) with exit code `code`.
pub fn request(code: i32) {
    if GRACEFUL.load(Ordering::SeqCst) {
        REQUESTED.store(true, Ordering::SeqCst);
    } else {
        now(code);
    }
}

/// Takes the output as placed (where it is to stay, a commit in it): no signal removes it any
/// more, and [`now`] undoes what was done by `undo` instead.
pub fn placed(undo: Undo) {
    let _gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    imp::set(None);
    *UNDO.lock().unwrap_or_else(|e| e.into_inner()) = Some(undo);
}

/// Has [`now`] undo what is done to an output there before (an archive appended to) by `undo`,
/// until the guard is dropped; no signal removes it.
pub fn undo_by(undo: Undo) -> Guard {
    placed(undo);
    Guard(())
}

pub struct Guard(());

/// Stops the process now, with exit code `code`, what was done to the output being written
/// undone ([`Undo`]; once one being made is: [`create`]; `exit` runs its handlers while the other
/// threads go on).
pub fn now(code: i32) -> ! {
    let gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    imp::set(None);
    match UNDO.lock().unwrap_or_else(|e| e.into_inner()).take() {
        // (open as it is, but shared for deletion on Windows: gone once the process is)
        Some(Undo::Remove(p)) => _ = std::fs::remove_file(p),
        Some(Undo::Truncate(p, len)) => {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(p) {
                _ = f.set_len(len);
                _ = f.sync_all();
            }
        }
        None => {}
    }
    std::mem::forget(gate);
    std::process::exit(code)
}

/// Ends the process now with exit code `code`, the output left as it is (ended already).
pub fn exit(code: i32) -> ! {
    let gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    imp::set(None);
    UNDO.lock().unwrap_or_else(|e| e.into_inner()).take();
    std::mem::forget(gate);
    std::process::exit(code)
}

impl Drop for Guard {
    fn drop(&mut self) {
        imp::set(None);
        UNDO.lock().unwrap_or_else(|e| e.into_inner()).take();
        GRACEFUL.store(false, Ordering::SeqCst);
    }
}

/// What the first signal tells, where the output is ended in two steps.
const STOPPING: &[u8] = b"\naqr: stopping: ending what is written as it is (again to stop at once)\n";

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::path::Path;
    use std::sync::Once;
    use std::sync::atomic::{AtomicPtr, Ordering};

    use super::{GRACEFUL, REQUESTED, STOPPING};

    /// The path to remove, as the handler takes it: never freed, as the handler may be reading
    /// it while it is replaced (a path for each output, a few bytes).
    static PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    pub fn set(path: Option<&Path>) {
        install();
        use std::os::unix::ffi::OsStrExt;
        let p = path
            .and_then(|p| CString::new(p.as_os_str().as_bytes()).ok())
            .map_or(std::ptr::null_mut(), CString::into_raw);
        PATH.store(p, Ordering::SeqCst);
    }

    pub fn install() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            for s in SIGNALS {
                // SAFETY: a handler doing only what is async-signal-safe, set once the
                // disposition is known not to be ignored (as `nohup` has SIGHUP)
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(s, std::ptr::null(), &mut old) != 0 || old.sa_sigaction == libc::SIG_IGN {
                        continue;
                    }
                    let mut a: libc::sigaction = std::mem::zeroed();
                    a.sa_sigaction = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    libc::sigemptyset(&mut a.sa_mask);
                    libc::sigaction(s, &a, std::ptr::null_mut());
                }
            }
        });
    }

    extern "C" fn handler(s: libc::c_int) {
        // (the first of an output ended in two steps: its end requested alone)
        if GRACEFUL.load(Ordering::SeqCst) && !REQUESTED.swap(true, Ordering::SeqCst) {
            // SAFETY: write is async-signal-safe
            unsafe { libc::write(2, STOPPING.as_ptr().cast(), STOPPING.len()) };
            return;
        }
        let p = PATH.load(Ordering::SeqCst);
        // SAFETY: a C string never freed; unlink, signal and raise are async-signal-safe
        unsafe {
            if !p.is_null() {
                libc::unlink(p);
            }
            // (the default restored: raised again, the signal stops the process, delivered
            // once the handler returns, the signal blocked until then)
            libc::signal(s, libc::SIG_DFL);
            libc::raise(s);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, Once};

    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
    use windows_sys::core::BOOL;

    use super::{GRACEFUL, REQUESTED, STOPPING};

    static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub fn set(path: Option<&Path>) {
        install();
        *PATH.lock().unwrap_or_else(|e| e.into_inner()) = path.map(Path::to_owned);
    }

    pub fn install() {
        static INSTALL: Once = Once::new();
        // SAFETY: a handler of the process's own, run on a thread of its own
        INSTALL.call_once(|| unsafe { _ = SetConsoleCtrlHandler(Some(handler), 1) });
    }

    unsafe extern "system" fn handler(event: u32) -> BOOL {
        if GRACEFUL.load(Ordering::SeqCst) && !REQUESTED.swap(true, Ordering::SeqCst) {
            use std::io::Write;
            _ = std::io::stderr().write_all(STOPPING);
            // Ctrl-C and Ctrl-Break handled; the console closed, the process is ended once this
            // returns (or 5 seconds after): the time given to end the archive, as it ends the
            // process itself
            if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
            return TRUE;
        }
        if let Some(p) = PATH.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // (open as it is, but shared for deletion: gone once the process is)
            _ = std::fs::remove_file(p);
        }
        // the next handler, the default one, ends the process
        FALSE
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn set(_: Option<&std::path::Path>) {}

    pub fn install() {}
}
