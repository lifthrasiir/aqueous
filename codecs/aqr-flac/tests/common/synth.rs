//! FLAC files written synthetically, frame by frame: headers of any codes, subframes of each
//! type (tests/edge.rs; aqr-decompress's tests/alike_misc.rs takes them too).

#![allow(dead_code)]

use aqr_flac::flac::{crc8, crc16};

#[derive(Default)]
pub struct Bw {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bw {
    pub fn put(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            self.acc = self.acc << 1 | (v >> i) & 1;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    pub fn signed(&mut self, v: i64, n: u32) {
        if n > 0 {
            self.put(v as u64 & (u64::MAX >> (64 - n)), n);
        }
    }
    pub fn align(&mut self) {
        while self.n != 0 {
            self.put(0, 1);
        }
    }
}

#[derive(Clone)]
pub enum Sub {
    Constant(i64),
    Verbatim(Vec<i64>),
    /// Fixed order, or LPC when `lpc` is `Some((precision, shift))`.
    Pred {
        order: usize,
        lpc: Option<(u32, i32)>,
        method: u32,
        porder: u32,
        escape: Option<u32>,
        k: u32,
        wasted: u32,
    },
}

impl Sub {
    pub fn fixed(order: usize, porder: u32) -> Sub {
        Sub::Pred { order, lpc: None, method: 0, porder, escape: None, k: 3, wasted: 0 }
    }
}

#[derive(Clone)]
pub struct Hdr {
    pub variable: bool,
    pub number: u64,
    pub bs_code: u8,
    pub bs_extra: u32,
    pub sr_code: u8,
    pub sr_extra: u32,
    pub assign: u8,
    pub ss_code: u8,
}

impl Hdr {
    pub fn new(bs_code: u8, assign: u8, ss_code: u8) -> Hdr {
        Hdr { variable: false, number: 0, bs_code, bs_extra: 0, sr_code: 9, sr_extra: 0, assign, ss_code }
    }
}

pub fn block_of(h: &Hdr) -> usize {
    match h.bs_code {
        1 => 192,
        2..=5 => 576 << (h.bs_code - 2),
        6 | 7 => h.bs_extra as usize + 1,
        c => 256 << (c - 8),
    }
}

pub fn bps_of(h: &Hdr, si_bps: u32) -> u32 {
    [si_bps, 8, 12, 0, 16, 20, 24, 32][h.ss_code as usize]
}

/// A frame of subframes (one for each channel, the header's block size of samples).
pub fn frame(h: &Hdr, si_bps: u32, subs: &[Sub], seed: u32) -> Vec<u8> {
    let mut w = Bw::default();
    w.put(0xfff8 | h.variable as u64, 16);
    w.put((h.bs_code as u64) << 4 | h.sr_code as u64, 8);
    w.put((h.assign as u64) << 4 | (h.ss_code as u64) << 1, 8);
    // coded number
    let v = h.number;
    if v < 0x80 {
        w.put(v, 8);
    } else {
        let mut n = 2;
        while n < 7 && v >= 1u64 << (5 * n + 1) {
            n += 1;
        }
        let lead = if n == 7 { 0xfe } else { (0xff00u16 >> n) as u8 };
        w.put(lead as u64 | v >> (6 * (n - 1)), 8);
        for i in (0..n - 1).rev() {
            w.put(0x80 | (v >> (6 * i)) & 0x3f, 8);
        }
    }
    match h.bs_code {
        6 => w.put(h.bs_extra as u64, 8),
        7 => w.put(h.bs_extra as u64, 16),
        _ => {}
    }
    match h.sr_code {
        12 => w.put(h.sr_extra as u64, 8),
        13 | 14 => w.put(h.sr_extra as u64, 16),
        _ => {}
    }
    let c = crc8(&w.out);
    w.put(c as u64, 8);
    let block = block_of(h);
    let bps = bps_of(h, si_bps);
    let mut s = seed;
    let mut rnd = move |m: i64| {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        (s >> 16) as i64 % (2 * m + 1) - m
    };
    for (ch, sub) in subs.iter().enumerate() {
        let side = matches!((h.assign, ch), (8, 1) | (9, 0) | (10, 1));
        let full = bps + side as u32;
        let big = |m: i64| m.min((1i64 << (full.clamp(2, 40) - 1)) - 1);
        match sub {
            Sub::Constant(v) => {
                w.put(0, 8);
                w.signed(*v, full);
            }
            Sub::Verbatim(vs) => {
                w.put(1 << 1, 8);
                for &v in vs {
                    w.signed(v, full);
                }
            }
            Sub::Pred { order, lpc, method, porder, escape, k, wasted } => {
                let typ = if lpc.is_some() { 32 + order - 1 } else { 8 + order } as u64;
                w.put(typ << 1 | (*wasted > 0) as u64, 8);
                if *wasted > 0 {
                    w.put(1, *wasted);
                }
                let bits = full.saturating_sub(*wasted);
                for _ in 0..*order {
                    w.signed(rnd(big(1000)), bits);
                }
                if let Some((p, sh)) = lpc {
                    w.put(*p as u64 - 1, 4);
                    w.signed(*sh as i64, 5);
                    for _ in 0..*order {
                        w.signed(rnd(1 << (p - 1).min(8)), *p);
                    }
                }
                w.put(*method as u64, 2);
                w.put(*porder as u64, 4);
                let np = 1usize << porder;
                let pbits = 4 + method;
                for i in 0..np {
                    let n = (block / np).saturating_sub(if i == 0 { *order } else { 0 });
                    match escape {
                        Some(nb) => {
                            w.put((1 << pbits) - 1, pbits);
                            w.put(*nb as u64, 5);
                            for _ in 0..n {
                                w.signed(rnd((1i64 << nb.saturating_sub(1)) - 1), *nb);
                            }
                        }
                        None => {
                            w.put(*k as u64, pbits);
                            for _ in 0..n {
                                let r = rnd(40);
                                let u = ((r << 1) ^ (r >> 63)) as u64;
                                for _ in 0..u >> k {
                                    w.put(0, 1);
                                }
                                w.put(1, 1);
                                w.put(u & ((1 << k) - 1), *k);
                            }
                        }
                    }
                }
            }
        }
    }
    w.align();
    let mut out = w.out;
    let c = crc16(&out);
    out.extend_from_slice(&c.to_be_bytes());
    out
}

pub fn streaminfo(block: usize, rate: u32, channels: u32, bps: u32, total: u64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(block as u16).to_be_bytes());
    b.extend_from_slice(&(block as u16).to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    let x = (rate as u64) << 44 | ((channels - 1) as u64) << 41 | ((bps - 1) as u64) << 36 | total;
    b.extend_from_slice(&x.to_be_bytes());
    b.extend_from_slice(&[7; 16]);
    b
}

pub fn meta(blocks: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut o = b"fLaC".to_vec();
    for (i, (t, b)) in blocks.iter().enumerate() {
        o.push(*t | if i + 1 == blocks.len() { 0x80 } else { 0 });
        o.extend_from_slice(&(b.len() as u32).to_be_bytes()[1..]);
        o.extend_from_slice(b);
    }
    o
}

/// A file of `n` frames of the header and subframes, its STREAMINFO of `si_bps` and `channels`.
pub fn file(h: &Hdr, si_bps: u32, channels: u32, subs: &[Sub], n: usize) -> Vec<u8> {
    let mut o = meta(&[(0, streaminfo(block_of(h), 44100, channels, si_bps.clamp(1, 32), 0))]);
    for i in 0..n {
        let mut h = h.clone();
        h.number = if h.variable { h.number + (i * block_of(&h)) as u64 } else { i as u64 };
        o.extend(frame(&h, si_bps, subs, i as u32 + 1));
    }
    o
}
