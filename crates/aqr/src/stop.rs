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
//! What is to be done however the process stops ([`on_end`]: `--stats`'s files written) is done
//! before it does, by [`now`] and [`exit`], and by a signal stopping it (a second signal while
//! it is being done stops it at once).
//!
//! A frontend cancelling ([`now`]) undoes what was done to the output instead: an output made
//! is removed, an archive appended to cut back to where it ended before ([`undo_by`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

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

/// What [`on_end`] set, and whether it has been done (held while it is done).
static ON_END: OnceLock<fn()> = OnceLock::new();
static ENDED: Mutex<bool> = Mutex::new(false);

/// Has `f` done as the process stops however it does (but returning from `main` or a panic):
/// [`now`], [`exit`] or a signal. Set once.
pub fn on_end(f: fn()) {
    _ = ON_END.set(f);
    imp::install();
}

/// Does what [`on_end`] set, where not done already (waiting for it where it is being done).
fn end() {
    let Some(f) = ON_END.get() else { return };
    let mut ended = ENDED.lock().unwrap_or_else(|e| e.into_inner());
    if !std::mem::replace(&mut *ended, true) {
        f();
    }
}

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
    end();
    std::mem::forget(gate);
    std::process::exit(code)
}

/// Ends the process now with exit code `code`, the output left as it is (ended already).
pub fn exit(code: i32) -> ! {
    let gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    imp::set(None);
    UNDO.lock().unwrap_or_else(|e| e.into_inner()).take();
    end();
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
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

    use super::{GATE, GRACEFUL, ON_END, REQUESTED, STOPPING};

    /// The path to remove, as the handler takes it: never freed, as the handler may be reading
    /// it while it is replaced (a path for each output, a few bytes).
    static PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The pipe the handler tells a signal stopping the process by (where [`ON_END`] is set): a
    /// thread of its own does that, then stops the process by it. -1 where none.
    static ENDER: AtomicI32 = AtomicI32::new(-1);
    /// Whether a signal stopping the process was told to it (any other stops it at once).
    static ENDING: AtomicBool = AtomicBool::new(false);

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
        static ENDER_MADE: Once = Once::new();
        if ON_END.get().is_some() {
            ENDER_MADE.call_once(make_ender);
        }
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

    /// Makes the thread that ends the process by a signal ([`ENDER`]).
    fn make_ender() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: a pipe of two descriptors, the read end the thread's alone
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        let [rx, tx] = fds;
        let made = std::thread::Builder::new().name("aqr-end".into()).spawn(move || {
            let mut b = 0u8;
            loop {
                // SAFETY: reading a byte into `b`
                match unsafe { libc::read(rx, (&raw mut b).cast(), 1) } {
                    1 => break,
                    -1 if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
                    _ => {
                        // (gone: the handler stops the process itself)
                        ENDER.store(-1, Ordering::SeqCst);
                        return;
                    }
                }
            }
            {
                // (the output neither made nor placed meanwhile)
                let _gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
                super::end();
            }
            // SAFETY: the default restored, the signal raised again stops the process
            unsafe {
                libc::signal(b as libc::c_int, libc::SIG_DFL);
                libc::raise(b as libc::c_int);
            }
        });
        if made.is_ok() {
            ENDER.store(tx, Ordering::SeqCst);
        }
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
            // (where a thread is to end it first, and not ending it already)
            let tx = ENDER.load(Ordering::SeqCst);
            if tx >= 0 && !ENDING.swap(true, Ordering::SeqCst) {
                let b = s as u8;
                if libc::write(tx, (&raw const b).cast(), 1) == 1 {
                    return;
                }
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

    /// Whether a signal stopping the process is being handled (any other stops it at once).
    static ENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
        // (on a thread of its own: what is to be done before it ends, where not doing it already)
        if !ENDING.swap(true, Ordering::SeqCst) {
            super::end();
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
