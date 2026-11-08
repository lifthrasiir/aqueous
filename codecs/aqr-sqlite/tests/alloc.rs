//! Streams claiming more groups than their bytes can hold are refused before the groups' room
//! is reserved, rather than aborting the process on an allocation of terabytes.

use aqr_sqlite::page::Geom;
use aqr_sqlite::transform::{NSTREAMS, S_DIR, Sizes, plan};

/// A global allocator that remembers the largest single request of each thread, and refuses
/// (as an exhausted heap does) those of 1 GiB and above: the aborted allocation is how a count
/// from the input reserving its room before it is checked shows.
mod big_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Largest;

    thread_local! {
        static MOST: Cell<usize> = const { Cell::new(0) };
    }

    const REFUSED: usize = 1 << 30;

    fn note(n: usize) -> bool {
        let _ = MOST.try_with(|m| m.set(m.get().max(n)));
        n < REFUSED
    }

    unsafe impl GlobalAlloc for Largest {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if note(l.size()) { unsafe { System.alloc(l) } } else { std::ptr::null_mut() }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            if note(l.size()) { unsafe { System.alloc_zeroed(l) } } else { std::ptr::null_mut() }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            if note(n) { unsafe { System.realloc(p, l, n) } } else { std::ptr::null_mut() }
        }
    }

    /// The largest single allocation that `f` asked for on this thread.
    pub fn largest<R>(f: impl FnOnce() -> R) -> (R, usize) {
        MOST.with(|m| m.set(0));
        let r = f();
        (r, MOST.with(Cell::get))
    }
}

#[global_allocator]
static ALLOC: big_alloc::Largest = big_alloc::Largest;

#[test]
fn plan_of_a_directory_of_more_groups_than_bytes() {
    let g = Geom { psize: 4096, usable: 4096, npages: 1, fmt4: true };
    let mut dir = Vec::new();
    // (a varint of 2^40 groups, and nothing after)
    let mut v = 1u64 << 40;
    while v >= 0x80 {
        dir.push(v as u8 | 0x80);
        v >>= 7;
    }
    dir.push(v as u8);
    let empty: &[u8] = &[];
    let mut s = vec![empty; NSTREAMS];
    s[S_DIR] = &dir;
    let (r, most) = big_alloc::largest(|| plan(&g, 4096, &s, &Sizes::default(), 0, 0));
    assert!(r.is_none());
    assert!(most < (1 << 20), "{most} bytes reserved");
}
