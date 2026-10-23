//! Peak heap use of coding small PSD files: the encoder's buffers must follow the file, not
//! sit at their largest whatever its size. (A test binary of its own: the allocator counts
//! every thread.)

mod common;

use aqr_psd::formula;
use common::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    PEAK.fetch_max(LIVE.fetch_add(n, Relaxed) + n, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            if n > l.size() {
                grew(n - l.size());
            } else {
                LIVE.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Peak heap bytes above the current level while running `f`.
fn peak<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let r = f();
    (r, PEAK.load(Relaxed) - base)
}

#[test]
fn memory_follows_small_files() {
    // layers and a composite predicted from them: every part of the encoder runs
    let file = hinted_file(400, 300);
    for threads in [1, 4] {
        let (f, enc) = peak(|| formula::encode(&file, threads).expect("not coded"));
        let (back, dec) =
            peak(|| f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| formula::apply(a, i, threads)).unwrap());
        assert!(back == file, "roundtrip ({threads} threads)");
        let mb = |x: usize| x as f64 / 1e6;
        println!(
            "{threads} threads: {:.2} MB file, peak {:.2} MB encoding, {:.2} MB decoding",
            mb(file.len()),
            mb(enc),
            mb(dec)
        );
        // the streams and rows of a few batches in flight, and the model's tables
        let bound = 4 * file.len() + (2 << 20);
        assert!(enc < bound, "{threads} threads: encoding peak {enc} bytes, over {bound}");
        assert!(dec < bound, "{threads} threads: decoding peak {dec} bytes, over {bound}");
    }
}
