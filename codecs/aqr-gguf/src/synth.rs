//! Made-up GGUF files for the tests (here and aqr-decompress's): tensors of every type this crate
//! codes, of weights alike to a model's (a bell of values), with the corners its formulas take:
//! floats of exponents past 16 (escapes), F16 of BF16's precision and F16 of another in part,
//! tensors too small to code, a type not coded, a long run of it, padding.

use aqr_common::rng::Rng;

use crate::layout::Quant;

/// A value of about a bell (the sum of four uniform ones), in -1 to 1.
fn bell(r: &mut Rng) -> f64 {
    (0..4).map(|_| r.below(1 << 20) as f64 / (1 << 19) as f64 - 1.0).sum::<f64>() / 4.0
}

/// A tensor: its ggml type, its elements, and its data.
struct Tensor {
    ty: u32,
    n: u64,
    data: Vec<u8>,
}

fn f32_to_bf16(v: f32) -> u16 {
    (v.to_bits() >> 16) as u16
}

/// F16 of `v` (rounded toward zero; no subnormals: those are zero).
fn f32_to_f16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = (b >> 16) & 0x8000;
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    if e <= 0 {
        return sign as u16;
    }
    if e >= 31 {
        return (sign | 0x7bff) as u16;
    }
    (sign | (e as u32) << 10 | (b >> 13) & 0x3ff) as u16
}

/// `n` weights of scale `scale`, with one in `wide` of them far off (0 for none).
fn weights(r: &mut Rng, n: usize, scale: f64, wide: u64) -> Vec<f32> {
    (0..n)
        .map(|_| {
            let v = bell(r) * scale;
            (if wide > 0 && r.below(wide) == 0 { v * (1 << r.below(30)) as f64 / 64.0 } else { v }) as f32
        })
        .collect()
}

/// The blocks of `q` of `blocks` blocks: their side bytes noise of a few values, their quants a
/// bell.
fn blocks(r: &mut Rng, q: Quant, blocks: usize) -> Vec<u8> {
    let mut d = Vec::with_capacity(blocks * q.block());
    let nib = |r: &mut Rng| ((bell(r) * 7.5 + 8.0) as i32).clamp(0, 15) as u8;
    for _ in 0..blocks {
        for p in q.parts() {
            match *p {
                crate::layout::Part::Side(n) => {
                    d.extend((0..n).map(|i| if i % 2 == 1 { 0x2c } else { r.below(256) as u8 }))
                }
                crate::layout::Part::Nib(n) => d.extend((0..n).map(|_| nib(r) | nib(r) << 4)),
                crate::layout::Part::Crumb(n) => d.extend(
                    (0..n).map(|_| (0..4).fold(0u8, |a, k| a | ((bell(r) * 1.5 + 2.0) as u8).min(3) << (2 * k))),
                ),
                crate::layout::Part::HiNib(n) => d.extend((0..n).map(|_| ((bell(r) * 127.0) as i8) as u8)),
                // (the parts of fifth bits and nibbles of quants of a bell, as they are)
                p @ (crate::layout::Part::Five32 | crate::layout::Part::Five256) => {
                    let quants = if p == crate::layout::Part::Five32 { 32 } else { 256 };
                    let v: Vec<u8> = (0..quants).map(|_| ((bell(r) * 15.5 + 16.0) as i32).clamp(0, 31) as u8).collect();
                    let mut b = vec![0u8; p.bytes()];
                    let (h, qs) = b.split_at_mut(if quants == 32 { 4 } else { 32 });
                    for (j, &x) in v.iter().enumerate() {
                        if quants == 32 {
                            qs[j % 16] |= (x & 15) << (4 * (j / 16));
                            h[j / 8] |= (x >> 4) << (j % 8);
                        } else {
                            let (k, l, hi) = (j / 64, j % 32, j % 64 >= 32);
                            qs[32 * k + l] |= (x & 15) << (4 * hi as usize);
                            h[l] |= (x >> 4) << (2 * k + hi as usize);
                        }
                    }
                    d.extend_from_slice(&b);
                }
            }
        }
    }
    d
}

/// A GGUF file of seed `seed`, its tensors of `scale` times a few thousand elements each.
pub fn file(seed: u64, scale: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut ts: Vec<Tensor> = Vec::new();
    let n = 4096 * scale;
    let float = |r: &mut Rng, ty: u32, n: usize, wide: u64| -> Tensor {
        let w = weights(r, n, 0.05, wide);
        let data: Vec<u8> = match ty {
            0 => w.iter().flat_map(|v| v.to_le_bytes()).collect(),
            1 => w.iter().flat_map(|&v| f32_to_f16(v).to_le_bytes()).collect(),
            _ => w.iter().flat_map(|&v| f32_to_bf16(v).to_le_bytes()).collect(),
        };
        Tensor { ty, n: n as u64, data }
    };
    ts.push(float(&mut r, 30, n * 3 + 17, 0));
    ts.push(float(&mut r, 30, n, 50));
    ts.push(float(&mut r, 0, n + 5, 0));
    ts.push(float(&mut r, 1, n * 2, 0));
    // (F16 of BF16's precision: the low 3 bits of every mantissa zero; and in part)
    let mut f = float(&mut r, 1, n * 2 + 31, 0);
    for (k, c) in f.data.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let v = u16::from_le_bytes(*c);
        *c = if k % 97 == 0 { v } else { v & !7 }.to_le_bytes();
    }
    ts.push(f);
    let mut f = float(&mut r, 1, n, 0);
    for c in f.data.as_chunks_mut::<2>().0 {
        *c = (u16::from_le_bytes(*c) & !7).to_le_bytes();
    }
    ts.push(f);
    // (too small to code)
    ts.push(float(&mut r, 0, 100, 0));
    for (k, q) in Quant::ALL.into_iter().enumerate() {
        let t = [2, 3, 8, 12, 14, 20, 23, 39, 6, 7, 13][k];
        let b = (n * (k + 2)).div_ceil(q.weights());
        ts.push(Tensor { ty: t, n: (b * q.weights()) as u64, data: blocks(&mut r, q, b) });
    }
    // (a type not coded, short and long: Q2_K's bytes of noise)
    ts.push(Tensor { ty: 10, n: 512, data: (0..2 * 84).map(|_| r.below(256) as u8).collect() });
    if scale >= 8 {
        let b = (1 << 20) / 84 + 3;
        ts.push(Tensor { ty: 10, n: (b * 256) as u64, data: (0..b * 84).map(|_| r.below(256) as u8).collect() });
    }
    // (a quantised tensor after it, of an odd length)
    ts.push(Tensor { ty: 12, n: 256 * 41, data: blocks(&mut r, Quant::Q4K, 41) });

    let align = 32usize;
    let mut h = Vec::new();
    h.extend_from_slice(b"GGUF");
    h.extend_from_slice(&3u32.to_le_bytes());
    h.extend_from_slice(&(ts.len() as u64).to_le_bytes());
    h.extend_from_slice(&3u64.to_le_bytes());
    let string = |h: &mut Vec<u8>, s: &[u8]| {
        h.extend_from_slice(&(s.len() as u64).to_le_bytes());
        h.extend_from_slice(s);
    };
    string(&mut h, b"general.architecture");
    h.extend_from_slice(&8u32.to_le_bytes());
    string(&mut h, b"made-up");
    string(&mut h, b"general.alignment");
    h.extend_from_slice(&4u32.to_le_bytes());
    h.extend_from_slice(&(align as u32).to_le_bytes());
    string(&mut h, b"tokenizer.tokens");
    h.extend_from_slice(&9u32.to_le_bytes());
    h.extend_from_slice(&8u32.to_le_bytes());
    h.extend_from_slice(&3u64.to_le_bytes());
    for s in [&b"<s>"[..], b"hello", b"world"] {
        string(&mut h, s);
    }
    let mut off = 0usize;
    let mut places = Vec::new();
    for (i, t) in ts.iter().enumerate() {
        string(&mut h, format!("blk.{i}.weight").as_bytes());
        h.extend_from_slice(&1u32.to_le_bytes());
        h.extend_from_slice(&t.n.to_le_bytes());
        h.extend_from_slice(&t.ty.to_le_bytes());
        h.extend_from_slice(&(off as u64).to_le_bytes());
        places.push(off);
        off = (off + t.data.len()).div_ceil(align) * align;
    }
    h.resize(h.len().div_ceil(align) * align, 0);
    let base = h.len();
    for (t, &at) in ts.iter().zip(&places) {
        h.resize(base + at, 0);
        h.extend_from_slice(&t.data);
    }
    h
}
