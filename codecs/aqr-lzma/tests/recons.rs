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
        let sp = recons::split(s, h.props, h.dict, h.size, 1 << 24, 1).unwrap();
        assert!(sp.data == dec.data && sp.used == s.len(), "{name}");
        assert!(recons::join(&sp.data, &sp.record, 1).as_deref() == Some(s), "{name}: rebuilt otherwise");
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
        let (record, _) = recons::deconstruct(&data, &packets, dict as u32, &[]).unwrap();
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
        let record = recons::split(s, h.props, h.dict, h.size, 1 << 24, 1).unwrap().record;
        for _ in 0..50 {
            let mut r = record.clone();
            if r.is_empty() || rng.below(4) == 0 {
                r.truncate(rng.below(r.len() + 1));
            } else {
                let i = rng.below(r.len());
                r[i] ^= 1 << rng.below(8);
            }
            // Anything but a panic: a failure, or some stream of the data.
            if let Some(out) = recons::join(&dec.data, &r, 1) {
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
        let sp = recons::split(&f[alone::HEADER..], h.props, h.dict, h.size, 1 << 24, 1).unwrap();
        for _ in 0..200 {
            let mut r = sp.record[..recons::HEADER].to_vec();
            r.extend((0..rng.below(64)).map(|_| rng.next() as u8));
            if let Some(out) = recons::join(&sp.data, &r, 1) {
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
    assert!(recons::split(&t, h.props, h.dict, h.size, 1 << 24, 1).is_ok_and(|sp| sp.used == s.len()));
    // Any other flush leaves the decoder's code nonzero: the stream does not decode, so a
    // stream that decodes encodes as it is (`split` checks it all the same).
    for back in 1..=5 {
        let mut t = s.to_vec();
        let i = t.len() - back;
        t[i] ^= 0x10;
        assert!(lzma::decode(&t, h.props, h.size, 1 << 24).is_err(), "{back}");
    }
}

/// Data of `n` bytes, repeating itself at distances near and far.
fn repetitive(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut data = vec![];
    while data.len() < n {
        let k = 1 + rng.below(40);
        if data.len() > 1000 && rng.below(3) > 0 {
            let src = rng.below(data.len() - k);
            data.extend_from_within(src..src + k);
        } else {
            data.extend((0..k).map(|_| rng.below(8) as u8 + b'a'));
        }
    }
    data.truncate(n);
    data
}

/// A record cut into parts: rebuilt whatever the threads (one match finder going on through
/// the parts, or one a part), the same record whatever the threads, and parts at most where the
/// data is no longer than one.
#[test]
fn parts() {
    let data = repetitive(300_000, 17);
    for (props, dict) in [(Props { lc: 3, lp: 0, pb: 2 }, 1 << 16), (Props { lc: 0, lp: 2, pb: 0 }, 1 << 12)] {
        let packets = greedy::packets(&data, dict);
        let s = lzma::encode(&data, props, &packets, false);
        let size = Some(data.len() as u64);
        let whole = recons::split(&s, props, dict as u32, size, 1 << 24, 1).unwrap().record;
        for part in [10_000, 64 << 10, 1 << 20] {
            let sp = recons::split_with(&s, props, dict as u32, size, 1 << 24, part, 1).unwrap();
            assert_eq!(sp.record[5] & 2 != 0, part < data.len(), "{part}");
            if part >= data.len() {
                assert!(sp.record == whole, "{part}");
            }
            for threads in [1, 2, 4] {
                let again = recons::split_with(&s, props, dict as u32, size, 1 << 24, part, threads).unwrap();
                assert!(again.record == sp.record, "{part}, {threads} threads");
                let back = recons::join(&sp.data, &sp.record, threads);
                assert!(back.as_deref() == Some(&s[..]), "{part}, {threads} threads: rebuilt otherwise");
            }
        }
    }
}

/// Damaged parts of a record: anything but a panic.
#[test]
fn damaged_parts() {
    let data = repetitive(100_000, 19);
    let (props, dict) = (Props { lc: 3, lp: 0, pb: 2 }, 1 << 16);
    let packets = greedy::packets(&data, dict);
    let s = lzma::encode(&data, props, &packets, false);
    let sp = recons::split_with(&s, props, dict as u32, Some(data.len() as u64), 1 << 24, 20_000, 1).unwrap();
    let mut rng = Rng(23);
    for _ in 0..300 {
        let mut r = sp.record.clone();
        // (mostly the table of parts, just after the header)
        let span = if rng.below(2) == 0 { 40 } else { r.len() - recons::HEADER };
        let i = recons::HEADER + rng.below(span);
        r[i] ^= 1 << rng.below(8);
        if rng.below(4) == 0 {
            r.truncate(rng.below(r.len() + 1));
        }
        for threads in [1, 3] {
            if let Some(out) = recons::join(&sp.data, &r, threads) {
                let again = lzma::decode(&out, props, Some(data.len() as u64), 1 << 24).unwrap();
                assert!(again.data == data);
            }
        }
    }
}
