use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use aqr_api::stream::Slice;
use aqr_common::varint;
use aqr_walk::chunks;
use aqr_walk::reader::Reader;

/// The largest single allocation so far.
static LARGEST: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LARGEST.fetch_max(l.size(), Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn table(entries: &[(u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    chunks::put(&mut out, entries.iter().copied());
    out
}

#[test]
fn ten_byte_varints() {
    let mut d = Vec::new();
    varint::put(&mut d, u64::MAX);
    assert_eq!(d.len(), 10);
    assert_eq!(Reader::new(&mut Slice(&d), 16).varint(), Ok(Some(u64::MAX)));
    // the tenth byte may be 1 at most, and no byte follows it
    let mut bad = d.clone();
    bad[9] = 2;
    assert!(Reader::new(&mut Slice(&bad), 16).varint().is_err());
    bad[9] = 0x81;
    bad.push(0);
    assert!(Reader::new(&mut Slice(&bad), 16).varint().is_err());
}

#[test]
fn tables_with_big_numbers() {
    let t = table(&[(u64::MAX, u64::MAX), (1, 2)]);
    let mut pos = 0;
    assert_eq!(chunks::read_at(&t, &mut pos).unwrap(), [(u64::MAX, u64::MAX), (1, 2)]);
    assert_eq!(pos, t.len());
    let got = chunks::read(&mut Reader::new(&mut Slice(&t), 16), t.len() as u64).unwrap();
    assert_eq!(got, [(u64::MAX, u64::MAX), (1, 2)]);
}

#[test]
fn a_count_beyond_the_input_reserves_nothing() {
    // sixteen million chunks claimed in three bytes
    let mut t = Vec::new();
    varint::put(&mut t, 1 << 24);
    t.push(0);
    let before = LARGEST.swap(0, Relaxed);
    assert!(chunks::read(&mut Reader::new(&mut Slice(&t), 16), t.len() as u64).is_err());
    assert!(chunks::read_at(&t, &mut 0).is_err());
    assert!(LARGEST.swap(before, Relaxed) < 1 << 20);
}
