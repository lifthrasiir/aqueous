//! Made-up files of tensors for the tests (here and aqr-decompress's). GGUF files ([`file`]):
//! tensors of every type this crate codes, of weights alike to a model's (a bell of values), with
//! the corners its formulas take: floats of exponents past 16 (escapes), F16 of BF16's precision
//! and F16 of another in part, tensors too small to code, a type not coded, a long run of it,
//! padding. Files of `torch.save` ([`pth`]) and safetensors ([`safetensors`]): floats of each
//! type, integers not coded, a tensor sharing another's storage.

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
    // (the types below 4 bits: their symbols of a bell, raw bytes noise, side bytes as above)
    if Quant::ALL[11..].contains(&q) {
        use crate::layout::{join_quant, quant_raw, quant_side};
        let mut b = vec![0u8; q.block()];
        for _ in 0..blocks {
            let mut syms = Vec::new();
            for p in q.coded() {
                let a = p.alphabet() as f64;
                syms.extend((0..p.syms()).map(|_| ((bell(r) + 1.0) * a / 2.0).clamp(0.0, a - 1.0) as u8));
            }
            let raw: Vec<u8> = (0..quant_raw(q, 1)).map(|_| r.below(256) as u8).collect();
            let side: Vec<u8> =
                (0..quant_side(q, 1)).map(|i| if i % 2 == 1 { 0x2c } else { r.below(256) as u8 }).collect();
            join_quant(q, 1, &syms, &raw, &side, &mut b);
            d.extend_from_slice(&b);
        }
        return d;
    }
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
                // (only of the types below 4 bits, made above)
                _ => unreachable!(),
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
        let t = [2, 3, 8, 12, 14, 20, 23, 39, 6, 7, 13, 10, 11, 16, 17, 18, 19, 21, 22, 29][k];
        let b = (n * (k + 2)).div_ceil(q.weights());
        ts.push(Tensor { ty: t, n: (b * q.weights()) as u64, data: blocks(&mut r, q, b) });
    }
    // (a type not coded, short and long: TQ1_0's bytes of noise)
    ts.push(Tensor { ty: 34, n: 512, data: (0..2 * 54).map(|_| r.below(256) as u8).collect() });
    if scale >= 8 {
        let b = (1 << 20) / 54 + 3;
        ts.push(Tensor { ty: 34, n: (b * 256) as u64, data: (0..b * 54).map(|_| r.below(256) as u8).collect() });
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

/// The floats of a made-up model, as safetensors names their types, and their bytes: of each
/// type, a tensor; a small one; integers (not coded); and a tensor of BF16 again.
fn floats(seed: u64, scale: usize) -> Vec<(&'static str, Vec<u8>)> {
    let mut r = Rng::new(seed);
    let n = 4096 * scale;
    let w = |r: &mut Rng, n: usize| weights(r, n, 0.05, 0);
    let f32s = |v: Vec<f32>| -> Vec<u8> { v.iter().flat_map(|v| v.to_le_bytes()).collect() };
    let bf16s = |v: Vec<f32>| -> Vec<u8> { v.iter().flat_map(|&v| f32_to_bf16(v).to_le_bytes()).collect() };
    vec![
        ("F32", f32s(w(&mut r, 3 * n + 7))),
        ("BF16", bf16s(w(&mut r, 2 * n))),
        ("F16", w(&mut r, n + 1).iter().flat_map(|&v| f32_to_f16(v).to_le_bytes()).collect()),
        ("F32", f32s(w(&mut r, 50))),
        ("I64", (0..n as u64 / 4).flat_map(|i| (i * 7 % 1000).to_le_bytes()).collect()),
        ("BF16", bf16s(w(&mut r, n))),
    ]
}

/// Bytes of an element of a type of safetensors' (of [`floats`]).
fn width(ty: &str) -> usize {
    match ty {
        "F16" | "BF16" => 2,
        "F32" => 4,
        _ => 8,
    }
}

/// A safetensors file of seed `seed`, its tensors of `scale` times a few thousand elements each.
pub fn safetensors(seed: u64, scale: usize) -> Vec<u8> {
    let ts = floats(seed, scale);
    let mut h = String::from(r#"{"__metadata__":{"format":"pt","note":"a \"quote\" {}"}"#);
    let mut at = 0;
    for (i, (ty, d)) in ts.iter().enumerate() {
        let (n, end) = (d.len() / width(ty), at + d.len());
        h += &format!(r#","layer.{i}.weight":{{"dtype":"{ty}","shape":[{n}],"data_offsets":[{at},{end}]}}"#);
        at = end;
    }
    h.push('}');
    while h.len() % 8 != 0 {
        h.push(' ');
    }
    let mut out = (h.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(h.as_bytes());
    ts.iter().for_each(|(_, d)| out.extend_from_slice(d));
    out
}

/// The pickle of a state dict as `torch.save` writes it (protocol 2, the strings and globals got
/// from the memo again) of tensors `ts`, the last a view of the second's storage.
fn pickle(ts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut p = vec![0x80, 2, b'}', b'q', 0, b'('];
    let mut memo: std::collections::HashMap<String, u8> = Default::default();
    // (what `make` puts the first time, memoised; got from the memo after)
    let mut once = |p: &mut Vec<u8>, key: &str, make: &dyn Fn(&mut Vec<u8>)| match memo.get(key) {
        Some(&m) => p.extend_from_slice(&[b'h', m]),
        None => {
            make(p);
            let m = memo.len() as u8 + 1;
            p.extend_from_slice(&[b'q', m]);
            memo.insert(key.into(), m);
        }
    };
    let s = |p: &mut Vec<u8>, s: &str| {
        p.push(b'X');
        p.extend_from_slice(&(s.len() as u32).to_le_bytes());
        p.extend_from_slice(s.as_bytes());
    };
    let storage = |ty: &str| match ty {
        "F32" => "FloatStorage",
        "F16" => "HalfStorage",
        "BF16" => "BFloat16Storage",
        _ => "LongStorage",
    };
    for (i, (ty, d)) in ts.iter().enumerate() {
        let key = if i == ts.len() - 1 { 1 } else { i };
        s(&mut p, &format!("layer.{i}.weight"));
        once(&mut p, "rebuild", &|p| p.extend_from_slice(b"ctorch._utils\n_rebuild_tensor_v2\n"));
        p.extend_from_slice(b"((");
        once(&mut p, "storage", &|p| s(p, "storage"));
        once(&mut p, ty, &|p| p.extend_from_slice(format!("ctorch\n{}\n", storage(ty)).as_bytes()));
        s(&mut p, &key.to_string());
        once(&mut p, "cpu", &|p| s(p, "cpu"));
        p.push(b'J');
        p.extend_from_slice(&((ts[key].1.len() / width(ty)) as i32).to_le_bytes());
        p.extend_from_slice(b"tQK\x00(J");
        p.extend_from_slice(&((d.len() / width(ty)) as i32).to_le_bytes());
        p.extend_from_slice(b"t(K\x01t\x89");
        once(&mut p, "dict", &|p| p.extend_from_slice(b"ccollections\nOrderedDict\n"));
        p.extend_from_slice(b")RtR");
    }
    p.extend_from_slice(b"u.");
    p
}

/// A file of `torch.save` of seed `seed` (a state dict, its tensors those of [`safetensors`]', the
/// last a view of the second's storage), its tensors of `scale` times a few thousand elements each.
pub fn pth(seed: u64, scale: usize) -> Vec<u8> {
    let ts = floats(seed, scale);
    // (the archive's entries stored, their data at multiples of 64, padded by an extra field)
    let mut entries: Vec<(String, Vec<u8>)> = vec![("archive/data.pkl".into(), pickle(&ts))];
    for (i, (_, d)) in ts.iter().enumerate().take(ts.len() - 1) {
        entries.push((format!("archive/data/{i}"), d.clone()));
    }
    entries.push(("archive/version".into(), b"3\n".to_vec()));
    entries.push(("archive/byteorder".into(), b"little".to_vec()));
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, d) in &entries {
        let at = out.len();
        let crc = aqr_common::crc32::hash(d);
        let pad = (64 - (at + 30 + name.len() + 4) % 64) % 64;
        let fields = |h: &mut Vec<u8>, extra: usize| {
            h.extend_from_slice(&[0, 0, 8, 8, 0, 0, 0, 0, 0, 0]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(d.len() as u32).to_le_bytes());
            h.extend_from_slice(&(d.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&(extra as u16).to_le_bytes());
        };
        out.extend_from_slice(b"PK\x03\x04");
        fields(&mut out, pad + 4);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b"FB");
        out.extend_from_slice(&(pad as u16).to_le_bytes());
        out.resize(out.len() + pad, b'Z');
        out.extend_from_slice(d);
        central.extend_from_slice(b"PK\x01\x02\x00\x00");
        fields(&mut central, 0);
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&(at as u32).to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd = out.len();
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&(cd as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}
