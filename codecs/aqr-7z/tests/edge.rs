//! Edge cases of the 7z reader: headers that lie or repeat themselves, encryption, sizes taken
//! from the header, and damage (with the CRC-32s made right, so that the parser is reached).

use std::time::Duration;

use aqr_7z::formula::{self, Options};
use aqr_7z::header;
use aqr_archive::Evaluate;
use aqr_common::crc32;
use aqr_formula::{Apply, Formula};
use aqr_testkit::{Refusing, damaged};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 28);

struct Modules;

impl aqr_archive::Modules for Modules {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
        if let Some(r) = aqr_bcj::formula::decode(a.module, &a.params, inputs, a.len) {
            return r.map_err(|e| aqr_formula::Error::new(e.to_string()));
        }
        let decode = match a.module.name {
            "lzma-recons" => aqr_lzma::formula::decode,
            "lzma2-recons" => aqr_lzma::formula::decode2,
            m => panic!("{m}"),
        };
        decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    }

    fn apply_shared(&self, s: &aqr_formula::Shared, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<Vec<u8>>> {
        let all = self.apply(s.apply().unwrap(), inputs)?;
        let mut rest = &all[..];
        Ok(s.sizes.iter().map(|&n| rest.split_off(..n as usize).unwrap().to_vec()).collect())
    }
}

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&Modules)
}

/// A NUMBER.
fn number(out: &mut Vec<u8>, v: u64) {
    let n = (0..8).find(|&n| v < 1u64 << (7 * (n + 1))).unwrap_or(8);
    let first = if n == 8 { 0xff } else { (0xff00u16 >> n) as u8 | (v >> (8 * n)) as u8 };
    out.push(first);
    out.extend(&v.to_le_bytes()[..n]);
}

/// An archive of the packed streams `packs` (one after another, from `pack_pos` 0) and the
/// header bytes `h`, the CRC-32s right.
fn wrap(packs: &[u8], h: &[u8]) -> Vec<u8> {
    let mut start = Vec::new();
    start.extend((packs.len() as u64).to_le_bytes());
    start.extend((h.len() as u64).to_le_bytes());
    start.extend(crc32::hash(h).to_le_bytes());
    let mut out = header::MAGIC.to_vec();
    out.extend([0, 4]);
    out.extend(crc32::hash(&start).to_le_bytes());
    out.extend(start);
    out.extend(packs);
    out.extend(h);
    out
}

/// `d` with the CRC-32s of its signature header made right for its header (all of what follows
/// the `packs` bytes of packed streams).
fn mend(d: &[u8], packs: usize) -> Vec<u8> {
    let end = (header::SIGNATURE + packs).min(d.len());
    wrap(&d[header::SIGNATURE.min(d.len())..end], &d[end..])
}

fn pack_info(sizes: &[u64]) -> Vec<u8> {
    let mut h = vec![6, 0];
    number(&mut h, sizes.len() as u64);
    h.push(9);
    sizes.iter().for_each(|&n| number(&mut h, n));
    h.push(0);
    h
}

/// UNPACK_INFO of `sizes.len()` folders, each of one coder `coder` (flags, id, properties) of the
/// size.
fn unpack_info(coder: &[u8], sizes: &[u64]) -> Vec<u8> {
    let mut h = vec![7, 11];
    number(&mut h, sizes.len() as u64);
    h.push(0);
    for _ in sizes {
        h.push(1);
        h.extend(coder);
    }
    h.push(12);
    sizes.iter().for_each(|&n| number(&mut h, n));
    h.push(0);
    h
}

/// FILES: `names` (all of data).
fn files(names: &[&str]) -> Vec<u8> {
    let mut h = vec![5];
    number(&mut h, names.len() as u64);
    let mut n = vec![0];
    names.iter().for_each(|s| s.encode_utf16().chain([0]).for_each(|u| n.extend(u.to_le_bytes())));
    h.push(17);
    number(&mut h, n.len() as u64);
    h.extend(n);
    h.push(0);
    h
}

/// A stored coder, as a folder's coder list entry: flags (id of 1 byte), the id.
const COPY: &[u8] = &[0x01, 0x00];
/// 7zAES (06F10701) with 2 bytes of properties.
const AES: &[u8] = &[0x24, 0x06, 0xf1, 0x07, 0x01, 2, 0x00, 0x00];

fn data() -> Vec<u8> {
    (0..100u8).map(|i| i.wrapping_mul(7)).collect()
}

/// An archive of the streams info `s` (the end of it added) and the files `names`.
fn plain(packs: &[u8], s: &[u8], names: &[&str]) -> Vec<u8> {
    let mut h = vec![1, 4];
    h.extend(s);
    h.push(0);
    h.extend(files(names));
    h.push(0);
    wrap(packs, &h)
}

fn assert_roundtrip(d: &[u8]) {
    if let Some(f) = formula::encode(d, 1) {
        assert!(eval(&f).unwrap() == d);
    }
}

/// The header of an archive of `packs` bytes of packed streams.
fn header_of(d: &[u8], packs: usize) -> &[u8] {
    &d[header::SIGNATURE + packs..]
}

#[test]
fn baseline_stored() {
    let x = data();
    let mut s = pack_info(&[100]);
    s.extend(unpack_info(COPY, &[100]));
    s.extend([8, 0]);
    let d = plain(&x, &s, &["a"]);
    assert!(header::parse(header_of(&d, 100)).is_some());
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f).unwrap() == d);
}

/// A second UNPACK_INFO changes the folders after SUBSTREAMS counted them.
#[test]
fn repeated_unpack_info() {
    let x = data();
    let packs = [&x[..], &x[..]].concat();
    let mut s = pack_info(&[100, 100]);
    s.extend(unpack_info(COPY, &[100]));
    s.extend([8, 0]);
    s.extend(unpack_info(COPY, &[100, 100]));
    let d = plain(&packs, &s, &["a"]);
    assert_roundtrip(&d);
    assert!(header::parse(header_of(&d, 200)).is_none());
    // the other way: more substreams than folders
    let mut s = pack_info(&[100, 100]);
    s.extend(unpack_info(COPY, &[100, 100]));
    s.extend([8, 0]);
    s.extend(unpack_info(COPY, &[100]));
    let d = plain(&packs, &s, &["a", "b"]);
    assert_roundtrip(&d);
    assert!(header::parse(header_of(&d, 200)).is_none());
}

#[test]
fn substreams_before_folders() {
    let x = data();
    let mut s = vec![8, 0];
    s.extend(pack_info(&[100]));
    s.extend(unpack_info(COPY, &[100]));
    assert_roundtrip(&plain(&x, &s, &["a"]));
}

#[test]
fn encrypted_folders() {
    let x = data();
    let mut s = pack_info(&[100]);
    s.extend(unpack_info(AES, &[100]));
    s.extend([8, 0]);
    let d = plain(&x, &s, &["a"]);
    assert!(header::parse(header_of(&d, 100)).is_some());
    assert!(formula::encode(&d, 1).is_none());
    // one encrypted next to a stored one: the stored taken, the other kept
    let packs = [&x[..], &x[..]].concat();
    let mut s = pack_info(&[100, 100]);
    s.extend([7, 11, 2, 0, 1]);
    s.extend(COPY);
    s.push(1);
    s.extend(AES);
    s.extend([12, 100, 100, 0]);
    s.extend([8, 0]);
    let d = plain(&packs, &s, &["a", "b"]);
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f).unwrap() == d);
}

#[test]
fn encrypted_header() {
    // an encoded header: a folder of AES over a packed stream
    let x = data();
    let mut h = vec![23];
    h.extend(pack_info(&[100]));
    h.extend(unpack_info(AES, &[100]));
    h.push(0);
    let d = wrap(&x, &h);
    assert!(matches!(header::parse(&h), Some(header::Top::Encoded(_))));
    assert!(formula::encode(&d, 1).is_none());
    assert_eq!(formula::entry_sums(&mut |r| d.get(r.start as usize..r.end as usize).map(<[u8]>::to_vec)), vec![]);
}

#[test]
fn empty_and_tiny_archives() {
    // no header at all (7-Zip's empty archive)
    let d = wrap(&[], &[]);
    assert!(formula::starts(&d));
    assert!(formula::encode(&d, 1).is_none());
    assert!(formula::encode(&wrap(&[], &[1, 0]), 1).is_none());
    assert!(formula::encode(&wrap(&[], &[1, 5, 0, 0, 0]), 1).is_none());
    assert!(formula::encode(&d[..header::SIGNATURE], 1).is_none());
}

#[test]
fn pack_streams_off_the_archive() {
    let x = data();
    let mut s = pack_info(&[100]);
    s.extend(unpack_info(COPY, &[100]));
    s.extend([8, 0]);
    // the position at the extreme
    let mut h = vec![1, 4, 6];
    number(&mut h, u64::MAX);
    h.extend(&s[1..]);
    h.extend([0, 0]);
    assert!(formula::encode(&wrap(&x, &h), 1).is_none());
    // a packed stream longer than the archive
    let mut s2 = pack_info(&[1 << 40]);
    s2.extend(unpack_info(COPY, &[100]));
    assert!(formula::encode(&plain(&x, &s2, &["a"]), 1).is_none());
    // two packed streams of 100 bytes in an archive of one: the second is the header's
    let mut s3 = pack_info(&[100, 100]);
    s3.extend(unpack_info(COPY, &[100, 100]));
    s3.extend([8, 0]);
    assert_roundtrip(&plain(&x, &s3, &["a", "b"]));
}

/// An unpack size that the data cannot hold is no allocation of it.
#[test]
fn lying_sizes_allocate_nothing() {
    let data: Vec<u8> = (0..4000u32).map(|i| (i % 251) as u8).collect();
    let packed = aqr_lzma::greedy::lzma2(&data, 1 << 20);
    let lzma2 = [0x21, 0x21, 1, 16];
    for size in [data.len() as u64 + 1, 1 << 31, (1 << 31) + 1, u64::MAX] {
        let mut s = pack_info(&[packed.len() as u64]);
        s.extend(unpack_info(&lzma2, &[size]));
        s.extend([8, 0]);
        aqr_testkit::case(&format!("lzma2 of {size}"));
        let d = plain(&packed, &s, &["a"]);
        assert!(formula::encode(&d, 1).is_none(), "{size}");
        assert!(aqr_testkit::most() < 1 << 24, "{size}: {}", aqr_testkit::most());
    }
    // stored
    for size in [101u64, 1 << 40] {
        let mut s = pack_info(&[100]);
        s.extend(unpack_info(COPY, &[size]));
        s.extend([8, 0]);
        assert!(formula::encode(&plain(&data[..100], &s, &["a"]), 1).is_none());
    }
}

/// BCJ2's folder: the coder over three stored streams, the range coder's a fourth packed one.
fn bcj2_folder(main: &[u8], call: &[u8], jump: &[u8], rc: &[u8], size: u64) -> Vec<u8> {
    let mut s = pack_info(&[main.len() as u64, call.len() as u64, jump.len() as u64, rc.len() as u64]);
    s.extend([7, 11, 1, 0, 4]);
    // BCJ2 (4 in, 1 out), then three stored coders
    s.extend([0x14, 3, 3, 1, 0x1b, 4, 1]);
    s.extend([0x01, 0, 0x01, 0, 0x01, 0]);
    // binds: in 0 from out 1, in 1 from out 2, in 2 from out 3; packed: in 4, 5, 6, 3
    s.extend([0, 1, 1, 2, 2, 3, 4, 5, 6, 3]);
    s.push(12);
    for n in [size, main.len() as u64, call.len() as u64, jump.len() as u64] {
        number(&mut s, n);
    }
    s.push(0);
    s.extend([8, 0]);
    let packs = [main, call, jump, rc].concat();
    plain(&packs, &s, &["a"])
}

#[test]
fn bcj2_folder_sizes() {
    let rc = [0u8; 5];
    let main = b"hello, no calls here".to_vec();
    let d = bcj2_folder(&main, &[], &[], &rc, main.len() as u64);
    assert!(header::parse(header_of(&d, main.len() + 5)).is_some());
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f).unwrap() == d);
    // a size nothing could give
    for size in [main.len() as u64 + 1, 1 << 40, u64::MAX >> 1, u64::MAX] {
        aqr_testkit::case(&format!("bcj2 of {size}"));
        let d = bcj2_folder(&main, &[], &[], &rc, size);
        assert_roundtrip(&d);
        assert!(aqr_testkit::most() < 1 << 24, "{size}: {}", aqr_testkit::most());
    }
}

/// BCJ2 streams of any bytes: whatever the formula makes is exact.
#[test]
fn bcj2_not_round_tripping() {
    let mut x = 0x1234_5678u32;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let mut some = 0;
    for _ in 0..300 {
        let n = 8 + next() as usize % 40;
        let main: Vec<u8> = (0..n).map(|_| if next() % 3 == 0 { 0xe8 } else { next() as u8 }).collect();
        let call: Vec<u8> = (0..4 * (next() as usize % 4)).map(|_| next() as u8).collect();
        let jump: Vec<u8> = (0..4 * (next() as usize % 3)).map(|_| next() as u8).collect();
        let rc: Vec<u8> = (0..6 + next() as usize % 10).map(|_| next() as u8).collect();
        let size = (main.len() + call.len() + jump.len()) as u64;
        for size in [size, main.len() as u64] {
            let d = bcj2_folder(&main, &call, &jump, &rc, size);
            if let Some(f) = formula::encode(&d, 1) {
                some += 1;
                assert!(eval(&f).unwrap() == d);
            }
        }
    }
    eprintln!("{some} of 600 taken");
}

/// The header of a stored archive and of 7-Zip's, damaged and the CRC-32s mended.
#[test]
fn damaged_headers() {
    let x = data();
    let mut s = pack_info(&[100, 100]);
    s.extend(unpack_info(COPY, &[100, 100]));
    s.extend([8, 0]);
    let packs = [&x[..], &x[..]].concat();
    let d = plain(&packs, &s, &["a", "b"]);
    damaged(&d, 100, d.len(), Duration::from_secs(20), |v| assert_roundtrip(&mend(v, 200)));
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    for name in ["lzma2-plain", "filter-bcj2"] {
        let d = std::fs::read(format!("{dir}/{name}.7z")).unwrap();
        let (span, _) = header::signature(&d).unwrap();
        let head = span.start as usize;
        let packs = head - header::SIGNATURE;
        // (the header's bytes at every place)
        damaged(&d[head..], 60, 0, Duration::from_secs(20), |v| {
            let mut a = d[..head].to_vec();
            a.extend(v);
            assert_roundtrip(&mend(&a, packs));
        });
    }
}

#[test]
fn options_limit() {
    let x = data();
    let mut s = pack_info(&[100]);
    s.extend(unpack_info(COPY, &[100]));
    s.extend([8, 0]);
    let d = plain(&x, &s, &["a"]);
    assert!(formula::encode_with(&d, &Options { limit: 99, ..Options::default() }).is_none());
    assert!(formula::encode_with(&d, &Options { limit: 100, ..Options::default() }).is_some());
}

/// A folder of a branch converter over a stored stream: `x` is what the converter's coder reads.
fn filter_folder(id: &[u8], x: &[u8]) -> Vec<u8> {
    let mut s = pack_info(&[x.len() as u64]);
    s.extend([7, 11, 1, 0, 2]);
    // the filter (1 in, 1 out), then a stored coder; in 0 from out 1; packed: in 1
    s.push(id.len() as u8);
    s.extend(id);
    s.extend([0x01, 0]);
    s.extend([0, 1]);
    s.push(12);
    number(&mut s, x.len() as u64);
    number(&mut s, x.len() as u64);
    s.push(0);
    s.extend([8, 0]);
    plain(x, &s, &["a"])
}

/// A branch converter over any stream of bytes: each is a bijection (aqr-bcj's tests), so the
/// formula applies its module (`Made::Apart` of a chain is not met by data) and is exact.
#[test]
fn filter_over_any_stream() {
    let mut x = 0x2468_ace1u32;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let (mut apart, mut chained) = (0, 0);
    for i in 0..400 {
        let ids: [&[u8]; 8] =
            [&[3, 3, 1, 3], &[3, 3, 2, 5], &[3, 3, 4, 1], &[3, 3, 5, 1], &[3, 3, 7, 1], &[3, 3, 8, 5], &[0xa], &[0xb]];
        let id = ids[i % ids.len()];
        let n = 16 + next() as usize % 48;
        let x: Vec<u8> = (0..n)
            .map(|_| if next() % 3 == 0 { [0xe8, 0xe9, 0, 0xff][next() as usize % 4] } else { next() as u8 })
            .collect();
        let d = filter_folder(id, &x);
        let Some(f) = formula::encode(&d, 1) else { continue };
        assert!(eval(&f).unwrap() == d, "{i}");
        // (the filter's module is applied only where its encoding gives the stream back)
        if !format!("{:?}", f.root).contains("bcj-") {
            apart += 1;
        } else {
            chained += 1;
        }
    }
    eprintln!("{apart} apart, {chained} chained");
    assert_eq!(apart, 0, "a stream that did not round-trip");
    assert_eq!(chained, 400);
}
