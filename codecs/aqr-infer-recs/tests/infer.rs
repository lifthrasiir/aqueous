//! Files of known layouts (as `scripts/rec_synth.py` makes): the layout is found, and every
//! region taken restores.

use aqr_infer_recs::{Choice, Shape, apply, infer, zstd_size};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn check(data: &[u8], choices: &[Choice]) {
    for c in choices {
        let r = &c.region;
        let back = r.restore(c.form, &r.render(data, c.form));
        assert_eq!(back.as_deref(), Some(&data[r.start..r.end]), "{} does not restore", r.label());
    }
    assert!(zstd_size(&apply(data, choices)) < zstd_size(data));
}

/// A 316-byte header, 30000 records of 20 bytes (u32 counting up, u64 time in ms rising by
/// 900–1100, f32 random walk, u16 of four values, u16 zero) and a footer of 68 bytes.
#[test]
fn records() {
    let mut rng = Rng(1);
    let n = 30000;
    let mut data: Vec<u8> = b"SYNT".to_vec();
    for v in [1u32, n as u32, 20] {
        data.extend(v.to_le_bytes());
    }
    data.extend((0..300).map(|_| rng.next() as u8));
    let (mut ts, mut v) = (1_700_000_000_000u64, 0f32);
    for i in 0..n {
        ts += 900 + rng.below(200);
        v += (rng.below(2001) as f32 - 1000.0) / 1000.0;
        data.extend((1000 + i as u32).to_le_bytes());
        data.extend(ts.to_le_bytes());
        data.extend(v.to_le_bytes());
        data.extend([1u16, 2, 7, 300][rng.below(4) as usize].to_le_bytes());
        data.extend([0, 0]);
    }
    data.extend(b"END!");
    data.extend([0; 64]);
    let choices = infer(&data);
    check(&data, &choices);
    let r = &choices.iter().max_by_key(|c| c.region.end - c.region.start).unwrap().region;
    assert_eq!((r.start, r.count, &r.shape), (316, n, &Shape::Fixed { k: 20 }), "{} {}", r.label(), r.describe());
}

/// A pcap file: 24 bytes, then packets of a 16-byte header (seconds, microseconds, captured
/// and original length) and 60–1514 bytes of payload.
#[test]
fn packets() {
    let mut rng = Rng(2);
    let mut data: Vec<u8> = Vec::new();
    for v in [0xa1b2c3d4u32, 0x0004_0002, 0, 0, 65535, 1] {
        data.extend(v.to_le_bytes());
    }
    data.truncate(24);
    let words: Vec<&[u8]> =
        vec![b"GET ", b"/index.html ", b"HTTP/1.1\r\n", b"Host: example ", b"\x00\x01", b"Accept: */*\r\n"];
    let (mut sec, mut usec) = (1_700_000_000u32, 0u32);
    let n = 4000;
    for _ in 0..n {
        let len = [60, 64, 1514, 60 + rng.below(1454) as usize][rng.below(4) as usize];
        usec += rng.below(4000) as u32;
        if usec >= 1_000_000 {
            (sec, usec) = (sec + 1, usec - 1_000_000);
        }
        for v in [sec, usec, len as u32, len as u32] {
            data.extend(v.to_le_bytes());
        }
        let mut p = Vec::new();
        while p.len() < len {
            p.extend_from_slice(words[rng.below(words.len() as u64) as usize]);
            p.push(rng.next() as u8);
        }
        data.extend(&p[..len]);
    }
    let choices = infer(&data);
    check(&data, &choices);
    let c = choices.iter().max_by_key(|c| c.region.end - c.region.start).unwrap();
    let Shape::Entries { rule, head, .. } = &c.region.shape else { panic!("{}", c.region.label()) };
    assert_eq!((c.region.start, c.region.count, *head), (24, n, 16), "{}", c.region.describe());
    assert!(rule.d == 8 || rule.d == 12, "{}", c.region.describe());
}
