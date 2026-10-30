//! The global allocator of the binaries ([`Mapped`]); heap accounting for memory profiling
//! (bench -m, probes and memory tests), phase marks (feature `memprof`), and the process's
//! peak resident set.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts heap bytes in use and their peak. A growing realloc counts the old and the new
/// block together, as it may need both at once. Registered by the binaries (a library
/// does not pick the global allocator).
pub struct Counting;

pub static CUR: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Allocations of at least this many bytes are logged ([`log_large`]); 0: none.
static LOG_MIN: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Set while an allocation is being logged, whose own allocations are not.
    static LOGGING: Cell<bool> = const { Cell::new(false) };
}

#[inline]
fn add(n: usize) {
    let c = CUR.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
    let min = LOG_MIN.load(Relaxed);
    if min != 0 && n >= min {
        log(n, c);
    }
}

#[cold]
fn log(n: usize, cur: usize) {
    if LOGGING.with(|l| l.replace(true)) {
        return;
    }
    // The two innermost frames of the aqr crates, past this module's.
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    let frames: Vec<&str> = bt
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("aqr_") && !l.contains("aqr_common::mem::") && !l.starts_with("at "))
        .take(2)
        .collect();
    eprintln!("  +{:7.1} MB  in use {:7.1} MB  {}", n as f64 / 1e6, cur as f64 / 1e6, frames.join(" <- "));
    LOGGING.with(|l| l.set(false));
}

/// Logs every allocation of at least `min` bytes to stderr, with the heap in use and the
/// two innermost frames of the aqr crates (`None`: stops). Slow: a backtrace each.
pub fn log_large(min: Option<usize>) {
    LOG_MIN.store(min.unwrap_or(0), Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CUR.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            if n >= l.size() {
                // Both blocks at the moment of the copy.
                PEAK.fetch_max(CUR.load(Relaxed) + n, Relaxed);
                CUR.fetch_add(n - l.size(), Relaxed);
                let min = LOG_MIN.load(Relaxed);
                if min != 0 && n - l.size() >= min {
                    log(n - l.size(), CUR.load(Relaxed));
                }
            } else {
                CUR.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

/// Starts a peak measurement; returns the bytes in use now.
pub fn peak_reset() -> usize {
    let c = CUR.load(Relaxed);
    PEAK.store(c, Relaxed);
    c
}

/// The result of `f` and the peak heap bytes above those in use when it started.
pub fn peak<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = peak_reset();
    let r = f();
    (r, PEAK.load(Relaxed).saturating_sub(base))
}

/// The peak resident set of the process so far, in bytes (0 where unknown).
pub fn max_rss() -> u64 {
    #[cfg(unix)]
    {
        #[repr(C)]
        struct Rusage {
            utime: [i64; 2],
            stime: [i64; 2],
            maxrss: i64,
            rest: [i64; 13],
        }
        unsafe extern "C" {
            fn getrusage(who: i32, usage: *mut Rusage) -> i32;
        }
        let mut u = Rusage { utime: [0; 2], stime: [0; 2], maxrss: 0, rest: [0; 13] };
        // SAFETY: RUSAGE_SELF (0) into a struct of the size and layout of 64-bit `rusage`.
        if unsafe { getrusage(0, &mut u) } != 0 {
            return 0;
        }
        // bytes on macOS, KiB elsewhere
        let unit = if cfg!(target_vendor = "apple") { 1 } else { 1024 };
        u.maxrss as u64 * unit
    }
    #[cfg(not(unix))]
    0
}

/// Prints the heap in use and the peak since the last mark (with feature `memprof`).
#[inline]
pub fn mark(_label: &str) {
    #[cfg(feature = "memprof")]
    {
        eprintln!(
            "mem {:>24}: cur {:>8.1} MB, peak {:>8.1} MB",
            _label,
            CUR.load(Relaxed) as f64 / 1e6,
            PEAK.load(Relaxed) as f64 / 1e6
        );
        PEAK.store(CUR.load(Relaxed), Relaxed);
    }
}

/// Blocks of at least this many bytes are mapped by [`Mapped`] (as of 10gb: 1 MiB below 256 KiB
/// and 4 MiB keeps decompression the smallest, of the same speed).
pub const MAPPED_MIN: usize = 1 << 20;

/// The system's allocator, but for blocks of at least [`MAPPED_MIN`] bytes, which are mapped
/// and unmapped at once (on Unix): macOS's gives back the pages of large blocks freed only
/// some seconds later, as large blocks come and go faster than that (10gb decompression of
/// 4.4 GB in use took 7.5 GB). A growing block is extended in place where the pages after it
/// are free, as the system's realloc does. Registered by the binaries.
pub struct Mapped;

#[cfg(unix)]
mod mapped {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    static PAGE: AtomicUsize = AtomicUsize::new(0);

    pub fn page() -> usize {
        match PAGE.load(Relaxed) {
            0 => {
                // SAFETY: of no preconditions
                let p = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
                PAGE.store(p, Relaxed);
                p
            }
            p => p,
        }
    }

    pub fn round(n: usize) -> usize {
        n.next_multiple_of(page())
    }

    /// `n` bytes mapped (zeroed), at `at` if given and free there; null where they could not be.
    pub fn map(at: *mut u8, n: usize) -> *mut u8 {
        let (prot, flags) = (libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON);
        // SAFETY: a new mapping, without MAP_FIXED: nothing mapped already is touched
        let p = unsafe { libc::mmap(at.cast(), n, prot, flags, -1, 0) };
        if p == libc::MAP_FAILED { std::ptr::null_mut() } else { p.cast() }
    }

    /// # Safety
    /// `p..p + n` are pages mapped by [`map`] and nothing uses them any more.
    pub unsafe fn unmap(p: *mut u8, n: usize) {
        // SAFETY: as the caller guarantees
        unsafe { libc::munmap(p.cast(), n) };
    }
}

impl Mapped {
    #[inline]
    fn mapped(l: Layout) -> bool {
        cfg!(unix) && l.size() >= MAPPED_MIN && l.align() <= 4096
    }
}

impl Mapped {
    unsafe fn raw_alloc(&self, l: Layout) -> *mut u8 {
        #[cfg(unix)]
        if Self::mapped(l) {
            return mapped::map(std::ptr::null_mut(), mapped::round(l.size()));
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn raw_alloc_zeroed(&self, l: Layout) -> *mut u8 {
        #[cfg(unix)]
        if Self::mapped(l) {
            return mapped::map(std::ptr::null_mut(), mapped::round(l.size()));
        }
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn raw_dealloc(&self, p: *mut u8, l: Layout) {
        #[cfg(unix)]
        if Self::mapped(l) {
            // SAFETY: mapped by `alloc` as of this size, and the caller's no more
            return unsafe { mapped::unmap(p, mapped::round(l.size())) };
        }
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn raw_realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        // SAFETY (all): `l` is the block's layout, `n` its new size, of the same alignment
        let nl = unsafe { Layout::from_size_align_unchecked(n, l.align()) };
        let (old, new) = (Self::mapped(l), Self::mapped(nl));
        if !old && !new {
            return unsafe { System.realloc(p, l, n) };
        }
        #[cfg(unix)]
        if old && new {
            let (have, want) = (mapped::round(l.size()), mapped::round(n));
            if want <= have {
                if want < have {
                    unsafe { mapped::unmap(p.add(want), have - want) };
                }
                return p;
            }
            // the pages right after it, where free
            let end = unsafe { p.add(have) };
            let more = mapped::map(end, want - have);
            if more == end {
                return p;
            }
            if !more.is_null() {
                unsafe { mapped::unmap(more, want - have) };
            }
        }
        let q = unsafe { self.raw_alloc(nl) };
        if !q.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(n)) };
            unsafe { self.raw_dealloc(p, l) };
        }
        q
    }
}

/// (The heap counted for [`crate::trace`], once it is on.)
unsafe impl GlobalAlloc for Mapped {
    #[inline]
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { self.raw_alloc(l) };
        crate::trace::allocated(p, l.size());
        p
    }
    #[inline]
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { self.raw_alloc_zeroed(l) };
        crate::trace::allocated(p, l.size());
        p
    }
    #[inline]
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        crate::trace::freed(p, l.size());
        unsafe { self.raw_dealloc(p, l) }
    }
    #[inline]
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { self.raw_realloc(p, l, n) };
        crate::trace::reallocated(p, l.size(), q, n);
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped() {
        let a = Mapped;
        for (from, to) in
            [(100, 5 << 20), (3 << 20, 9 << 20), (9 << 20, 2 << 20), (2 << 20, 1000), (MAPPED_MIN, MAPPED_MIN + 1)]
        {
            let l = Layout::from_size_align(from, 8).unwrap();
            // SAFETY: blocks of these layouts, written and read within them
            unsafe {
                let p = a.alloc_zeroed(l);
                assert!(!p.is_null());
                assert!(std::slice::from_raw_parts(p, from).iter().all(|&b| b == 0));
                (0..from).for_each(|i| *p.add(i) = i as u8);
                let q = a.realloc(p, l, to);
                assert!(!q.is_null());
                assert!((0..from.min(to)).all(|i| *q.add(i) == i as u8), "{from} -> {to}");
                (0..to).for_each(|i| *q.add(i) = !(i as u8));
                a.dealloc(q, Layout::from_size_align(to, 8).unwrap());
            }
        }
    }
}
