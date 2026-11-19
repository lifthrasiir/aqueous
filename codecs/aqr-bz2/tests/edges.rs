//! Edge cases: damaged streams, streams cut as another compressor would, randomised blocks,
//! the bound on data, blocks at the size limit.

use aqr_archive::Evaluate;
use aqr_bz2::formula::{self, MODULE};
use aqr_bz2::{compress, stream};
use aqr_formula::{Apply, Formula};
use std::time::Duration;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 28);

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.bz2", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A formula, where there is one, rebuilds its input.
fn sound(d: &[u8]) {
    let _ = formula::extent(d);
    let _ = stream::decode(d, true, 1 << 24);
    if let Some(f) = formula::encode(d, 1) {
        assert!(eval(&f).as_deref() == Ok(d), "a formula that does not rebuild its input");
    }
}

#[test]
fn damaged_streams() {
    for name in ["small-9", "runs-1", "periodic-1", "empty-9"] {
        let d = fixture(name);
        aqr_testkit::damaged(&d, 200, 64, Duration::from_secs(20), sound);
    }
}

/// Streams of valid syntax that libbzip2 would not write (blocks cut elsewhere, here a level 1's
/// blocks under a level 9 header) are left as they are.
#[test]
fn another_compressor() {
    let data: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let blocks = compress::blocks(&data, 1, 1);
    assert!(blocks.len() > 1);
    let other = compress::stream(9, &blocks);
    assert_eq!(stream::decode(&other, true, usize::MAX).unwrap().data, data);
    assert!(formula::encode(&other, 1).is_none());
    // the same under its own level
    let own = compress::stream(1, &blocks);
    let f = formula::encode(&own, 1).expect("own level");
    assert!(eval(&f).unwrap() == own);
}

/// The randomised bit of a block is set: refused, not misdecoded.
#[test]
fn randomised_block() {
    let mut d = fixture("small-9");
    // 4 bytes of header, 48 bits of magic, 32 of CRC, then the bit
    assert_eq!(d[14] & 0x80, 0);
    d[14] |= 0x80;
    assert!(stream::decode(&d, true, usize::MAX).is_err());
    assert!(formula::extent(&d).is_none());
    assert!(formula::encode(&d, 1).is_none());
}

/// More data than the bound: the stream is not taken (and not decoded in full).
#[test]
fn bound_of_data() {
    let d = fixture("text-5");
    let n = stream::decode(&d, true, usize::MAX).unwrap().data.len();
    assert!(n > 1000);
    assert!(stream::decode(&d, true, n - 1).is_err());
    assert!(stream::decode(&d, true, n).is_ok());
    let _g = formula::MAX_DATA.set_here(n - 1);
    assert!(formula::encode(&d, 1).is_none());
    drop(_g);
    let _g = formula::MAX_DATA.set_here(n);
    assert!(formula::encode(&d, 1).is_some());
}

/// Blocks at the limit of their size: data of one symbol per byte around `block_max`, and a
/// run-heavy block of 900000 bytes.
#[test]
fn blocks_at_the_limit() {
    let max = aqr_bz2::block_max(9);
    let plain = |n: usize| -> Vec<u8> {
        // no run of 4: each byte is one symbol
        (0..n as u32).map(|i| ((i.wrapping_mul(2654435761) >> 11) as u8) & 0x3f | ((i % 3) as u8) << 6).collect()
    };
    for n in [max - 1, max, max + 1, max + 4, 900_000, 2 * max] {
        let mut data = plain(n);
        // break any run of equal bytes
        for i in 1..data.len() {
            if data[i] == data[i - 1] {
                data[i] ^= 1;
            }
        }
        let d = compress::stream(9, &compress::blocks(&data, 9, 2));
        let s = stream::decode(&d, true, usize::MAX).unwrap();
        assert_eq!(s.data, data, "{n}");
        let f = formula::encode(&d, 2).unwrap_or_else(|| panic!("{n}: no formula"));
        assert!(eval(&f).unwrap() == d, "{n}");
    }
    // 900000 bytes in runs of 255: one block
    let runs: Vec<u8> = (0..900_000usize).map(|i| (i / 255) as u8).collect();
    let d = compress::stream(9, &compress::blocks(&runs, 9, 1));
    assert_eq!(stream::decode(&d, true, usize::MAX).unwrap().sizes.len(), 1);
    let f = formula::encode(&d, 1).expect("runs");
    assert!(eval(&f).unwrap() == d);
}
