//! Heap accounting for memory profiling (bench -m, probes and memory tests), phase marks
//! (feature `memprof`), and the process's peak resident set.

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
