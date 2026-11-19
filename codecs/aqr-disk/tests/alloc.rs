//! Compressed files whose chunks or units declare far more data than their bytes hold: not
//! taken apart, rather than an allocation that aborts (the allocator refuses what an exhausted
//! heap would).

use aqr_disk::Compressed;
use aqr_disk::compressed::unpack;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

/// A WOF stream of XPRESS of 2 GiB of data in chunks of 1 byte each.
#[test]
fn wof_of_bytes_claiming_chunks() {
    let len: u64 = 2 << 30;
    let k = (len / 4096) as usize;
    let mut s: Vec<u8> = (1..k as u32).flat_map(|i| i.to_le_bytes()).collect();
    s.resize(s.len() + k, 0x55);
    aqr_testkit::case("wof");
    assert!(unpack(&s, &Compressed::Wof { algorithm: 0, len }).is_none());
}

/// An LZNT1 attribute of 2 GiB of holes, of no bytes.
#[test]
fn lznt1_of_holes() {
    let len: u64 = 2 << 30;
    let allocated = vec![0u32; (len / 4096) as usize];
    aqr_testkit::case("lznt1");
    let _ = unpack(&[], &Compressed::Lznt1 { len, unit: 4096, allocated });
}
