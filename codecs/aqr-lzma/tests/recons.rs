use aqr_lzma::lzma::{self, Props};
use aqr_lzma::{alone, greedy, recons};

/// The fixtures of XZ Utils 5.8 (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzma"))
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
fn streams_of_xz() {
    for (name, f) in fixtures() {
        let h = alone::parse(&f).unwrap();
        let s = &f[alone::HEADER..];
        let dec = lzma::decode(s, h.props, h.size, 1 << 24).unwrap();
        assert_eq!(dec.used, s.len(), "{name}");
        assert!(lzma::encode(&dec.data, h.props, &dec.packets, dec.marker) == s, "{name}: encoded otherwise");
        let sp = recons::split(s, h.props, h.dict, h.size, 1 << 24).unwrap();
        assert!(sp.data == dec.data && sp.used == s.len(), "{name}");
        assert!(recons::join(&sp.data, &sp.record).as_deref() == Some(s), "{name}: rebuilt otherwise");
    }
}

#[test]
fn streams_of_known_size() {
    let mut rng = Rng(7);
    let mut data = vec![];
    while data.len() < 200_000 {
        let n = 1 + rng.below(40);
        if data.len() > 1000 && rng.below(3) > 0 {
            let src = rng.below(data.len() - n);
            data.extend_from_within(src..src + n);
        } else {
            data.extend((0..n).map(|_| rng.below(8) as u8 + b'a'));
        }
    }
    for (props, dict) in [
        (Props { lc: 3, lp: 0, pb: 2 }, 1 << 16),
        (Props { lc: 0, lp: 4, pb: 4 }, 1 << 12),
        (Props { lc: 8, lp: 0, pb: 0 }, 1 << 20),
    ] {
        let packets = greedy::packets(&data, dict);
        let s = lzma::encode(&data, props, &packets, false);
        let dec = lzma::decode(&s, props, Some(data.len() as u64), 1 << 24).unwrap();
        assert!(dec.data == data && dec.packets == packets && !dec.marker, "{props:?}");
        let (record, _) = recons::deconstruct(&data, &packets, dict as u32).unwrap();
        assert!(recons::rebuild(&data, &record, props, dict as u32, false).as_deref() == Some(&s[..]), "{props:?}");
    }
}

#[test]
fn damaged_records_and_streams() {
    let mut rng = Rng(11);
    for (name, f) in fixtures() {
        let h = alone::parse(&f).unwrap();
        let s = &f[alone::HEADER..];
        let dec = lzma::decode(s, h.props, h.size, 1 << 24).unwrap();
        let record = recons::split(s, h.props, h.dict, h.size, 1 << 24).unwrap().record;
        for _ in 0..50 {
            let mut r = record.clone();
            if r.is_empty() || rng.below(4) == 0 {
                r.truncate(rng.below(r.len() + 1));
            } else {
                let i = rng.below(r.len());
                r[i] ^= 1 << rng.below(8);
            }
            // Anything but a panic: a failure, or some stream of the data.
            if let Some(out) = recons::join(&dec.data, &r) {
                let props = lzma::Props::from_byte(r[0]).unwrap();
                let size = if r[5] != 0 { None } else { Some(dec.data.len() as u64) };
                let again = lzma::decode(&out, props, size, 1 << 24).expect(&name);
                assert!(again.data == dec.data, "{name}");
            }
            let mut t = s.to_vec();
            if !t.is_empty() {
                let i = rng.below(t.len());
                t[i] ^= 1 << rng.below(8);
            }
            let _ = lzma::decode(&t, h.props, h.size, 1 << 20);
        }
    }
}

#[test]
fn random_records() {
    let mut rng = Rng(13);
    for (name, f) in fixtures() {
        let h = alone::parse(&f).unwrap();
        let sp = recons::split(&f[alone::HEADER..], h.props, h.dict, h.size, 1 << 24).unwrap();
        for _ in 0..200 {
            let mut r = sp.record[..recons::HEADER].to_vec();
            r.extend((0..rng.below(64)).map(|_| rng.next() as u8));
            if let Some(out) = recons::join(&sp.data, &r) {
                let size = if r[5] != 0 { None } else { Some(sp.data.len() as u64) };
                assert!(lzma::decode(&out, h.props, size, 1 << 24).expect(&name).data == sp.data, "{name}");
            }
        }
    }
}

#[test]
fn stream_ended_otherwise() {
    let (_, f) = fixtures().into_iter().find(|(n, _)| n == "text-p6").unwrap();
    let h = alone::parse(&f).unwrap();
    let s = &f[alone::HEADER..];
    // A flush of one byte more: the decoder reads the same, the encoder writes the canonical one.
    let mut t = s.to_vec();
    t.push(0);
    let dec = lzma::decode(&t, h.props, h.size, 1 << 24).unwrap();
    assert_eq!(dec.used, s.len());
    assert!(recons::split(&t, h.props, h.dict, h.size, 1 << 24).is_ok_and(|sp| sp.used == s.len()));
    // Any other flush leaves the decoder's code nonzero: the stream does not decode, so a
    // stream that decodes encodes as it is (`split` checks it all the same).
    for back in 1..=5 {
        let mut t = s.to_vec();
        let i = t.len() - back;
        t[i] ^= 0x10;
        assert!(lzma::decode(&t, h.props, h.size, 1 << 24).is_err(), "{back}");
    }
}
