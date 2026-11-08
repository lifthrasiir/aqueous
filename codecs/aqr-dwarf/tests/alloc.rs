//! Tables and params claiming more than their bytes can hold are refused before their room is
//! reserved, rather than aborting the process on an allocation of gigabytes.

use aqr_dwarf::info::{self, NUM_STREAMS};

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

/// The allocations that damaged input asks for stay small.
const SMALL: usize = 1 << 20;

#[test]
fn apple_table_of_atoms_beyond_its_bytes() {
    // magic, version 1, hash function 0, buckets, hashes, the data's length, the atoms' count
    let count: u32 = 0x3fff_fff0;
    let mut d = Vec::new();
    d.extend_from_slice(&0x4841_5348u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes());
    d.extend_from_slice(&1u32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&(8 + 4 * count).to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&count.to_le_bytes());
    d.extend_from_slice(&[0; 16]);
    let (r, most) = big_alloc::largest(|| aqr_dwarf::apple::encode(&d, b"\0"));
    assert!(r.is_none());
    assert!(most < SMALL, "{most} bytes reserved");
}

#[test]
fn info_shape_beyond_its_bytes() {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    let empty: Vec<&[u8]> = vec![&[]; 2 + NUM_STREAMS];
    // the strings, the DIEs, those open and the references after, one claimed at a time
    for k in 0..4 {
        let mut params = Vec::new();
        for i in 0..4 {
            varint(&mut params, if i == k { 1 << 40 } else { 0 });
        }
        let (r, most) = big_alloc::largest(|| info::decode(&params, &empty, 0));
        assert!(r.is_err(), "claim {k}");
        assert!(most < SMALL, "claim {k}: {most} bytes reserved");
    }
}
