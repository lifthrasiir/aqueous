//! FSB5 banks made here: their layout, and the formula of their head and samples giving them
//! back.

use aqr_formula::{Apply, Formula};
use aqr_fsb::formula;
use aqr_fsb::fsb5::{Codec, Layout, extent, sample_sums, starts};

fn eval(f: &Formula) -> Vec<u8> {
    assert!(f.applies().is_empty());
    f.eval(&|_: &Apply, _: &[&[u8]]| unreachable!()).unwrap()
}

/// A bank of version 1 and `mode`, of samples of `sizes` bytes (multiples of 32), the first with
/// a chunk of 4 bytes, and `names` bytes of names.
fn bank(mode: u32, sizes: &[usize], names: usize) -> Vec<u8> {
    let mut headers = Vec::new();
    let mut offset = 0;
    for (i, &n) in sizes.iter().enumerate() {
        let more = (i == 0) as u64;
        let h = more | 8 << 1 | (offset as u64 / 32) << 7 | 1000 << 34;
        headers.extend_from_slice(&h.to_le_bytes());
        if i == 0 {
            // a chunk of type 2 (the frequency), 4 bytes, the last
            headers.extend_from_slice(&(2u32 << 25 | 4 << 1).to_le_bytes());
            headers.extend_from_slice(&44100u32.to_le_bytes());
        }
        offset += n;
    }
    let mut d = b"FSB5".to_vec();
    for v in [1, sizes.len() as u32, headers.len() as u32, names as u32, offset as u32, mode] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.resize(0x3C, 0xab);
    d.extend_from_slice(&headers);
    d.extend((0..names).map(|i| b'a' + (i % 26) as u8));
    for (i, &n) in sizes.iter().enumerate() {
        d.extend((0..n).map(|k| (k * 7 + i * 13) as u8));
    }
    d
}

#[test]
fn layout_of_banks() {
    let d = bank(15, &[64, 96, 32], 20);
    let l = Layout::of(&d).unwrap();
    let head = 0x3C + 8 * 3 + 8 + 20;
    assert_eq!((l.codec, l.head), (Codec::Vorbis, head));
    assert_eq!(l.samples, [head..head + 64, head + 64..head + 160, head + 160..head + 192]);
    assert_eq!(l.headers.iter().map(|h| (h.channels, h.chunks.len())).collect::<Vec<_>>(), [(1, 1), (1, 0), (1, 0)]);
    assert_eq!(l.headers[0].chunk(&d, 2), Some(&44100u32.to_le_bytes()[..]));
    assert_eq!(extent(&d), Some(d.len()));
    assert_eq!(Layout::of(&bank(7, &[32], 0)).unwrap().codec, Codec::ImaAdpcm);
    assert_eq!(Layout::of(&bank(17, &[32], 0)).unwrap().codec, Codec::Other(17));
    // bytes after a bank are not its
    let mut longer = d.clone();
    longer.extend_from_slice(b"trailing");
    assert_eq!(extent(&longer), Some(d.len()));
    // the sums of samples of at least 64 bytes
    assert_eq!(sample_sums(&d, 64).iter().map(|s| s.1).collect::<Vec<_>>(), [64, 96]);
}

#[test]
fn not_banks() {
    let d = bank(15, &[64, 96], 4);
    assert!(starts(&d));
    let corrupt = |at: usize, v: u8| {
        let mut e = d.clone();
        e[at] = v;
        e
    };
    // magic, version, no samples, sample headers' size, data past the end
    assert!(!starts(&corrupt(0, b'G')));
    assert!(!starts(&corrupt(4, 2)));
    assert!(!starts(&corrupt(8, 0)));
    assert!(!starts(&corrupt(12, 30)));
    assert!(!starts(&d[..d.len() - 1]));
    // offsets out of order (the second sample's before the first's end is fine; before 0 is not)
    let mut e = d.clone();
    let p = 0x3C + 8 + 8;
    let h = u64::from_le_bytes(e[p..p + 8].try_into().unwrap()) | ((1 << 27) - 1) << 7;
    e[p..p + 8].copy_from_slice(&h.to_le_bytes());
    assert!(!starts(&e));
    for n in 0..d.len() {
        let _ = Layout::of(&d[..n]);
    }
}

#[test]
fn formula_gives_banks_back() {
    for d in [bank(15, &[64, 96, 32], 20), bank(7, &[32], 0), bank(2, &[32, 0, 64], 3)] {
        let f = formula::encode(&d, 1).unwrap();
        assert_eq!(eval(&f), d);
        let mut longer = d.clone();
        longer.extend_from_slice(b"after");
        assert_eq!(eval(&formula::encode(&longer, 1).unwrap()), longer);
    }
    assert!(formula::encode(b"FSB5", 1).is_none());
}
