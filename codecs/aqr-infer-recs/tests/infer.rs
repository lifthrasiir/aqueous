//! Files of known layouts (as `scripts/rec_synth.py` makes): the layout is found, and every
//! region taken restores.

use aqr_formula::{Formula, Node};
use aqr_infer_recs::formula::{self, ENTRIES, FIELDS, FLOATS};
use aqr_infer_recs::{Choice, Shape, apply, candidates, choose, infer, zstd_size};

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
    let f = formula::formula(data, choices);
    assert_eq!(eval(&f, &f.root), data);
}

/// The output of node n of formula f, by the modules' code.
fn eval(f: &Formula, n: &Node) -> Vec<u8> {
    match n {
        Node::Slice { stream, start, len } => f.streams[*stream].data[*start as usize..(start + len) as usize].to_vec(),
        Node::Concat(parts) => parts.iter().flat_map(|p| eval(f, p)).collect(),
        Node::Apply(a) => {
            let inputs: Vec<Vec<u8>> = a.inputs.iter().map(|i| eval(f, i)).collect();
            let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
            match a.module {
                m if m == FIELDS => formula::decode_fields(&a.params, &inputs, a.len).unwrap(),
                m if m == ENTRIES => formula::decode_entries(&a.params, &inputs, a.len).unwrap(),
                m if m == FLOATS => formula::decode_floats(&a.params, &inputs, a.len).unwrap(),
                m => panic!("{m:?}"),
            }
        }
        n => panic!("{n:?}"),
    }
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
    let choices = choose(&data, candidates(&data, true)).0;
    check(&data, &choices);
    let c = choices.iter().max_by_key(|c| c.region.end - c.region.start).unwrap();
    let Shape::Entries { rule, head, .. } = &c.region.shape else { panic!("{}", c.region.label()) };
    assert_eq!((c.region.start, c.region.count, *head), (24, n, 16), "{}", c.region.describe());
    assert!(rule.d == 8 || rule.d == 12, "{}", c.region.describe());
}

/// A catalogue: a 28-byte header, then 40000 stars of 28 bytes (right ascension and declination
/// in 1 ms of time and 0.01″ as f64 radians, each by a constant of its own, a spectral type,
/// a magnitude in 0.01, and proper motions in 0.0001 s and 0.001″ a year as f32 radians), in
/// zones of declination, each sorted by right ascension, as the SAO catalogue's binary is.
#[test]
fn catalogue() {
    let mut rng = Rng(3);
    let n = 40000u64;
    let mut data: Vec<u8> = Vec::new();
    for v in [0u32, 1, n as u32, 0, 1, 1, 28] {
        data.extend(v.to_le_bytes());
    }
    let types: [&[u8; 2]; 6] = [b"A0", b"F2", b"K0", b"G5", b"  ", b"B9"];
    for zone in 0..8u64 {
        let mut ras: Vec<u64> = (0..n / 8).map(|_| rng.below(86_400_000)).collect();
        ras.sort_unstable();
        for ra in ras {
            let dec = (zone as i64 * 10 - 40) * 360_000 + rng.below(3_600_000) as i64;
            data.extend((ra as f64 / 3_600_000.0 * 15.0 * 0.0174532925199433).to_le_bytes());
            data.extend((dec as f64 * (std::f64::consts::PI / 64_800_000.0)).to_le_bytes());
            data.extend(types[rng.below(6) as usize]);
            data.extend((600 + rng.below(400) as i16).to_le_bytes());
            let pm = |rng: &mut Rng, unit: f64| ((rng.below(2000) as f64 - 1000.0) * unit) as f32;
            data.extend(pm(&mut rng, std::f64::consts::PI / 432_000_000.0).to_le_bytes());
            data.extend(pm(&mut rng, std::f64::consts::PI / 648_000_000.0).to_le_bytes());
        }
    }
    let choices = infer(&data);
    check(&data, &choices);
    let r = &choices.iter().max_by_key(|c| c.region.end - c.region.start).unwrap().region;
    assert_eq!((r.start, r.count), (28, n as usize), "{} {}", r.label(), r.describe());
    assert_eq!(r.floats.len(), 4, "{}", r.describe());
    // the floats lifted save most of what is left
    let before = zstd_size(&data);
    let after = zstd_size(&apply(&data, &choices));
    assert!(after * 2 < before, "{before} -> {after}: {}", r.describe());
}
