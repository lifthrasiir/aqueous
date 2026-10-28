use aqr_lzo::lzo1x::{self, Form, Match, Parse, WINDOW, long_fits, short_fits, state};
use aqr_lzo::recons;

/// The raw streams of liblzo2 (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzo1x"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn streams_of_liblzo2() {
    for (name, s) in fixtures() {
        let dec = lzo1x::decode(&s, 1 << 24).unwrap();
        assert_eq!(dec.used, s.len(), "{name}");
        assert!(lzo1x::encode(&dec.data, &dec.parse) == s, "{name}: encoded otherwise");
        let sp = recons::split(&s, 1 << 24).unwrap();
        assert!(sp.data == dec.data, "{name}");
        assert!(recons::parse(&sp.data, &sp.record).unwrap() == dec.parse, "{name}");
        assert!(recons::join(&sp.data, &sp.record).unwrap() == s, "{name}");
        // the other mode of the match finder codes it too
        for mode in [0, recons::SKIP] {
            if let Ok((r, _)) = recons::record(&dec.data, &dec.parse, mode) {
                assert!(recons::join(&sp.data, &r).unwrap() == s, "{name}: mode {mode}");
            }
        }
    }
}

/// A parse of `d` made at random: matches of any valid length, distance and form, first runs of
/// either form.
fn random_parse(d: &[u8], rng: &mut Rng) -> Parse {
    let mut p = Parse { first_short: rng.below(2) == 0, ..Default::default() };
    let (mut pos, mut run) = (0, 0u32);
    while pos < d.len() {
        let mut taken = None;
        if pos > 0 && rng.below(3) == 0 {
            // the nearest of a few candidates at random distances, or a near one
            for _ in 0..64 {
                let dist = if rng.below(2) == 0 { 1 + rng.below(8) } else { 1 + rng.below(WINDOW) };
                if dist > pos {
                    continue;
                }
                let l = recons::lcp(d, pos - dist, pos);
                if l < 2 {
                    continue;
                }
                let n = 2 + rng.below(l - 1);
                let (s, lg) = (short_fits(run, n as u32, dist as u32), long_fits(n as u32, dist as u32));
                let form = match (s, lg) {
                    (true, true) => [Form::Short, Form::Long][rng.below(2)],
                    (true, false) => Form::Short,
                    (false, true) => Form::Long,
                    _ => continue,
                };
                taken = Some(Match { lits: run, len: n as u32, dist: dist as u32, form });
                break;
            }
        }
        match taken {
            Some(m) => {
                p.matches.push(m);
                pos += m.len as usize;
                run = 0;
            }
            None => {
                pos += 1;
                run += 1;
            }
        }
    }
    p.tail = run;
    let first = p.matches.first().map_or(p.tail, |m| m.lits);
    p.first_short = match first {
        0 => false,
        1..=3 => true,
        4..=238 => p.first_short,
        _ => false,
    };
    p
}

#[test]
fn any_parse() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for (name, s) in fixtures().into_iter().filter(|(n, _)| n.ends_with(".1")) {
        let data = lzo1x::decode(&s, 1 << 24).unwrap().data;
        for _ in 0..3 {
            let p = random_parse(&data, &mut rng);
            let stream = lzo1x::encode(&data, &p);
            let dec = lzo1x::decode(&stream, 1 << 24).unwrap();
            assert!(dec.data == data && dec.parse == p, "{name}: decoded otherwise");
            let sp = recons::split(&stream, 1 << 24).unwrap();
            assert!(recons::join(&sp.data, &sp.record).unwrap() == stream, "{name}");
        }
    }
    // M1 after 1-3 literals (length 2) and after 4 or more (length 3, 2049-3072 back), and M4
    let mut d: Vec<u8> = (0..20000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    d.copy_within(10..14, 100);
    d.copy_within(20..22, 106);
    d.copy_within(1000..1003, 3100);
    d.copy_within(1000..1010, 19000);
    let m = |lits, len, dist, form| Match { lits, len, dist, form };
    let parses = [
        vec![m(100, 4, 90, Form::Short), m(2, 2, 86, Form::Short)],
        vec![m(3100, 3, 2100, Form::Short)],
        vec![m(3100, 3, 2100, Form::Long)],
        vec![m(19000, 10, 18000, Form::Long)],
    ];
    for matches in parses {
        let end: u32 = matches.iter().map(|m| m.lits + m.len).sum();
        let p = Parse { matches, tail: d.len() as u32 - end, first_short: false };
        let stream = lzo1x::encode(&d, &p);
        let dec = lzo1x::decode(&stream, 1 << 24).unwrap();
        assert!(dec.data == d && dec.parse == p);
        let sp = recons::split(&stream, 1 << 24).unwrap();
        assert!(recons::join(&sp.data, &sp.record).unwrap() == stream);
    }
    assert_eq!(state(0), 0);
}

#[test]
fn damaged_records() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (name, s) in fixtures().into_iter().filter(|(n, _)| n.ends_with(".999-9") || n.ends_with(".1")) {
        let sp = recons::split(&s, 1 << 24).unwrap();
        for _ in 0..20 {
            let mut r = sp.record.clone();
            if r.len() > 1 {
                let i = 1 + rng.below(r.len() - 1);
                r[i] ^= 1 << rng.below(8);
            }
            // fails or gives some stream, but does not panic
            let _ = recons::join(&sp.data, &r);
            let cut = rng.below(r.len() + 1);
            let _ = recons::join(&sp.data, &r[..cut]);
        }
        let junk: Vec<u8> = (0..64).map(|_| rng.next() as u8).collect();
        let _ = recons::join(&sp.data, &[&[0u8][..], &junk].concat());
        let _ = recons::join(&sp.data, &[&[1u8][..], &junk].concat());
        assert!(recons::join(&sp.data, &[]).is_err(), "{name}");
        assert!(recons::join(&sp.data, &[2]).is_err(), "{name}");
    }
}

#[test]
fn streams_refused() {
    let s = &fixtures().into_iter().find(|(n, _)| n == "records.1").unwrap().1;
    // cut short, an end of another length, data beyond the limit, a match before the data
    assert!(lzo1x::decode(&s[..s.len() - 1], 1 << 24).is_err());
    let mut e = s.clone();
    let n = e.len();
    e[n - 3] = 0x12;
    assert!(lzo1x::decode(&e, 1 << 24).is_err());
    assert!(lzo1x::decode(s, 1000).is_err());
    assert!(lzo1x::decode(&[0x40, 0, 0x11, 0, 0], 1 << 24).is_err());
    // LZO-RLE's version byte
    assert!(lzo1x::decode(&[0x11, 1, 0x11, 0, 0], 1 << 24).is_err());
    assert!(lzo1x::decode(&[], 1 << 24).is_err());
}
