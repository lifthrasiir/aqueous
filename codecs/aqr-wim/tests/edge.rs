//! Edge cases: chunk tables claiming more data than the file can hold, the 8-byte table,
//! spanned and overlapping resources, a pipable WIM, damaged files.

use std::time::Duration;

use aqr_testkit::{Refusing, case, damaged, most, within};
use aqr_wim::{formula, wim};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn reshdr(size: usize, flags: u8, offset: usize, orig: u64) -> Vec<u8> {
    let mut h = (size as u64).to_le_bytes()[..7].to_vec();
    h.push(flags);
    h.extend_from_slice(&(offset as u64).to_le_bytes());
    h.extend_from_slice(&orig.to_le_bytes());
    h
}

/// A WIM file (LZX, or none) of resources `(flags, bytes, orig)` one after another, a lookup
/// table of them.
fn wim_of(res: &[(u8, Vec<u8>, u64)], compressed: bool) -> Vec<u8> {
    let mut d = vec![0u8; 208];
    let mut entries = Vec::new();
    for (flags, bytes, orig) in res {
        entries.push(reshdr(bytes.len(), *flags, d.len(), *orig));
        d.extend_from_slice(bytes);
    }
    let lookup = d.len();
    for e in &entries {
        d.extend_from_slice(e);
        d.extend_from_slice(&[1, 0, 1, 0, 0, 0]);
        d.extend_from_slice(&[0x5a; 20]);
    }
    let n = d.len() - lookup;
    let h = &mut d[..208];
    h[..8].copy_from_slice(b"MSWIM\0\0\0");
    h[8..12].copy_from_slice(&208u32.to_le_bytes());
    h[12..16].copy_from_slice(&0x10d00u32.to_le_bytes());
    let flags: u32 = if compressed { 0x40082 } else { 0 };
    h[16..20].copy_from_slice(&flags.to_le_bytes());
    h[20..24].copy_from_slice(&32768u32.to_le_bytes());
    h[48..72].copy_from_slice(&reshdr(n, 0, lookup, n as u64));
    d
}

/// A resource of `orig` bytes claimed, all chunks of one byte (the table of `es`-byte entries).
fn tiny_chunks(orig: u64, es: usize) -> Vec<u8> {
    let n = orig.div_ceil(32768) as usize;
    let mut r = Vec::new();
    for i in 1..n {
        r.extend_from_slice(&(i as u64).to_le_bytes()[..es]);
    }
    r.extend(std::iter::repeat_n(0x55u8, n));
    r
}

fn check(d: &[u8]) {
    case("claim");
    if let Some(f) = formula::encode(d, 2) {
        use aqr_archive::Evaluate;
        assert!(f.eval(&aqr_decompress::Native::default()).unwrap() == d);
    }
    assert!(most() < 256 << 20);
}

/// One-byte chunks of a huge claimed size: no chunk of 32 KiB is of so few bytes, and the
/// data they would give (4 GiB of a 128 KiB file) is not planned, let alone allocated.
#[test]
fn one_byte_chunks_of_a_huge_orig() {
    for (orig, es) in [(u32::MAX as u64, 4), (u32::MAX as u64 + 1000, 8)] {
        let d = wim_of(&[(0x04, tiny_chunks(orig, es), orig)], true);
        assert!(wim::parse(&d).unwrap().resources.is_empty());
        within(LIMIT, || check(&d)).unwrap();
        assert!(formula::encode(&d, 2).is_none());
    }
}

/// A chunk of as few bytes as an LZX chunk can be (a block's header and trees) is taken.
#[test]
fn small_chunks_taken() {
    let orig = 3 * 32768u64;
    let mut r = Vec::new();
    r.extend_from_slice(&16u32.to_le_bytes());
    r.extend_from_slice(&32u32.to_le_bytes());
    r.extend(std::iter::repeat_n(0x55u8, 48));
    let d = wim_of(&[(0x04, r, orig)], true);
    assert_eq!(wim::parse(&d).unwrap().resources.len(), 1);
}

/// Where chunks claim more than 2^43 bytes the table cannot fit the file: not taken.
#[test]
fn orig_past_the_file() {
    let d = wim_of(&[(0x04, vec![0x55; 4000], 1 << 50)], true);
    assert!(formula::encode(&d, 1).is_none());
    let d = wim_of(&[(0x04, vec![0x55; 4000], u64::MAX)], true);
    assert!(formula::encode(&d, 1).is_none());
}

/// The table of 8-byte entries (data over 4 GiB) is read as one: its chunks are found.
#[test]
fn eight_byte_table() {
    let orig = u32::MAX as u64 + 1000;
    let n = orig.div_ceil(32768) as usize;
    let mut r = Vec::new();
    for i in 1..n {
        r.extend_from_slice(&(i as u64 * 40).to_le_bytes());
    }
    r.extend(std::iter::repeat_n(0x55u8, n * 40));
    let d = wim_of(&[(0x04, r, orig)], true);
    let w = wim::parse(&d).unwrap();
    let wim::Resource::Chunks { table, chunks } = &w.resources[0] else { panic!("not chunks") };
    assert_eq!(chunks.len(), n);
    assert_eq!(table.len(), 8 * (n - 1));
    assert!(chunks.iter().all(|(r, _)| r.len() == 40));
    // read as 4-byte entries (the claim at the limit) it is another file
    let d = wim_of(&[(0x04, tiny_chunks(u32::MAX as u64, 4), u32::MAX as u64)], true);
    assert!(wim::parse(&d).unwrap().resources.is_empty());
}

/// Spanned resources are left out; resources overlapping another are left out.
#[test]
fn spanned_and_overlapping() {
    let plain = b"plain data, stored as it is. ".repeat(100);
    let n = plain.len() as u64;
    let d = wim_of(&[(0x08, plain.clone(), n)], false);
    assert!(wim::parse(&d).unwrap().resources.is_empty());
    assert!(formula::encode(&d, 1).is_none());
    // a second entry inside the first one's bytes
    let mut d = wim_of(&[(0, plain.clone(), n), (0, plain.clone(), n)], false);
    let lookup = u64::from_le_bytes(d[56..64].try_into().unwrap()) as usize;
    let first = u64::from_le_bytes(d[lookup + 8..lookup + 16].try_into().unwrap());
    d[lookup + 50 + 8..lookup + 50 + 16].copy_from_slice(&(first + 100).to_le_bytes());
    let w = wim::parse(&d).unwrap();
    assert_eq!(w.resources.len(), 1);
    check(&d);
    // two entries of the same offset: one is taken
    d[lookup + 50 + 8..lookup + 50 + 16].copy_from_slice(&first.to_le_bytes());
    assert_eq!(wim::parse(&d).unwrap().resources.len(), 1);
    check(&d);
}

/// A pipable WIM (`WLPWM`) is not one of this format.
#[test]
fn pipable() {
    let mut d = wim_of(&[(0, b"x".repeat(3000), 3000)], false);
    d[..8].copy_from_slice(b"WLPWM\0\0\0");
    assert!(!wim::starts(&d));
    assert!(formula::encode(&d, 1).is_none());
}

#[test]
fn damaged_files() {
    let d =
        wim_of(&[(0, b"plain data, stored as it is. ".repeat(40), 1160), (0x04, tiny_chunks(100000, 4), 100000)], true);
    damaged(&d, 200, 260, LIMIT, check);
}
