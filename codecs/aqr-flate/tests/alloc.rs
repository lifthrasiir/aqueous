//! Counts and sizes read from untrusted data are not reserved before they are found wrong,
//! rather than aborting the process on an allocation of gigabytes.

use aqr_flate::bits::BitWriter;
use aqr_flate::code::put_gamma;

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

/// A record of an adaptive profile with no blocks that claims `nseg` segments and has no
/// bits for them.
fn record_of_segments(nseg: u64) -> Vec<u8> {
    let mut w = BitWriter::new();
    // (a rule of the catalog, no blocks, no tail padding, no trailing bytes, no zlib emulation)
    w.bits(1, 1);
    put_gamma(&mut w, 0);
    put_gamma(&mut w, 0);
    w.bits(1, 0);
    put_gamma(&mut w, 0);
    w.bits(1, 0);
    put_gamma(&mut w, nseg);
    w.bits(w.pad_len(), 0);
    let main = w.finish();
    let mut meta = aqr_flate::ADAPTIVE.to_le_bytes().to_vec();
    aqr_flate::code::put_varint(&mut meta, main.len() as u64);
    aqr_flate::code::put_varint(&mut meta, 0);
    meta.extend_from_slice(&main);
    meta
}

/// A record claiming a segment for each byte of its data is refused for its lack of bits,
/// without room made for every segment first.
#[test]
fn segments_beyond_the_record() {
    let n = 48 << 20;
    let data = vec![0u8; n];
    let meta = record_of_segments(n as u64);
    let model = aqr_flate::profile_data(aqr_flate::ADAPTIVE).unwrap();
    let (r, most) = big_alloc::largest(|| aqr_flate::reconstruct_sized(&data, &meta, model, 1, 0));
    assert!(r.is_err());
    assert!(most < (256 << 20), "{most} bytes reserved");
}

/// A container's size beyond what the stream could make (but within DEFLATE's 1032 to 1) is not
/// reserved before the stream is found wrong.
#[test]
fn parse_with_a_size_beyond_the_stream() {
    // (a stored block whose LEN and NLEN do not match)
    let raw = vec![0u8; 3 << 20];
    // (the room is asked for fallibly, so that its refusal is survived, not aborted on)
    let r = aqr_flate::inflate::parse_segments_max(&raw, usize::MAX, usize::MAX, 4 << 30, false);
    assert!(r.is_err());
}
