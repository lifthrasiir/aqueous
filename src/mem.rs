//! Heap accounting for memory profiling (bench -m).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts heap bytes in use and their peak. A growing realloc counts the old and the new
/// block together, as it may need both at once. Registered by the `objr` binary (a library
/// does not pick the global allocator).
pub struct Counting;

pub static CUR: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn add(n: usize) {
    let c = CUR.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
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
