//! A file claiming the most data its chunks can (2048 times their bytes) is not planned as
//! buffers all at once: memory stays bounded by the applications' data, however large the claim.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use aqr_testkit::Refusing;
use aqr_wim::formula;

/// [`Refusing`], counting the bytes held at once.
struct Counting(Refusing);

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize) {
    let live = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(live, Relaxed);
}

// SAFETY: Refusing's, counting what it allows.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { self.0.alloc(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { self.0.alloc_zeroed(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { self.0.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { self.0.realloc(p, l, n) };
        if !q.is_null() {
            LIVE.fetch_sub(l.size(), Relaxed);
            add(n);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting(Refusing(256 << 20));

fn reshdr(size: usize, flags: u8, offset: usize, orig: u64) -> Vec<u8> {
    let mut h = (size as u64).to_le_bytes()[..7].to_vec();
    h.push(flags);
    h.extend_from_slice(&(offset as u64).to_le_bytes());
    h.extend_from_slice(&orig.to_le_bytes());
    h
}

/// A compressed WIM of one resource of `orig` bytes claimed, in chunks of 16 bytes each.
fn claiming(orig: u64) -> Vec<u8> {
    let n = orig.div_ceil(32768) as usize;
    let mut r = Vec::new();
    for i in 1..n {
        r.extend_from_slice(&((i * 16) as u64).to_le_bytes()[..4]);
    }
    r.extend(std::iter::repeat_n(0x55u8, n * 16));
    let mut d = vec![0u8; 208];
    let e = reshdr(r.len(), 0x04, d.len(), orig);
    d.extend_from_slice(&r);
    let lookup = d.len();
    d.extend_from_slice(&e);
    d.extend_from_slice(&[1, 0, 1, 0, 0, 0]);
    d.extend_from_slice(&[0x5a; 20]);
    let n = d.len() - lookup;
    d[..8].copy_from_slice(b"MSWIM\0\0\0");
    d[8..12].copy_from_slice(&208u32.to_le_bytes());
    d[12..16].copy_from_slice(&0x10d00u32.to_le_bytes());
    d[16..20].copy_from_slice(&0x40082u32.to_le_bytes());
    d[20..24].copy_from_slice(&32768u32.to_le_bytes());
    d[48..72].copy_from_slice(&reshdr(n, 0, lookup, n as u64));
    d
}

/// 2 MiB of chunks claiming 4 GiB: nothing is allocated for more than a few applications.
#[test]
fn claimed_data_is_not_planned_whole() {
    let d = claiming((4 << 30) - 32768);
    assert!(aqr_wim::wim::parse(&d).is_some_and(|w| !w.resources.is_empty()));
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let base = LIVE.load(Relaxed);
    assert!(formula::encode(&d, 2).is_none());
    let peak = PEAK.load(Relaxed) - base;
    assert!(peak < 160 << 20, "peak {peak} bytes");
}
