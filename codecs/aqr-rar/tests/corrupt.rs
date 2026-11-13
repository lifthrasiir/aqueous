//! Decoders given sizes that the stream does not have.

use aqr_rar::lz3::{self, Decoder3, FilterRec};
use aqr_rar::lz5::{self, Decoder5, Filter, Placed};
use aqr_rar::{rar4, rar5};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.rar", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The first compressed file's part and size of a RAR5 archive.
fn part5(d: &[u8]) -> (&[u8], usize, bool, u64) {
    let a = rar5::parse(d).unwrap();
    let e = a.entries.iter().find(|e| !e.service && e.method != 0 && !e.data.is_empty()).unwrap();
    (&d[e.data.clone()], e.size.unwrap() as usize, e.version == 1, e.dict)
}

fn part3(d: &[u8]) -> (&[u8], usize, u64) {
    let a = rar4::parse(d).unwrap();
    let e = a.entries.iter().find(|e| !e.service && e.method != 0x30 && !e.data.is_empty()).unwrap();
    (&d[e.data.clone()], e.size as usize, e.dict)
}

#[test]
fn rar5_declared_size_bounds_allocation() {
    let d = fixture("r5_m3");
    let (p, size, v1, dict) = part5(&d);
    assert!(Decoder5::new(v1, dict).file(p, size).is_ok());
    assert!(Decoder5::new(v1, dict).file(p, usize::MAX / 2).is_err());
}

#[test]
fn rar5_output_stops_at_declared_size() {
    let d = fixture("r5_m3");
    let (p, size, v1, dict) = part5(&d);
    let mut dec = Decoder5::new(v1, dict);
    assert!(dec.file(p, size / 2).is_err());
    assert!(dec.out.len() < size, "{} of {size}", dec.out.len());
}

#[test]
fn rar3_declared_size_bounds_allocation() {
    let d = fixture("r4_m3");
    let (p, size, dict) = part3(&d);
    assert!(Decoder3::new(dict).file(p, size).is_ok());
    assert!(Decoder3::new(dict).file(p, usize::MAX / 2).is_err());
}

#[test]
fn rar3_output_stops_at_declared_size() {
    let d = fixture("r4_m3");
    let (p, size, dict) = part3(&d);
    let mut dec = Decoder3::new(dict);
    assert!(dec.file(p, size / 2).is_err());
    assert!(dec.out.len() < size, "{} of {size}", dec.out.len());
}

#[test]
fn filters_out_of_range_are_errors() {
    let f = Filter { start: 0, len: u32::MAX, kind: 1, channels: 0, start_bytes: 1, len_bytes: 4 };
    let mut d = [0u8; 16];
    assert!(lz5::unfilter(&mut d, &[Placed { at: usize::MAX - 2, f }]).is_err());
    let r = FilterRec { raw: Vec::new(), at: usize::MAX - 2, len: 16, kind: lz3::E8, regs: [0; 2] };
    assert!(lz3::unfilter(&mut d, &[r]).is_err());
}
