//! A plan's counts above what its data could hold are not reserved before they are found wrong,
//! rather than aborting the process on an allocation of terabytes.

use aqr_bvx::plan::{self, Counts, Plan};
use aqr_bvx::{module, recons};

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
fn counts_beyond_the_data() {
    let big = 1usize << 36;
    let c = Counts { parts: big, segs: big, matches: big, freqs: big, zd: big, lmd: 0, lits: 0 };
    let data = [0u8; 16];
    let (r, most) = big_alloc::largest(|| recons::plan(&data, &[], &c));
    assert!(r.is_err());
    assert!(most < (64 << 20), "{most} bytes reserved");
}

#[test]
fn triple_counts_beyond_the_data() {
    let big = 1usize << 36;
    let c = Counts { parts: 0, segs: 0, matches: 0, freqs: 0, zd: 0, lmd: big, lits: big };
    let mut out = Vec::new();
    let (r, most) = big_alloc::largest(|| plan::join(&[0u8; 16], &Plan::default(), &c, &mut out));
    assert!(r.is_err());
    assert!(most < (64 << 20), "{most} bytes reserved");
}

#[test]
fn plan_of_counts_that_overflow() {
    let big = usize::MAX / 8;
    let c = Counts { parts: 0, segs: 0, matches: 0, freqs: 0, zd: 0, lmd: big, lits: 0 };
    assert_eq!(module::plan(&c.params(), &[0, 0], 0), None);
}
