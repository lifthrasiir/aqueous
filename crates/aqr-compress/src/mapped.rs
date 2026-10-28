//! Files read mapped into memory ([`Mapper`]), as a [`Source`](crate::Source) reads the
//! contents to archive: what is read of them is the system's to drop and read again, not the
//! heap's, and is let go of as the bytes are dropped.
//!
//! A file cut short while it is mapped faults where it is read past its new end (`SIGBUS`).
//! The fault is taken by a handler of the process's, installed as the first mapper is made and
//! left so: in a mapping of a mapper, it puts a page of zeros where the fault was, tells of the
//! file as cut ([`Mapper::cut`]) and the reading goes on; any other fault it hands on to the
//! handler there was before it (Rust's own for stack overflows, Wasmtime's for its traps). As
//! the handler is the process's, a host opts in by making a mapper, as it does a
//! [`Sandbox`](aqr_decompress::sandbox::Sandbox): nothing else of this crate maps files. Where
//! a mapping cannot be told of to the handler (more of them at once than it keeps), the file
//! is read instead. On Windows a mapped file cannot be cut short.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aqr_formula::Bytes;

/// Files of at least this many bytes are mapped rather than read: of fewer, a copy costs less
/// than a mapping.
pub const MAP_MIN: u64 = 64 << 10;

/// Reads files, mapped into memory where they are of at least [`MAP_MIN`] bytes, and tells of
/// those cut short while mapped.
pub struct Mapper {
    /// By file, whether it was cut short while mapped.
    cut: Mutex<HashMap<PathBuf, Arc<AtomicBool>>>,
}

impl Mapper {
    /// A mapper; the first one installs the handler of faults (where it fails, so does this).
    pub fn new() -> io::Result<Mapper> {
        os::install()?;
        Ok(Mapper { cut: Mutex::default() })
    }

    /// The bytes of file `path`, mapped where it is of at least [`MAP_MIN`] bytes.
    pub fn read(&self, path: &Path) -> io::Result<Bytes> {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        if len >= MAP_MIN {
            let cut = self.cut.lock().unwrap().entry(path.to_path_buf()).or_default().clone();
            // SAFETY: the file is not to change while it is mapped; where it is cut short, the
            // handler takes the fault, and where it changes otherwise, the archive's
            // verification (or a reread) tells
            let map = unsafe { memmap2::Mmap::map(&f) }?;
            if let Some(m) = os::Mapped::of(map, cut) {
                return Ok(Bytes::shared(Arc::new(m)));
            }
        }
        let mut d = Vec::with_capacity(len as usize);
        f.read_to_end(&mut d)?;
        Ok(d.into())
    }

    /// Whether file `path` was cut short while it was mapped: what was read of it then is
    /// not its.
    pub fn cut(&self, path: &Path) -> bool {
        self.cut.lock().unwrap().get(path).is_some_and(|c| c.load(Ordering::SeqCst))
    }
}

#[cfg(unix)]
mod os {
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering, fence};
    use std::sync::{Arc, Mutex, OnceLock};

    /// The mappings told of to the handler at most at once.
    const SLOTS: usize = 1 << 14;

    /// A mapping as the handler finds it: its bytes and the flag of its file, read as a
    /// sequence lock (`seq` odd while they change).
    struct Slot {
        seq: AtomicUsize,
        start: AtomicUsize,
        end: AtomicUsize,
        cut: AtomicPtr<AtomicBool>,
    }

    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY: Slot = Slot {
        seq: AtomicUsize::new(0),
        start: AtomicUsize::new(0),
        end: AtomicUsize::new(0),
        cut: AtomicPtr::new(std::ptr::null_mut()),
    };
    static TABLE: [Slot; SLOTS] = [EMPTY; SLOTS];
    /// Slots used so far (those the handler looks at), and those of them free.
    static USED: AtomicUsize = AtomicUsize::new(0);
    static FREE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    static PAGE: AtomicUsize = AtomicUsize::new(0);
    /// What there was before the handler, to hand other faults on to.
    static OLD: OnceLock<Old> = OnceLock::new();

    struct Old(libc::sigaction);

    // SAFETY: a handler and its flags, plain data
    unsafe impl Send for Old {}
    unsafe impl Sync for Old {}

    fn set(i: usize, start: usize, end: usize, cut: *const AtomicBool) {
        let s = &TABLE[i];
        s.seq.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        s.start.store(start, Ordering::Relaxed);
        s.end.store(end, Ordering::Relaxed);
        s.cut.store(cut.cast_mut(), Ordering::Relaxed);
        s.seq.fetch_add(1, Ordering::Release);
    }

    /// The flag of the mapping that holds `addr`, if one does. Async-signal-safe.
    fn find(addr: usize) -> Option<&'static AtomicBool> {
        for s in &TABLE[..USED.load(Ordering::Acquire).min(SLOTS)] {
            let seq = s.seq.load(Ordering::Acquire);
            let (start, end, cut) =
                (s.start.load(Ordering::Relaxed), s.end.load(Ordering::Relaxed), s.cut.load(Ordering::Relaxed));
            fence(Ordering::Acquire);
            // (a slot changing is not of the mapping read: that is told of before it is read,
            // and kept as long as it is)
            if seq % 2 == 1 || s.seq.load(Ordering::Relaxed) != seq || !(start..end).contains(&addr) {
                continue;
            }
            // SAFETY: the flag lives as long as the mapping is told of ([`Mapped`])
            return unsafe { cut.as_ref() };
        }
        None
    }

    /// A file mapped, told of to the handler until dropped.
    pub(super) struct Mapped {
        map: memmap2::Mmap,
        slot: usize,
        /// Held for the handler, which has it by its address.
        _cut: Arc<AtomicBool>,
    }

    impl Mapped {
        /// `map` told of to the handler, the flag of its file `cut`; None where no slot is
        /// free.
        pub(super) fn of(map: memmap2::Mmap, cut: Arc<AtomicBool>) -> Option<Mapped> {
            let slot = FREE.lock().unwrap().pop().or_else(|| {
                let i = USED.fetch_add(1, Ordering::AcqRel);
                (i < SLOTS).then_some(i)
            })?;
            let start = map.as_ptr() as usize;
            set(slot, start, start + map.len(), Arc::as_ptr(&cut));
            Some(Mapped { map, slot, _cut: cut })
        }
    }

    impl Drop for Mapped {
        fn drop(&mut self) {
            // (no longer told of before it is unmapped, as the fields are dropped)
            set(self.slot, 0, 0, std::ptr::null());
            FREE.lock().unwrap().push(self.slot);
        }
    }

    impl AsRef<[u8]> for Mapped {
        fn as_ref(&self) -> &[u8] {
            &self.map
        }
    }

    pub(super) fn install() -> io::Result<()> {
        static DONE: OnceLock<Result<(), i32>> = OnceLock::new();
        let r = DONE.get_or_init(|| {
            // SAFETY: sysconf, and a handler doing only what is async-signal-safe
            unsafe {
                PAGE.store(libc::sysconf(libc::_SC_PAGESIZE) as usize, Ordering::Relaxed);
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut old) != 0 {
                    return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0));
                }
                let _ = OLD.set(Old(old));
                let mut a: libc::sigaction = std::mem::zeroed();
                a.sa_sigaction = handler as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
                    as libc::sighandler_t;
                // (on the alternate stack where there is one, as Rust's handler for stack
                // overflows, handed on to, must be)
                a.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut a.sa_mask);
                if libc::sigaction(libc::SIGBUS, &a, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0));
                }
            }
            Ok(())
        });
        r.map_err(io::Error::from_raw_os_error)
    }

    extern "C" fn handler(sig: libc::c_int, info: *mut libc::siginfo_t, cx: *mut libc::c_void) {
        // SAFETY: the information the system gives with the signal
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let addr = unsafe { (*info).si_addr() } as usize;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let addr = unsafe { (*info).si_addr } as usize;
        if let Some(cut) = find(addr) {
            let page = PAGE.load(Ordering::Relaxed);
            // SAFETY: a page of the mapping replaced by zeros, as it is read only
            let p = unsafe {
                libc::mmap(
                    (addr & !(page - 1)) as *mut libc::c_void,
                    page,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
                    -1,
                    0,
                )
            };
            if p != libc::MAP_FAILED {
                cut.store(true, Ordering::SeqCst);
                return;
            }
        }
        // handed on: to the handler before, or else as the system would (the fault, taken
        // again as the handler returns, then ends the process)
        // SAFETY: the handler before as it was set, called as its flags tell
        unsafe {
            match OLD.get().map(|o| &o.0) {
                Some(o) if o.sa_sigaction != libc::SIG_DFL && o.sa_sigaction != libc::SIG_IGN => {
                    if o.sa_flags & libc::SA_SIGINFO != 0 {
                        let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                            std::mem::transmute(o.sa_sigaction);
                        f(sig, info, cx)
                    } else {
                        let f: extern "C" fn(libc::c_int) = std::mem::transmute(o.sa_sigaction);
                        f(sig)
                    }
                }
                _ => {
                    let mut a: libc::sigaction = std::mem::zeroed();
                    a.sa_sigaction = libc::SIG_DFL;
                    libc::sigaction(sig, &a, std::ptr::null_mut());
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod os {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    pub(super) struct Mapped(memmap2::Mmap);

    impl Mapped {
        pub(super) fn of(map: memmap2::Mmap, _: Arc<AtomicBool>) -> Option<Mapped> {
            Some(Mapped(map))
        }
    }

    impl AsRef<[u8]> for Mapped {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }

    pub(super) fn install() -> io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A file cut short while mapped: read as it was up to its new end, as zeros past it, and
    /// told of as cut.
    #[test]
    fn cut_short() {
        let path = std::env::temp_dir().join(format!("aqr-mapped-{}", std::process::id()));
        std::fs::write(&path, vec![7u8; 1 << 20]).unwrap();
        let m = Mapper::new().unwrap();
        let d = m.read(&path).unwrap();
        assert!(d.is_shared() && d.iter().all(|&b| b == 7) && !m.cut(&path));
        std::fs::File::options().write(true).open(&path).unwrap().set_len(1000).unwrap();
        assert!(d[..1000].iter().all(|&b| b == 7) && d[1000..].iter().all(|&b| b == 0));
        assert!(m.cut(&path));
        drop(d);
        // (mapped again, of its new length: read as it is)
        let d = m.read(&path).unwrap();
        assert_eq!(&d[..], &[7; 1000]);
        std::fs::remove_file(&path).unwrap();
    }

    /// Other faults handed on: a stack overflow still told of by Rust's handler (a fault in
    /// the guard page, `SIGBUS` on macOS), in a child process of this test.
    #[test]
    fn others_handed_on() {
        const CHILD: &str = "AQR_MAPPED_OVERFLOW";
        if std::env::var_os(CHILD).is_some() {
            let _m = Mapper::new().unwrap();
            fn deep(n: u64) -> u64 {
                let a = std::hint::black_box([n; 1024]);
                if n == 0 { 0 } else { deep(n - 1) + a[(n % 1024) as usize] }
            }
            std::thread::Builder::new().stack_size(1 << 20).spawn(|| deep(1 << 20)).unwrap().join().unwrap();
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mapped::tests::others_handed_on", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && err.contains("has overflowed its stack"), "{:?}: {err}", out.status);
    }
}
