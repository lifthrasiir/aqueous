//! WIM files made here: a resource of LZX chunks (aqr-lzx's fixture, of its test encoder), one
//! of no compression, the lookup table and XML data, taken apart and given back.

use aqr_decompress::Native;
use aqr_formula::Formula;
use aqr_wim::formula;

/// The chunks of aqr-lzx's fixture: each its bytes and its data's length.
fn chunks() -> Vec<(Vec<u8>, usize)> {
    let d = std::fs::read(format!("{}/../aqr-lzx/tests/data/greedy.chunks", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (mut out, mut at) = (Vec::new(), 0);
    while at < d.len() {
        let n = u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
        out.push((d[at + 8..at + 8 + len].to_vec(), n));
        at += 8 + len;
    }
    out
}

fn reshdr(size: usize, flags: u8, offset: usize, orig: usize) -> Vec<u8> {
    let mut h = (size as u64).to_le_bytes()[..7].to_vec();
    h.push(flags);
    h.extend_from_slice(&(offset as u64).to_le_bytes());
    h.extend_from_slice(&(orig as u64).to_le_bytes());
    h
}

/// A WIM file of LZX: the chunks as a resource (twice where `twice`, the second a copy at
/// another offset), a resource of no compression, the lookup table, XML data.
fn wim(twice: bool) -> Vec<u8> {
    let cs = chunks();
    let mut d = vec![0u8; 208];
    let mut entries = Vec::new();
    for _ in 0..1 + twice as usize {
        let at = d.len();
        let mut off = 0;
        for (c, _) in &cs[..cs.len() - 1] {
            off += c.len();
            d.extend_from_slice(&(off as u32).to_le_bytes());
        }
        cs.iter().for_each(|(c, _)| d.extend_from_slice(c));
        let orig: usize = cs.iter().map(|c| c.1).sum();
        entries.push(reshdr(d.len() - at, 0x04, at, orig));
    }
    let at = d.len();
    d.extend(b"plain data, stored as it is. ".repeat(100));
    entries.push(reshdr(d.len() - at, 0, at, d.len() - at));
    let lookup = d.len();
    for e in &entries {
        d.extend_from_slice(e);
        d.extend_from_slice(&[1, 0, 1, 0, 0, 0]);
        d.extend_from_slice(&[0x5a; 20]);
    }
    let xml = d.len();
    d.extend("<WIM><IMAGE INDEX=\"1\"></IMAGE></WIM>".encode_utf16().flat_map(u16::to_le_bytes));
    let h = &mut d[..208];
    h[..8].copy_from_slice(b"MSWIM\0\0\0");
    h[8..12].copy_from_slice(&208u32.to_le_bytes());
    h[12..16].copy_from_slice(&0x10d00u32.to_le_bytes());
    h[16..20].copy_from_slice(&0x40082u32.to_le_bytes());
    h[20..24].copy_from_slice(&32768u32.to_le_bytes());
    h[40..44].copy_from_slice(&[1, 0, 1, 0]);
    h[44..48].copy_from_slice(&1u32.to_le_bytes());
    h[48..72].copy_from_slice(&reshdr(xml - lookup, 0, lookup, xml - lookup));
    let n = d.len() - xml;
    d[72..96].copy_from_slice(&reshdr(n, 0, xml, n));
    d
}

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&Native::default()).unwrap()
}

#[test]
fn taken_apart_and_given_back() {
    for twice in [false, true] {
        let d = wim(twice);
        let f = formula::encode(&d, 2).expect("a formula");
        assert!(eval(&f) == d);
        let orig: usize = chunks().iter().map(|c| c.1).sum();
        assert_eq!(f.streams[0].data.len(), orig * (1 + twice as usize) + 2900);
        assert!(f.streams.iter().any(|s| s.class == "lzx/record"));
    }
}

/// A chunk of another size than its place says is not taken: its resource is kept whole,
/// among the bytes between the streams.
#[test]
fn chunks_not_taken_kept() {
    let mut d = wim(false);
    // (the first chunk's offset in the table one byte less: the second chunk starts a byte
    // early, and neither reads whole)
    d[208] -= 1;
    let f = formula::encode(&d, 1).expect("a formula");
    assert!(eval(&f) == d);
    assert_eq!(f.streams[0].data.len(), 2900);
    assert!(f.streams[1].data.len() > chunks().iter().map(|c| c.0.len()).sum::<usize>());
}

/// The formula is applications one after another, each a part of the file as it is, and the
/// resource of no compression whole between them: no output of several taken apart.
#[test]
fn applications_in_order() {
    let d = wim(true);
    let f = formula::encode(&d, 1).unwrap();
    assert!(f.shared.is_empty());
    let aqr_formula::Node::Concat(ns) = &f.root else { panic!("not a concatenation") };
    let kinds: Vec<&str> = ns
        .iter()
        .map(|n| match n {
            aqr_formula::Node::Apply(_) => "apply",
            aqr_formula::Node::Slice { stream: 0, len: 2900, .. } => "resource",
            aqr_formula::Node::Slice { stream: 1, .. } => "rest",
            n => panic!("{n:?}"),
        })
        .collect();
    assert_eq!(kinds, ["apply", "resource", "rest"]);
}

#[test]
fn not_a_wim() {
    assert!(formula::encode(b"MSWIM\0\0\0", 1).is_none());
    assert!(formula::encode(&[0; 300], 1).is_none());
    let mut d = wim(false);
    // XPRESS
    d[16..20].copy_from_slice(&0x20082u32.to_le_bytes());
    assert!(formula::encode(&d, 1).is_none_or(|f| eval(&f) == d));
}

/// Files cut short or with bytes changed: no panic, and a formula, where there is one, gives
/// the input back.
#[test]
fn damaged() {
    let d = wim(true);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for cut in [100, 208, 300, d.len() / 2, d.len() - 10] {
        if let Some(f) = formula::encode(&d[..cut], 1) {
            assert!(eval(&f) == d[..cut], "cut at {cut}");
        }
    }
    let lookup = u64::from_le_bytes(d[56..64].try_into().unwrap()) as usize;
    for _ in 0..100 {
        let mut e = d.clone();
        for _ in 0..1 + next() % 3 {
            // the header, the chunk tables' start, the lookup table
            let at = match next() % 3 {
                0 => (next() % 208) as usize,
                1 => 208 + (next() % 16) as usize,
                _ => lookup + (next() % (d.len() - lookup) as u64) as usize,
            };
            e[at] = next() as u8;
        }
        if let Some(f) = formula::encode(&e, 1) {
            assert!(eval(&f) == e);
        }
    }
}
