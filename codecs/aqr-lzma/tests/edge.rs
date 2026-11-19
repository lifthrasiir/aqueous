//! Edge cases: data past the limit, `.xz` files of several streams and their damaged containers,
//! `.lzma` files of a known size with an end marker.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};
use aqr_lzma::lzma::{self, Props};
use aqr_lzma::{alone, formula, greedy, lzma2, recons, xz};
use aqr_testkit::damaged;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn eval_any(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        let out = match aqr_bcj::formula::decode(a.module, &a.params, inputs, a.len) {
            Some(r) => r.map_err(|e| e.to_string()),
            None if a.module == formula::LZMA2 => {
                formula::decode2(&a.params, inputs, a.len, 1).map_err(|e| e.to_string())
            }
            None => formula::decode(&a.params, inputs, a.len, 1).map_err(|e| e.to_string()),
        };
        out.map_err(aqr_formula::Error::new)
    })
}

// --- the limit ---

#[test]
fn lzma_past_the_limit() {
    let d = fixture("text-p6.lzma");
    let h = alone::parse(&d).unwrap();
    let s = &d[alone::HEADER..];
    let dec = lzma::decode(s, h.props, h.size, 1 << 24).unwrap();
    let n = dec.data.len();
    assert!(n > 1000);
    // exactly the limit is fine; one less is not
    assert_eq!(lzma::decode(s, h.props, h.size, n).unwrap().data.len(), n);
    assert!(lzma::decode(s, h.props, h.size, n - 1).is_err());
    assert!(lzma::decode(s, h.props, h.size, 0).is_err());
    assert!(recons::split(s, h.props, h.dict, h.size, n, 1).is_ok());
    assert!(recons::split(s, h.props, h.dict, h.size, n - 1, 1).is_err());
    let mut out = Vec::new();
    assert_eq!(lzma::decode_data(s, h.props, h.size, n, &mut out).unwrap(), s.len());
    assert!(lzma::decode_data(s, h.props, h.size, n - 1, &mut out).is_err());
    // (a stream of an end marker: no size to go by)
    let marked = lzma::encode(&dec.data, h.props, &dec.packets, true);
    assert!(lzma::decode(&marked, h.props, None, n).is_ok());
    assert!(lzma::decode(&marked, h.props, None, n - 1).is_err());
}

#[test]
fn lzma_formula_past_the_limit_is_none() {
    let d = fixture("text-p6.lzma");
    assert!(formula::encode(&d, 1).is_some());
    let _g = formula::MAX.set_here(100);
    assert!(formula::encode(&d, 1).is_none());
    assert!(formula::data(&d).is_none());
}

#[test]
fn lzma2_past_the_limit() {
    let s = fixture("mixed-p6.lzma2");
    let n = lzma2::decode(&s, 1 << 24).unwrap().segments.iter().map(|x| x.data.len()).sum::<usize>();
    assert!(lzma2::decode(&s, n).is_ok());
    assert!(lzma2::decode(&s, n - 1).is_err());
    assert!(lzma2::decode(&s, 0).is_err());
    assert!(lzma2::split(&s, 8 << 20, n - 1, 1).is_err());
}

#[test]
fn xz_past_the_limit() {
    let d = fixture("crc32.xz");
    let data = formula::data_xz(&d).unwrap();
    {
        let _g = formula::MAX.set_here(data.len());
        assert!(formula::encode_xz(&d, 1).is_some());
        assert_eq!(formula::data_xz(&d).unwrap().len(), data.len());
    }
    {
        let _g = formula::MAX.set_here(data.len() - 1);
        assert!(formula::encode_xz(&d, 1).is_none());
        assert!(formula::data_xz(&d).is_none());
    }
    // blocks: the limit holds for the data of all in all; those past it are kept as they are
    let b = fixture("blocks.xz");
    let all = formula::data_xz(&b).unwrap().len();
    let _g = formula::MAX.set_here(all - 1);
    if let Some(f) = formula::encode_xz(&b, 1) {
        assert!(eval_any(&f).unwrap() == b);
        assert!(f.streams[1].data.len() < all);
    }
    assert!(formula::data_xz(&b).is_none_or(|d| d.len() < all));
}

// --- the container ---

/// The offsets of the index (its indicator byte) and of the footer of a one-stream `.xz` file.
fn index_and_footer(f: &[u8]) -> (usize, usize) {
    let foot = f.len() - 12;
    let size = (u32::from_le_bytes(f[foot + 4..foot + 8].try_into().unwrap()) as usize + 1) * 4;
    (foot - size, foot)
}

fn fix_index_crc(f: &mut [u8]) {
    let (i, foot) = index_and_footer(f);
    let c = aqr_common::crc32::hash(&f[i..foot - 4]);
    f[foot - 4..foot].copy_from_slice(&c.to_le_bytes());
}

fn fix_footer_crc(f: &mut [u8]) {
    let foot = f.len() - 12;
    let c = aqr_common::crc32::hash(&f[foot + 4..foot + 10]);
    f[foot..foot + 4].copy_from_slice(&c.to_le_bytes());
}

#[test]
fn xz_streams_and_padding() {
    let d = fixture("streams.xz");
    assert_eq!(formula::extent_xz(&d), Some(d.len()));
    let f = formula::encode_xz(&d, 1).unwrap();
    assert!(eval_any(&f).unwrap() == d);
    assert_eq!(f.streams[1].data.len(), formula::data_xz(&d).unwrap().len());
    let one = fixture("none.xz");
    for pad in 0..=9usize {
        let e = [&one[..], &vec![0; pad], &one[..], &vec![0; pad]].concat();
        let x = formula::extent_xz(&e).unwrap();
        // (padding in fours goes with the streams where another stream or the end follows it)
        if pad % 4 == 0 {
            assert_eq!(x, e.len(), "{pad}");
        } else {
            assert_eq!(x, one.len(), "pad {pad}: the first stream alone");
        }
        let f = formula::encode_xz(&e, 1).unwrap();
        assert!(eval_any(&f).unwrap() == e, "{pad}");
    }
    let two = [&one[..], &one[..]].concat();
    assert_eq!(formula::extent_xz(&two), Some(two.len()));
    // a second stream damaged (its footer's CRC): the first alone
    let mut e = two.clone();
    let n = e.len();
    e[n - 12] ^= 1;
    assert_eq!(formula::extent_xz(&e), Some(one.len()));
    let f = formula::encode_xz(&e, 1).unwrap();
    assert!(eval_any(&f).unwrap() == e);
    // a second stream cut after its header
    let e = [&one[..], &one[..12]].concat();
    assert_eq!(formula::extent_xz(&e), Some(one.len()));
}

#[test]
fn xz_container_mutations() {
    let good = fixture("none.xz");
    assert!(formula::encode_xz(&good, 1).is_some());
    let (idx, foot) = index_and_footer(&good);
    let none = |d: &[u8], what: &str| {
        assert!(formula::encode_xz(d, 1).is_none() && formula::extent_xz(d).is_none(), "{what}");
    };
    let mut d = good.clone();
    d[foot - 1] ^= 1;
    none(&d, "index CRC");
    let mut d = good.clone();
    d[idx + 1] = 2;
    fix_index_crc(&mut d);
    none(&d, "index record count");
    let mut d = good.clone();
    d[idx] = 1;
    none(&d, "index indicator");
    let mut d = good.clone();
    d[foot] ^= 1;
    none(&d, "footer CRC");
    let mut d = good.clone();
    d[foot + 4] = d[foot + 4].wrapping_add(1);
    fix_footer_crc(&mut d);
    none(&d, "backward size");
    let mut d = good.clone();
    d[foot + 4..foot + 8].fill(0xff);
    fix_footer_crc(&mut d);
    none(&d, "backward size of u32::MAX");
    let mut d = good.clone();
    d[foot + 8] ^= 1;
    fix_footer_crc(&mut d);
    none(&d, "footer flags differ from the header's");
    let mut d = good.clone();
    d[foot + 11] ^= 1;
    none(&d, "footer magic");
    let mut d = good.clone();
    d[7] = 0x10;
    assert!(!formula::starts_xz(&d));
    let mut d = good.clone();
    d[6] = 1;
    assert!(!formula::starts_xz(&d));
    for n in 0..good.len() {
        assert!(formula::encode_xz(&good[..n], 1).is_none(), "cut at {n}");
        assert!(formula::extent_xz(&good[..n]).is_none(), "cut at {n}");
    }
    // block padding: the first block's LZMA2 stream ends before the padding up to the index
    let size = (good[12] as usize + 1) * 4;
    let st = 12 + size;
    let ext = xz::lzma2_extent(&good[st..]).unwrap();
    let pad = (4 - (size + ext) % 4) % 4;
    assert_eq!(st + ext + pad, idx);
    if pad > 0 {
        let mut d = good.clone();
        d[st + ext] = 1;
        none(&d, "block padding");
    }
    let mut d = good.clone();
    d[12 + size - 1] ^= 1;
    none(&d, "block header CRC");
    let mut d = good.clone();
    d[13] |= 0x04;
    let c = aqr_common::crc32::hash(&d[12..12 + size - 4]);
    d[12 + size - 4..12 + size].copy_from_slice(&c.to_le_bytes());
    none(&d, "block header's reserved flags");
}

#[test]
fn xz_damaged() {
    for name in ["none.xz", "crc64.xz", "streams.xz", "filter-chain.xz"] {
        let d = fixture(name);
        damaged(&d, 40, 80, Duration::from_secs(20), |v| {
            let x = formula::extent_xz(v);
            assert!(x.is_none_or(|x| x <= v.len()));
            let _ = formula::data_xz(v);
            if let Some(f) = formula::encode_xz(v, 1) {
                assert!(eval_any(&f).unwrap() == v);
            }
        });
    }
}

// --- `.lzma` of a known size and an end marker ---

fn sample() -> Vec<u8> {
    let mut x = 5u64;
    (0..30000)
        .map(|i| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            if i % 7 < 4 { b'a' + (i % 5) as u8 } else { b'a' + (x >> 62) as u8 }
        })
        .collect()
}

fn lzma_file(data: &[u8], props: Props, size: Option<u64>, marker: bool) -> Vec<u8> {
    let packets = greedy::packets(data, 1 << 16);
    let s = lzma::encode(data, props, &packets, marker);
    let mut f = vec![props.byte()];
    f.extend_from_slice(&(1u32 << 16).to_le_bytes());
    f.extend_from_slice(&size.unwrap_or(u64::MAX).to_le_bytes());
    f.extend_from_slice(&s);
    f
}

#[test]
fn lzma_known_size_with_end_marker() {
    let data = sample();
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let n = data.len() as u64;
    // well-formed: a size and a marker, a marker alone, a size alone
    for (name, size, marker) in [("both", Some(n), true), ("marker", None, true), ("size", Some(n), false)] {
        let f = lzma_file(&data, props, size, marker);
        let fo = formula::encode(&f, 1).unwrap_or_else(|| panic!("{name}: not taken"));
        assert!(eval_any(&fo).unwrap() == f, "{name}");
        assert_eq!(formula::data(&f).as_deref(), Some(&data[..]), "{name}");
    }
    // a marker where the size is not reached, or the size short of the data: taken whole or not at all
    for (name, size) in [("marker early", n + 10), ("size short", n - 10)] {
        let f = lzma_file(&data, props, Some(size), true);
        if let Some(fo) = formula::encode(&f, 1) {
            assert!(eval_any(&fo).unwrap() == f, "{name}");
        }
    }
    // a size with no marker and one stream of data too short
    let f = lzma_file(&data, props, Some(n + 10), false);
    if let Some(fo) = formula::encode(&f, 1) {
        assert!(eval_any(&fo).unwrap() == f);
    }
}
