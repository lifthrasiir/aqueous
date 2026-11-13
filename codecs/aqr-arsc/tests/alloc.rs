//! Counts in a string pool's header above its chunk are refused before the room for them is
//! reserved, rather than aborting the process on an allocation of gigabytes.

use aqr_arsc::arsc::{self, mode, s, ty};

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

/// The module's inputs for a file of one string pool chunk of `size` bytes, its header
/// (`n`, `ns`, flags 0, `strings`, `styles`), and the streams `style` and `text16`.
fn pool(size: u32, n: u32, ns: u32, strings: u32, styles: u32, style: &[u8], text16: &[u8]) -> Vec<Vec<u8>> {
    let mut head = Vec::new();
    head.extend_from_slice(&ty::STRING_POOL.to_le_bytes());
    head.extend_from_slice(&28u16.to_le_bytes());
    head.extend_from_slice(&size.to_le_bytes());
    for v in [n, ns, 0, strings, styles] {
        head.extend_from_slice(&v.to_le_bytes());
    }
    head.push(mode::POOL);
    let mut st = vec![Vec::new(); s::N];
    st[s::HEAD] = head;
    st[s::STYLE] = style.to_vec();
    st[s::TEXT16] = text16.to_vec();
    arsc::pack(&st)
}

fn decode(inputs: &[Vec<u8>], len: usize) -> (aqr_arsc::Result<Vec<u8>>, usize) {
    let refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();
    big_alloc::largest(|| arsc::decode(&refs, len))
}

#[test]
fn offsets_of_a_count_beyond_the_chunk() {
    // (2^29 strings: the room for their offsets, 2 GiB, in a chunk of 28 bytes)
    let n = 1 << 29;
    let (r, most) = decode(&pool(28, n, 0, 28 + 4 * n, 0, &[], &[]), 28);
    assert!(r.is_err());
    assert!(most < 1 << 20, "{most} bytes reserved");
}

#[test]
fn padding_to_styles_beyond_the_chunk() {
    // (a string and a style offset of 2 GiB, in a chunk of 40 bytes holding the offsets, the
    // style's 4 bytes and the string)
    let (r, most) = decode(&pool(40, 1, 1, 36, 1 << 31, &[0; 4], &[0, 0]), 40);
    assert!(r.is_err());
    assert!(most < 1 << 20, "{most} bytes reserved");
}
