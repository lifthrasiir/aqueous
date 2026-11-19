//! Edge cases: filters whose output does not come back (`unfilter`), properties the filter ids
//! do not allow, BCJ2's streams that do not fit the data, lengths no data can have, damage.

use std::time::Duration;

use aqr_bcj::bra::Arch;
use aqr_bcj::formula::{self, Filter};
use aqr_bcj::{bcj2, module};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(20);

fn lcg(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            [0xe8, 0xe9, 0x00, 0xff, 0x0f, 0x85, 0x41, 0x12][(x >> 40) as usize % 8]
        })
        .collect()
}

#[test]
fn unfilter_comes_back_on_every_stream() {
    // each filter is a bijection of the bytes (the decoder of any data encodes back to it), so
    // `unfilter` is None for no stream of the filters of the formats: search over odd ones
    for seed in 0..400u64 {
        for d in [lcg(40, seed), lcg(41, seed ^ 99).iter().map(|b| b ^ seed as u8).collect()] {
            for arch in Arch::ALL {
                for pc in [0, 0x10000] {
                    let f = [Filter::Bra(arch, pc)];
                    let u = formula::unfilter(&f, &d).unwrap_or_else(|| panic!("{arch:?} {seed}"));
                    let mut e = u.clone();
                    f[0].encode(&mut e);
                    assert_eq!(e, d, "{arch:?} {seed}");
                }
            }
        }
    }
    assert_eq!(formula::unfilter(&[], b"abc").as_deref(), Some(&b"abc"[..]));
    assert_eq!(formula::unfilter(&[Filter::Bra(Arch::X86, 0)], b"").as_deref(), Some(&b""[..]));
    let chain = [Filter::Delta(2), Filter::Swap(4), Filter::Bra(Arch::Arm64, 0x100)];
    let d = lcg(1000, 5);
    let mut e = d.clone();
    chain.iter().for_each(|f| f.encode(&mut e));
    assert_eq!(formula::unfilter(&chain, &e).as_deref(), Some(&d[..]));
}

#[test]
fn filter_properties() {
    // xz
    assert_eq!(Filter::from_xz(3, &[0]), Some(Filter::Delta(1)));
    assert_eq!(Filter::from_xz(3, &[255]), Some(Filter::Delta(256)));
    assert_eq!(Filter::from_xz(3, &[]), None);
    assert_eq!(Filter::from_xz(3, &[0, 0]), None);
    assert_eq!(Filter::from_xz(4, &[]), Some(Filter::Bra(Arch::X86, 0)));
    assert_eq!(Filter::from_xz(4, &[1, 0, 0, 0]), Some(Filter::Bra(Arch::X86, 1)));
    for props in [&[0u8][..], &[0, 0], &[0, 0, 0], &[0, 0, 0, 0, 0]] {
        assert_eq!(Filter::from_xz(4, props), None, "{props:?}");
    }
    // starts not aligned for the architectures that want it
    assert_eq!(Filter::from_xz(7, &[2, 0, 0, 0]), None);
    assert_eq!(Filter::from_xz(7, &[4, 0, 0, 0]), Some(Filter::Bra(Arch::Arm, 4)));
    assert_eq!(Filter::from_xz(8, &[2, 0, 0, 0]), Some(Filter::Bra(Arch::ArmThumb, 2)));
    assert_eq!(Filter::from_xz(8, &[1, 0, 0, 0]), None);
    assert_eq!(Filter::from_xz(0xa, &[2, 0, 0, 0]), None);
    assert_eq!(Filter::from_xz(0xb, &[1, 0, 0, 0]), None);
    assert_eq!(Filter::from_xz(0xb, &[2, 0, 0, 0]), Some(Filter::Bra(Arch::RiscV, 2)));
    assert_eq!(Filter::from_xz(6, &[8, 0, 0, 0]), None);
    // ids that are not filters: none, LZMA2, the future
    for id in [0, 1, 2, 0x21, 0xc, 0xff, u64::MAX] {
        assert_eq!(Filter::from_xz(id, &[]), None, "{id}");
    }
    // 7z
    assert_eq!(Filter::from_7z(&[3], &[7]), Some(Filter::Delta(8)));
    assert_eq!(Filter::from_7z(&[3], &[]), None);
    assert_eq!(Filter::from_7z(&[3, 3, 1, 3], &[]), Some(Filter::Bra(Arch::X86, 0)));
    assert_eq!(Filter::from_7z(&[3, 3, 1, 3], &[0, 0]), None);
    assert_eq!(Filter::from_7z(&[2, 3, 2], &[0]), None);
    assert_eq!(Filter::from_7z(&[0xa], &[2, 0, 0, 0]), None);
    assert_eq!(Filter::from_7z(&[], &[]), None);
    assert_eq!(Filter::from_7z(&[3, 3], &[]), None);
    assert_eq!(Filter::from_7z(&[3, 3, 1, 3, 0], &[]), None);
    assert_eq!(Filter::from_7z(&[2, 3, 8], &[]), None);
}

#[test]
fn module_parameters_and_lengths() {
    let d = lcg(100, 1);
    let n = d.len() as u64;
    for arch in Arch::ALL {
        assert!(module::bra(arch, &[], &[&d], n).is_ok());
        assert!(module::bra(arch, &[0; 3], &[&d], n).is_err());
        assert!(module::bra(arch, &[0; 5], &[&d], n).is_err());
        if arch.alignment() != 0 {
            assert!(module::bra(arch, &1u32.to_le_bytes(), &[&d], n).is_err(), "{arch:?}");
        }
        assert!(module::bra(arch, &[], &[&d], u64::MAX).is_err());
        assert!(module::bra(arch, &[], &[], 0).is_err());
        assert!(module::bra(arch, &[], &[&[]], 0).unwrap().is_empty());
    }
    assert!(module::delta(&[0], &[&[]], 0).unwrap().is_empty());
    assert!(module::delta(&[0], &[&d], u64::MAX).is_err());
    assert!(module::swap(2, &[], &[&[]], 0).unwrap().is_empty());
    assert!(module::swap(2, &[], &[&d], u64::MAX).is_err());
    assert!(module::bcj2(&[], &[&d], n).is_err());
    assert!(module::bcj2(&[], &[&d, &d, &d], n).is_err());
}

/// A range coder's stream whose bits all decode as 0 (a code of 0), long enough for `d`.
fn rc_of_zeros(d: &[u8]) -> Vec<u8> {
    vec![0u8; 5 + d.len() / 4 + 16]
}

#[test]
fn bcj2_streams_that_do_not_fit() {
    let d = lcg(500, 2);
    let rc = rc_of_zeros(&d);
    let s = bcj2::encode(&d, &rc).unwrap();
    assert!(s.call.is_empty() && s.jump.is_empty() && s.main == d);
    assert_eq!(bcj2::decode(&s, &rc, d.len()).unwrap(), d);
    // streams longer or shorter than the size says
    assert!(bcj2::decode(&s, &rc, d.len() - 1).is_err());
    assert!(bcj2::decode(&s, &rc, d.len() + 1).is_err());
    let mut more = s.clone();
    more.call = vec![0; 4];
    assert!(bcj2::decode(&more, &rc, d.len()).is_err());
    let mut more = s.clone();
    more.jump = vec![0; 3];
    assert!(bcj2::decode(&more, &rc, d.len()).is_err());
    // a code of 2^32 - 1 is no stream, nor a stream not starting with 0
    assert!(bcj2::encode(&d, &[0, 0xff, 0xff, 0xff, 0xff, 0, 0]).is_err());
    assert!(bcj2::decode(&s, &[0, 0xff, 0xff, 0xff, 0xff, 0, 0], d.len()).is_err());
    assert!(bcj2::decode(&s, &[1, 0, 0, 0, 0, 0, 0], d.len()).is_err());
    // empty data and no stream
    assert!(bcj2::encode(b"", &[]).is_err());
    assert!(bcj2::encode(b"", &[0, 0, 0, 0, 0]).unwrap().main.is_empty());
    assert!(bcj2::decode(&Default::default(), &[0, 0, 0, 0, 0], 0).unwrap().is_empty());
    // a candidate at the very end whose bit is 1 (code at the top of the range): no room for a
    // target
    let mut tail = vec![0x90; 10];
    tail.push(0xe8);
    let ones = [0u8, 0xff, 0xff, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    assert!(bcj2::encode(&tail, &ones).is_err());
    assert!(bcj2::encode_joined(&tail, &ones).is_err());
    // the same with room: a target taken out
    let mut with = tail.clone();
    with.extend([0; 4]);
    let s = bcj2::encode(&with, &ones).unwrap();
    assert_eq!(s.call.len(), 4);
    assert_eq!(bcj2::decode(&s, &ones, with.len()).unwrap(), with);
}

#[test]
fn bcj2_huge_lengths() {
    let rc = [0u8; 8];
    let s = bcj2::Streams { main: vec![0x90; 4], ..Default::default() };
    for size in [usize::MAX, usize::MAX / 2, 1 << 62, 1 << 40] {
        aqr_testkit::case(&format!("decode of size {size}"));
        assert!(bcj2::decode(&s, &rc, size).is_err());
    }
    assert!(aqr_testkit::most() < 1 << 20);
    assert!(module::bcj2(&[], &[&[0x90; 4], &rc], u64::MAX).is_err());
}

#[test]
fn damaged_inputs() {
    let d = lcg(300, 4);
    let rc = rc_of_zeros(&d);
    aqr_testkit::damaged(&rc, 64, 16, LIMIT, |v| {
        let _ = bcj2::encode(&d, v);
        let _ = bcj2::encode_joined(&d, v);
        let s = bcj2::Streams { main: d.clone(), ..Default::default() };
        let _ = bcj2::decode(&s, v, d.len());
    });
    aqr_testkit::damaged(&d, 64, 16, LIMIT, |v| {
        let _ = bcj2::encode(v, &rc);
        let _ = bcj2::encode_joined(v, &rc);
        let _ = bcj2::decode(&bcj2::Streams { main: v.to_vec(), ..Default::default() }, &rc, v.len());
        for arch in Arch::ALL {
            let _ = module::bra(arch, &[], &[v], v.len() as u64);
        }
    });
}
