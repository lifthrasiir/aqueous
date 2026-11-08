//! Counts, lengths and sizes read from an executable or an archive are not reserved before they
//! are found wrong: the content is kept as it is (the encoder) or the decoding fails as corrupt,
//! rather than aborting the process on an allocation of gigabytes.

use aqr_exe::arch::x86::{self, DecodedStreams, Opcodes};
use aqr_exe::arch::{arm64, cil};
use aqr_exe::{codesign, format, reloc, symtab};

/// A global allocator that remembers the largest single request of each thread that it granted,
/// and refuses (as an exhausted heap does) those of 1 GiB and above: an infallible reservation
/// so refused aborts, which is how a size from the input reserving its room before it is
/// checked shows; a fallible one (`try_reserve`) fails cleanly.
mod big_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Largest;

    thread_local! {
        static MOST: Cell<usize> = const { Cell::new(0) };
    }

    const REFUSED: usize = 1 << 30;

    fn note(n: usize) -> bool {
        if n >= REFUSED {
            return false;
        }
        let _ = MOST.try_with(|m| m.set(m.get().max(n)));
        true
    }

    unsafe impl GlobalAlloc for Largest {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if note(l.size()) {
                unsafe { System.alloc(l) }
            } else {
                std::ptr::null_mut()
            }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            if note(l.size()) {
                unsafe { System.alloc_zeroed(l) }
            } else {
                std::ptr::null_mut()
            }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            if note(n) {
                unsafe { System.realloc(p, l, n) }
            } else {
                std::ptr::null_mut()
            }
        }
    }

    /// The largest single allocation that `f` was given on this thread.
    pub fn largest<R>(f: impl FnOnce() -> R) -> (R, usize) {
        MOST.with(|m| m.set(0));
        let r = f();
        (r, MOST.with(Cell::get))
    }
}

#[global_allocator]
static ALLOC: big_alloc::Largest = big_alloc::Largest;

/// What `f` may reserve at most on input this small.
const LIMIT: usize = 64 << 20;

/// A declared output length no input could fill.
const HUGE: usize = 1 << 40;

#[test]
fn reloc_declared_length() {
    let (r, most) = big_alloc::largest(|| reloc::decode(&[], &[], 2, HUGE));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

#[test]
fn symtab_declared_length() {
    // no entries, no head
    let params = [0u8; 8];
    let (r, most) = big_alloc::largest(|| symtab::decode(&params, [&[][..]; symtab::NUM_STREAMS], HUGE));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

#[test]
fn arm64_declared_length() {
    let (r, most) = big_alloc::largest(|| arm64::decode([&[][..]; arm64::NUM_STREAMS], 0, HUGE));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

#[test]
fn arm64_delta_declared_length() {
    let (r, most) = big_alloc::largest(|| arm64::delta::decode(&[], [&[][..]; arm64::delta::NUM_STREAMS], HUGE));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

#[test]
fn x86_declared_length() {
    let streams = DecodedStreams::new(0, 0, [&[][..]; x86::NUM_STREAMS]);
    let (r, most) = big_alloc::largest(|| x86::decode_with(&streams, true, &Opcodes::BUILTIN, HUGE));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

/// Flags set by the gigabit: 2^29 flagged references of 16 bytes each, though the section is
/// empty.
#[test]
fn x86_remap_flags_beyond_the_section() {
    let flags = vec![0xffu8; 16 << 20];
    let mut streams = [&[][..]; x86::remap::NUM_STREAMS];
    streams[x86::remap::CALLF] = &flags;
    let (r, most) = big_alloc::largest(|| x86::remap::decode(0, 0, streams, true, &Opcodes::BUILTIN, 64));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

#[test]
fn arm64_remap_flags_beyond_the_section() {
    let flags = vec![0xffu8; 64 << 20];
    let mut streams = [&[][..]; arm64::remap::NUM_STREAMS];
    streams[arm64::remap::BLF] = &flags;
    let (r, most) = big_alloc::largest(|| arm64::remap::decode(streams, 0, 0));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

#[test]
fn cil_declared_length() {
    let (r, most) = big_alloc::largest(|| cil::decode([&[][..]; cil::NUM_STREAMS], 0, 0, HUGE));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

/// A #~ stream of 4 GB claiming 400 million rows of a table, in a section of 64 bytes.
#[test]
fn cil_rows_beyond_the_section() {
    let mut meta = Vec::new();
    meta.extend_from_slice(b"BSJB");
    meta.extend_from_slice(&[0; 8]);
    meta.extend_from_slice(&0u32.to_le_bytes()); // version length
    meta.extend_from_slice(&[0, 0, 1, 0]); // flags, one stream
    meta.extend_from_slice(&32u32.to_le_bytes()); // its offset: the header follows the root
    meta.extend_from_slice(&0xffff_fff0u32.to_le_bytes()); // its size
    meta.extend_from_slice(b"#~\0\0");
    assert_eq!(meta.len(), 32);
    // the tables' header: the Module table (10 bytes a row) present, 400 million rows
    let mut header = [0u8; 28];
    header[8..16].copy_from_slice(&1u64.to_le_bytes());
    header[24..28].copy_from_slice(&400_000_000u32.to_le_bytes());
    meta.extend_from_slice(&header);
    let mut streams = [&[][..]; cil::NUM_STREAMS];
    streams[cil::META] = &meta;
    let (r, most) = big_alloc::largest(|| cil::decode(streams, 0, 0, 64));
    assert!(r.is_none());
    assert!(most < LIMIT, "{most} bytes reserved");
}

/// A PE image whose CLR header (data directory 14) claims 3.75 GB in a section of that stored
/// size, in a file of a few hundred bytes.
#[test]
fn pe_clr_header_beyond_the_file() {
    let mut d = vec![0u8; 0x300];
    let mut put = |at: usize, v: &[u8]| d[at..at + v.len()].copy_from_slice(v);
    put(0, b"MZ");
    put(0x3c, &0x40u32.to_le_bytes());
    put(0x40, b"PE\0\0");
    put(0x44, &0x8664u16.to_le_bytes());
    put(0x46, &1u16.to_le_bytes()); // sections
    put(0x54, &0xe8u16.to_le_bytes()); // optional header size
    let opt = 0x58;
    put(opt, &0x20bu16.to_le_bytes());
    put(opt + 0x6c, &15u32.to_le_bytes()); // data directories
    put(opt + 0x70 + 14 * 8, &0x1000u32.to_le_bytes());
    put(opt + 0x70 + 14 * 8 + 4, &0xf000_0000u32.to_le_bytes());
    let sect = opt + 0xe8;
    put(sect + 12, &0x1000u32.to_le_bytes()); // RVA
    put(sect + 16, &0xf000_0000u32.to_le_bytes()); // stored size
    put(sect + 20, &0x200u32.to_le_bytes()); // offset
    let (r, most) = big_alloc::largest(|| format::pe::locate_code(&mut std::io::Cursor::new(&d)));
    assert!(r.is_ok());
    assert!(most < LIMIT, "{most} bytes reserved");
}

/// A code directory whose hashes have no size (and no slots): hashing its pages is no
/// allocation, but it divided the slots into chunks of 0 and panicked.
#[test]
fn codesign_hash_size_zero() {
    let mut sig = vec![0u8; 20 + 44];
    sig[0..4].copy_from_slice(&0xfade_0cc0u32.to_be_bytes());
    sig[4..8].copy_from_slice(&64u32.to_be_bytes());
    sig[8..12].copy_from_slice(&1u32.to_be_bytes());
    sig[16..20].copy_from_slice(&20u32.to_be_bytes());
    let cd = &mut sig[20..];
    cd[0..4].copy_from_slice(&0xfade_0c02u32.to_be_bytes());
    cd[4..8].copy_from_slice(&44u32.to_be_bytes());
    cd[16..20].copy_from_slice(&44u32.to_be_bytes()); // hash offset
    cd[37] = 2; // SHA-256
    let mut d = vec![0u8; 16];
    d.extend_from_slice(&sig);
    assert!(codesign::encode(&d, &[(0, 16, sig.len())]).is_none());
}
