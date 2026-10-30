use aqr_bcj::bra::{self, Arch};
use aqr_bcj::formula::{self, Filter};
use aqr_bcj::{bcj2, delta, module};

/// Bytes of which many are those the filters look for.
fn biased(n: usize, seed: u64) -> Vec<u8> {
    const SPECIAL: [u8; 22] = [
        0xe8, 0xe9, 0x00, 0xff, 0x0f, 0x80, 0x85, 0xeb, 0x94, 0x90, 0x48, 0x40, 0xf0, 0xf8, 0x01, 0x6f, 0x17, 0x97,
        0xef, 0x10, 0x16, 0x05,
    ];
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = (x >> 33) as usize;
            if r % 10 < 6 { SPECIAL[(r >> 8) % SPECIAL.len()] } else { (r >> 16) as u8 }
        })
        .collect()
}

#[test]
fn round_trips() {
    for n in [0, 3, 5, 6, 7, 16, 100, 4097, 100_000] {
        let d = biased(n, n as u64);
        for arch in Arch::ALL {
            for pc in [0u32, 0x1000, 0xdead_bee0] {
                let mut e = d.clone();
                bra::convert(arch, &mut e, pc, true);
                if n >= 4096 {
                    assert!(e != d, "{arch:?} {n}: converts nothing");
                }
                bra::convert(arch, &mut e, pc, false);
                assert!(e == d, "{arch:?} {n} {pc}");
            }
        }
        for dist in [1, 2, 7, 256] {
            let mut e = d.clone();
            delta::encode(&mut e, dist);
            delta::decode(&mut e, dist);
            assert!(e == d, "delta {dist} {n}");
        }
    }
}

#[test]
fn modules() {
    let d = biased(10_000, 3);
    let mut want = d.clone();
    bra::convert(Arch::Arm64, &mut want, 0x4000, true);
    let pc = 0x4000u32.to_le_bytes();
    assert_eq!(module::bra(Arch::Arm64, &pc, &[&d], d.len() as u64).unwrap(), want);
    // a start not aligned, other parameters, another length, two inputs
    assert!(module::bra(Arch::Arm64, &0x4002u32.to_le_bytes(), &[&d], d.len() as u64).is_err());
    assert!(module::bra(Arch::X86, &[1], &[&d], d.len() as u64).is_err());
    assert!(module::bra(Arch::X86, &[], &[&d], d.len() as u64 + 1).is_err());
    assert!(module::bra(Arch::X86, &[], &[&d, &d], d.len() as u64).is_err());
    let mut want = d.clone();
    delta::encode(&mut want, 4);
    assert_eq!(module::delta(&[3], &[&d], d.len() as u64).unwrap(), want);
    assert!(module::delta(&[], &[&d], d.len() as u64).is_err());
    // by the formula's filters, and their modules
    for f in [Filter::Bra(Arch::X86, 0), Filter::Bra(Arch::RiscV, 2), Filter::Delta(3)] {
        let mut e = d.clone();
        f.encode(&mut e);
        assert_eq!(formula::unfilter(&[f], &e).as_deref(), Some(&d[..]));
        let aqr_formula::Node::Apply(a) = f.apply(aqr_formula::Node::slice(0, 0, d.len() as u64), d.len() as u64)
        else {
            panic!()
        };
        assert_eq!(formula::decode(a.module, &a.params, &[&d], a.len).unwrap().unwrap(), e);
    }
}

/// LZMA's range encoder, for BCJ2's bits.
struct RcEnc {
    low: u64,
    range: u32,
    cache: u8,
    pending: u64,
    out: Vec<u8>,
}

impl RcEnc {
    fn shift(&mut self) {
        if (self.low as u32) < 0xff00_0000 || self.low >> 32 != 0 {
            let carry = (self.low >> 32) as u8;
            let mut b = self.cache;
            loop {
                self.out.push(b.wrapping_add(carry));
                b = 0xff;
                self.pending -= 1;
                if self.pending == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.pending += 1;
        self.low = (self.low & 0xff_ffff) << 8;
    }

    fn bit(&mut self, p: &mut u16, b: bool) {
        let bound = (self.range >> 11) * *p as u32;
        if !b {
            self.range = bound;
            *p += (2048 - *p) >> 5;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *p -= *p >> 5;
        }
        while self.range < 1 << 24 {
            self.range <<= 8;
            self.shift();
        }
    }
}

/// A BCJ2 encoder taking out the targets within the data: the streams and the range coder's.
fn bcj2_of(d: &[u8]) -> (bcj2::Streams, Vec<u8>) {
    let mut rc = RcEnc { low: 0, range: u32::MAX, cache: 0, pending: 1, out: Vec::new() };
    let mut probs = [1024u16; 258];
    let mut s = bcj2::Streams::default();
    let (mut i, mut prev) = (0, 0u8);
    while i < d.len() {
        let b = d[i];
        s.main.push(b);
        i += 1;
        let cand = b & 0xfe == 0xe8 || prev == 0x0f && b & 0xf0 == 0x80;
        if !cand {
            prev = b;
            continue;
        }
        let ctx = match b {
            0xe8 => 2 + prev as usize,
            0xe9 => 1,
            _ => 0,
        };
        let abs = d.get(i..i + 4).map(|t| u32::from_le_bytes(t.try_into().unwrap()).wrapping_add(i as u32 + 4));
        let take = abs.is_some_and(|a| (a as usize) < d.len());
        rc.bit(&mut probs[ctx], take);
        if !take {
            prev = b;
            continue;
        }
        (if b == 0xe8 { &mut s.call } else { &mut s.jump }).extend(abs.unwrap().to_be_bytes());
        prev = d[i + 3];
        i += 4;
    }
    (0..5).for_each(|_| rc.shift());
    (s, rc.out)
}

#[test]
fn bcj2_streams() {
    for n in [1, 5, 1000, 200_000] {
        // x86-like: calls and jumps to targets within the data, among other bytes
        let mut d = biased(n, 9);
        let mut x = 1u64;
        for i in (0..n.saturating_sub(5)).step_by(7) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            if x >> 62 == 0 {
                let t = ((x >> 20) as usize % n) as i64 - (i + 5) as i64;
                d[i] = [0xe8, 0xe9][(x >> 40) as usize & 1];
                d[i + 1..i + 5].copy_from_slice(&(t as i32).to_le_bytes());
            }
        }
        let (s, rc) = bcj2_of(&d);
        if n > 1000 {
            assert!(!s.call.is_empty() && !s.jump.is_empty());
        }
        assert_eq!(bcj2::decode(&s, &rc, n).unwrap(), d, "{n}");
        assert_eq!(bcj2::encode(&d, &rc).unwrap(), s, "{n}");
        let joined = [&s.main[..], &s.call, &s.jump].concat();
        assert_eq!(module::bcj2(&[], &[&d, &rc], n as u64).unwrap(), joined);
        // a range coder's stream cut short, of another first byte
        assert!(bcj2::encode(&d, &rc[..4]).is_err());
        let mut bad = rc.clone();
        bad[0] = 1;
        assert!(bcj2::encode(&d, &bad).is_err());
        // streams that do not give the data
        assert!(bcj2::decode(&s, &rc, n + 1).is_err());
    }
}
