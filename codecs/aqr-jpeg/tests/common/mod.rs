//! The synthetic JPEGs of the tests: headers made by hand, the data by the encoder from fixed coefficients.
#![allow(dead_code)]

use aqr_jpeg::jpeg::{self, Coefs, ScanInfo};
use aqr_jpeg::progressive;

pub fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

pub struct ScanSpec {
    pub comps: Vec<usize>,
    pub ss: u8,
    pub se: u8,
    pub ah: u8,
    pub al: u8,
    /// a Huffman table of other codes and a restart interval of this (0: as before) before the scan
    pub redefine: Option<u16>,
}

pub fn scan(comps: &[usize], ss: u8, se: u8, ah: u8, al: u8) -> ScanSpec {
    ScanSpec { comps: comps.to_vec(), ss, se, ah, al, redefine: None }
}

pub struct Spec {
    pub w: u16,
    pub h: u16,
    /// (id, h, v, quantisation table)
    pub comps: Vec<(u8, u8, u8, u8)>,
    pub scans: Vec<ScanSpec>,
    pub progressive: bool,
    pub ri: u16,
    pub dqt16: bool,
}

/// Table `alt` of DC codes of 4 bits (5 where alt) for the sizes 0..=11, and AC codes of 8 bits
/// (9 where alt) for EOB, ZRL and the runs of sizes 1..=10.
pub fn dht(out: &mut Vec<u8>, alt: bool, prog: bool) {
    let mut dc = vec![0x00];
    let mut counts = [0u8; 16];
    counts[if alt { 4 } else { 3 }] = 12;
    dc.extend_from_slice(&counts);
    dc.extend(0..12u8);
    let mut ac = vec![0x10];
    let mut counts = [0u8; 16];
    counts[if alt { 8 } else { 7 }] = if prog { 176 } else { 162 };
    ac.extend_from_slice(&counts);
    ac.push(0x00);
    ac.push(0xF0);
    // (the end-of-band runs of progressive scans)
    if prog {
        ac.extend((1..15u8).map(|r| r << 4));
    }
    ac.extend((0..16u8).flat_map(|r| (1..=10u8).map(move |s| r << 4 | s)));
    let mut body = dc;
    body.extend(ac);
    segment(out, 0xC4, &body);
}

pub fn coefficients(j: &jpeg::Jpeg, seed: u64) -> Vec<Coefs> {
    let mut x = seed | 1;
    j.comps
        .iter()
        .map(|c| {
            let n = c.bw * c.bh;
            let mut o = Coefs { bw: c.bw, bh: c.bh, y0: 0, blocks: vec![[0; 64]; n], masks: vec![0; n] };
            for i in 0..n {
                let b = &mut o.blocks[i];
                b[0] = ((i % c.bw) as i16 * 2 + (i / c.bw) as i16) - 40;
                for _ in 0..2 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    b[1 + (x % 6) as usize] = ((x >> 20) % 3) as i16 - 1;
                }
                o.masks[i] = b.iter().enumerate().fold(0, |m, (k, &v)| m | ((v != 0) as u64) << k);
            }
            o
        })
        .collect()
}

/// The JPEG of `s`: the headers, then the data of each scan made of coefficients by the encoder.
pub fn build(s: &Spec) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    for t in 0..4u8 {
        let mut dqt = vec![(s.dqt16 as u8) << 4 | t];
        for k in 0..64u16 {
            let q = 2 + k / 4 + t as u16 * 3 + if s.dqt16 { 300 } else { 0 };
            if s.dqt16 {
                dqt.extend_from_slice(&q.to_be_bytes());
            } else {
                dqt.push(q as u8);
            }
        }
        segment(&mut d, 0xDB, &dqt);
    }
    let mut sof = vec![8];
    sof.extend_from_slice(&s.h.to_be_bytes());
    sof.extend_from_slice(&s.w.to_be_bytes());
    sof.push(s.comps.len() as u8);
    for &(id, h, v, q) in &s.comps {
        sof.extend_from_slice(&[id, h << 4 | v, q]);
    }
    segment(&mut d, if s.progressive { 0xC2 } else { 0xC0 }, &sof);
    dht(&mut d, false, s.progressive);
    if s.ri > 0 {
        segment(&mut d, 0xDD, &s.ri.to_be_bytes());
    }
    for sc in &s.scans {
        if let Some(ri) = sc.redefine {
            dht(&mut d, true, s.progressive);
            if ri > 0 {
                segment(&mut d, 0xDD, &ri.to_be_bytes());
            }
        }
        let mut sos = vec![sc.comps.len() as u8];
        for &c in &sc.comps {
            sos.extend_from_slice(&[s.comps[c].0, 0x00]);
        }
        sos.extend_from_slice(&[sc.ss, sc.se, sc.ah << 4 | sc.al]);
        segment(&mut d, 0xDA, &sos);
    }
    // (the scans are empty here: the stream is only the headers, which parse as they are)
    d.extend_from_slice(&[0xFF, 0xD9]);
    let j = jpeg::parse(&d).unwrap();
    assert_eq!(j.scans.len(), s.scans.len());
    let coefs = coefficients(&j, 0x1234_5678 + s.w as u64);
    let mut out = d.clone();
    for sc in j.scans.iter().rev() {
        let mut data = Vec::new();
        let info = ScanInfo { standard: true, ..Default::default() };
        if j.progressive {
            progressive::encode_scan(&j, sc, &info, &coefs, &mut data).unwrap();
        } else {
            jpeg::encode_scan(&j, sc, &info, &coefs, &mut data).unwrap();
        }
        out.splice(sc.start..sc.start, data);
    }
    out
}

pub fn sequential(w: u16, h: u16, comps: &[(u8, u8, u8, u8)]) -> Spec {
    let all: Vec<usize> = (0..comps.len()).collect();
    Spec { w, h, comps: comps.to_vec(), scans: vec![scan(&all, 0, 63, 0, 0)], progressive: false, ri: 0, dqt16: false }
}

pub const YCC: [(u8, u8, u8, u8); 3] = [(1, 2, 2, 0), (2, 1, 1, 1), (3, 1, 1, 1)];

pub fn sampling(y: (u8, u8)) -> Vec<(u8, u8, u8, u8)> {
    vec![(1, y.0, y.1, 0), (2, 1, 1, 1), (3, 1, 1, 1)]
}
