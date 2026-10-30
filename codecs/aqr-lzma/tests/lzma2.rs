use aqr_lzma::lzma::Props;
use aqr_lzma::lzma2::{self, Chunk};
use aqr_lzma::{greedy, module};

/// The raw LZMA2 streams of XZ Utils 5.8 (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzma2"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

/// A dictionary no fixture's matches reach past.
const DICT: u32 = 8 << 20;

#[test]
fn streams_of_xz() {
    let mut kinds = [false; 3];
    for (name, s) in fixtures() {
        let dec = lzma2::decode(&s, 1 << 24).unwrap();
        assert_eq!(dec.used, s.len(), "{name}");
        for c in dec.segments.iter().flat_map(|s| &s.chunks) {
            kinds[match c.control {
                1 | 2 => 0,
                0x80 => 1,
                _ => 2,
            }] = true;
        }
        let sp = lzma2::split(&s, DICT, 1 << 24, 2).unwrap();
        assert_eq!(sp.len(), dec.segments.len(), "{name}");
        let mut out = Vec::new();
        for (x, d) in sp.iter().zip(&dec.segments) {
            assert!(x.data == d.data, "{name}");
            let j = lzma2::join(&x.data, &x.record).unwrap_or_else(|| panic!("{name}: not rebuilt"));
            assert_eq!(j.len(), x.used, "{name}");
            out.extend(j);
        }
        assert!(out == s, "{name}: rebuilt otherwise");
    }
    // stored chunks, LZMA chunks going on, and resetting
    assert_eq!(kinds, [true; 3]);
}

/// The data of `n` bytes that greedy parsing codes in several chunks, from `seed`.
fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if d.len() > 64 && x >> 60 < 6 {
            let len = 2 + (x >> 8) as usize % 30;
            let src = (x >> 20) as usize % (d.len() - len.min(d.len()) + 1);
            for j in 0..len {
                d.push(d[src + j]);
            }
        } else {
            d.push(b'a' + (x >> 40) as u8 % 20);
        }
    }
    d.truncate(n);
    d
}

/// Chunks of at most `max` bytes of data over the packets, never splitting one, with these
/// controls after the first.
fn chunks_over(packets: &[aqr_lzma::lzma::Packet], props: Props, max: usize, controls: &[u8]) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut len = 0;
    for pk in packets {
        if len + pk.len as usize > max {
            let i = out.len();
            let control = if i == 0 { 0xE0 } else { controls[(i - 1) % controls.len()] };
            out.push(Chunk { control, len, props: (control >= 0xC0).then_some(props) });
            len = 0;
        }
        len += pk.len as usize;
    }
    let i = out.len();
    let control = if i == 0 { 0xE0 } else { controls[(i - 1) % controls.len()] };
    out.push(Chunk { control, len, props: (control >= 0xC0).then_some(props) });
    out
}

#[test]
fn chunks_of_every_kind() {
    let d = data(100_000, 3);
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let packets = greedy::packets(&d, 1 << 16);
    let chunks = chunks_over(&packets, props, 7000, &[0x80, 0xA0, 0xC0, 0x80]);
    let s = [lzma2::encode(&d, &chunks, &packets).unwrap(), vec![0]].concat();
    let dec = lzma2::decode(&s, 1 << 24).unwrap();
    assert_eq!(dec.segments.len(), 1);
    assert!(dec.segments[0].data == d && dec.segments[0].chunks == chunks && dec.segments[0].packets == packets);
    let sp = lzma2::split(&s, 1 << 16, 1 << 24, 1).unwrap();
    assert!(lzma2::join(&sp[0].data, &sp[0].record).as_deref() == Some(&s[..]));
}

#[test]
fn segments_by_index() {
    // two streams of the fixtures, and segments of another (dictionary resets within it)
    let (mut data, mut records, mut lens, mut out) = (vec![], vec![], vec![], vec![]);
    for (_, s) in fixtures().into_iter().filter(|f| f.0.ends_with("-p6")) {
        for x in lzma2::split(&s, DICT, 1 << 24, 1).unwrap() {
            lens.push((x.data.len(), x.record.len()));
            data.extend(&x.data);
            records.extend(&x.record);
        }
        out.extend(&s);
    }
    let index = module::index(&lens);
    assert!(module::decode2(&[], &[&data, &records, &index], out.len() as u64, 4).unwrap() == out);
    assert!(module::decode2(&[], &[&data, &records, &index], out.len() as u64 + 1, 1).is_err());
    assert!(module::decode2(&[], &[&data[1..], &records, &index], out.len() as u64, 1).is_err());
    // a stream of several segments
    let d = data_of_segments();
    let sp = lzma2::split(&d, 1 << 16, 1 << 24, 3).unwrap();
    assert_eq!(sp.len(), 3);
    let joined: Vec<u8> = sp.iter().flat_map(|x| lzma2::join(&x.data, &x.record).unwrap()).collect();
    assert!(joined == d);
}

/// A stream of three segments: an LZMA chunk, stored data then LZMA chunks, and one more.
fn data_of_segments() -> Vec<u8> {
    let props = Props { lc: 0, lp: 2, pb: 0 };
    let mut s = Vec::new();
    for (i, seed) in [5u64, 6, 7].into_iter().enumerate() {
        let d = data(20_000, seed);
        let packets = greedy::packets(&d, 1 << 16);
        let chunks = chunks_over(&packets, props, 9000, &[0xA0, 0x80]);
        if i == 1 {
            // stored data first: the LZMA chunk after it gives the properties
            let packets = greedy::packets(&d[100..], 1 << 16);
            let mut c = chunks_over(&packets, props, 9000, &[0x80]);
            c[0].control = 0xC0;
            let chunks = [vec![Chunk { control: 1, len: 100, props: None }], c].concat();
            s.extend(lzma2::encode(&d, &chunks, &packets).unwrap());
            continue;
        }
        s.extend(lzma2::encode(&d, &chunks, &packets).unwrap());
    }
    s.push(0);
    s
}

#[test]
fn damaged_records() {
    let mut x = 9u64;
    for (name, s) in fixtures() {
        for sp in lzma2::split(&s, DICT, 1 << 24, 1).unwrap() {
            for _ in 0..20 {
                let mut r = sp.record.clone();
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let at = (x >> 33) as usize % r.len();
                r[at] ^= 1 << (x >> 29 & 7);
                // (fails, or gives some bytes: never panics)
                let _ = lzma2::join(&sp.data, &r);
            }
            assert!(sp.data.is_empty() || lzma2::join(&sp.data[1..], &sp.record).is_none(), "{name}");
        }
    }
}
