//! PPMd var.H with RAR's range coder: round trips, and RAR 3.x archives' PPMd streams decoded
//! and encoded again byte for byte.

use aqr_rar::ppmd::{Model, RangeDecoder, RangeEncoder};
use std::path::{Path, PathBuf};

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn random_bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut st = seed | 1;
    (0..n).map(|_| xorshift(&mut st) as u8).collect()
}

/// Words from a small vocabulary with some punctuation and numbers: compressible like text.
fn text_like(n: usize, seed: u64) -> Vec<u8> {
    const WORDS: &[&str] = &[
        "the",
        "of",
        "and",
        "a",
        "to",
        "in",
        "is",
        "you",
        "that",
        "it",
        "he",
        "was",
        "for",
        "on",
        "are",
        "as",
        "with",
        "his",
        "they",
        "I",
        "at",
        "be",
        "this",
        "have",
        "from",
        "or",
        "one",
        "had",
        "by",
        "word",
        "but",
        "not",
        "what",
        "all",
        "were",
        "we",
        "when",
        "your",
        "can",
        "said",
        "there",
        "use",
        "an",
        "each",
        "which",
        "she",
        "compression",
        "archive",
        "recompression",
        "model",
        "context",
        "symbol",
        "frequency",
        "escape",
    ];
    let mut st = seed | 1;
    let mut out = Vec::with_capacity(n + 32);
    while out.len() < n {
        let r = xorshift(&mut st);
        let w = WORDS[(r % WORDS.len() as u64) as usize];
        out.extend_from_slice(w.as_bytes());
        match (r >> 32) % 23 {
            0 => out.extend_from_slice(b".\n"),
            1 => out.extend_from_slice(b", "),
            2 => out.extend_from_slice(format!(" {} ", (r >> 40) % 1000).as_bytes()),
            3 => out.push(((r >> 48) as u8) | 0x80),
            _ => out.push(b' '),
        }
    }
    out.truncate(n);
    out
}

fn encode_all(model: &mut Model, data: &[u8]) -> Vec<u8> {
    let mut rc = RangeEncoder::new();
    for &b in data {
        model.encode(&mut rc, b);
    }
    rc.flush()
}

/// Decodes `n` symbols, checking that exactly `packed` was read.
fn decode_all(model: &mut Model, packed: &[u8], n: usize) -> Vec<u8> {
    let mut rc = RangeDecoder::new(packed);
    let out: Vec<u8> = (0..n).map(|_| model.decode(&mut rc).expect("valid stream")).collect();
    assert_eq!(rc.consumed(), packed.len(), "bytes read");
    assert!(!rc.overrun());
    out
}

fn round_trip(data: &[u8], order: u32, mem: u32) -> usize {
    let mut enc = Model::new(mem).unwrap();
    enc.init(order);
    let packed = encode_all(&mut enc, data);
    let mut dec = Model::new(mem).unwrap();
    dec.init(order);
    let out = decode_all(&mut dec, &packed, data.len());
    assert!(out == data, "order {order}, mem {mem}: round trip differs");
    packed.len()
}

#[test]
fn round_trips() {
    let text = text_like(300_000, 7);
    let random = random_bytes(60_000, 3);
    let mut mixed = text_like(100_000, 11);
    mixed.extend_from_slice(&random_bytes(20_000, 5));
    mixed.extend(std::iter::repeat_n(0u8, 30_000));
    mixed.extend_from_slice(&text_like(50_000, 13));
    for &order in &[2, 4, 8, 16, 64] {
        for &mem in &[1 << 20, 16 << 20] {
            let a = round_trip(&text, order, mem);
            let b = round_trip(&random, order, mem);
            let c = round_trip(&mixed, order, mem);
            println!("order {order:2} mem {:2} MiB: text {a}, random {b}, mixed {c}", mem >> 20);
        }
    }
    // Small enough that the model restarts (several times over, at high orders).
    for &order in &[2, 6, 16, 64] {
        round_trip(&text, order, 1 << 14);
        round_trip(&mixed, order, 1 << 15);
        round_trip(&random, order, 1 << 11);
    }
    round_trip(&[], 4, 1 << 20);
    round_trip(b"a", 4, 1 << 20);
}

#[test]
fn continued_across_blocks() {
    let a = text_like(80_000, 21);
    let b = text_like(70_000, 22);
    for &(order, mem) in &[(4, 1 << 20), (16, 1 << 16), (64, 16 << 20)] {
        let mut enc = Model::new(mem).unwrap();
        enc.init(order);
        let pa = encode_all(&mut enc, &a);
        let pb = encode_all(&mut enc, &b);
        let mut dec = Model::new(mem).unwrap();
        dec.init(order);
        // Both blocks back to back: each decoder stops where its block ends.
        let mut cat = pa.clone();
        cat.extend_from_slice(&pb);
        let mut rc = RangeDecoder::new(&cat);
        let oa: Vec<u8> = (0..a.len()).map(|_| dec.decode(&mut rc).unwrap()).collect();
        assert_eq!(rc.consumed(), pa.len());
        assert!(oa == a);
        let ob = decode_all(&mut dec, &pb, b.len());
        assert!(ob == b);
        // The continued model is worth something.
        let mut fresh = Model::new(mem).unwrap();
        fresh.init(order);
        assert!(pb.len() < encode_all(&mut fresh, &b).len());
    }
}

#[test]
fn overrun_detected() {
    let data = text_like(5000, 9);
    let mut enc = Model::new(1 << 20).unwrap();
    enc.init(4);
    let packed = encode_all(&mut enc, &data);
    let cut = &packed[..packed.len() - 2];
    let mut dec = Model::new(1 << 20).unwrap();
    dec.init(4);
    let mut rc = RangeDecoder::new(cut);
    for _ in 0..data.len() {
        if dec.decode(&mut rc).is_none() {
            break;
        }
    }
    assert!(rc.overrun());
}

// ---- RAR 3.x archives ----

fn fixtures() -> Option<PathBuf> {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/rar-dl/fixtures4");
    if d.is_dir() {
        Some(d)
    } else {
        println!("note: {} absent, skipped", d.display());
        None
    }
}

struct Entry {
    name: String,
    method: u8,
    unpacked: u64,
    crc: u32,
    data: Vec<u8>,
}

fn u16le(d: &[u8], o: usize) -> usize {
    u16::from_le_bytes([d[o], d[o + 1]]) as usize
}

fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

/// The file entries of a RAR 3.x archive (docs/rar3-decoding.md §2), not split, not encrypted.
fn entries(a: &[u8]) -> Vec<Entry> {
    assert_eq!(&a[..7], b"Rar!\x1a\x07\x00");
    let mut at = 7;
    let mut out = Vec::new();
    while at + 7 <= a.len() {
        let ty = a[at + 2];
        let flags = u16le(a, at + 3);
        let size = u16le(a, at + 5);
        let mut next = at + size;
        if ty == 0x74 || ty == 0x7A {
            let h = &a[at..at + size];
            let mut packed = u32le(h, 7) as u64;
            let mut unpacked = u32le(h, 11) as u64;
            let mut name_at = 32;
            if flags & 0x100 != 0 {
                packed |= (u32le(h, 32) as u64) << 32;
                unpacked |= (u32le(h, 36) as u64) << 32;
                name_at = 40;
            }
            let name_len = u16le(h, 26);
            let name = String::from_utf8_lossy(&h[name_at..name_at + name_len]).into_owned();
            let data = a[next..next + packed as usize].to_vec();
            next += packed as usize;
            if ty == 0x74 && flags & 0xE0 != 0xE0 {
                out.push(Entry { name, method: h[25], unpacked, crc: u32le(h, 16), data });
            }
        } else if flags & 0x8000 != 0 {
            next += u32le(a, at + 7) as usize;
        }
        if ty == 0x7B {
            break;
        }
        at = next;
    }
    out
}

struct Decoded {
    order: u32,
    mem: u32,
    /// The stream's header bytes (flags, M, E).
    header: Vec<u8>,
    /// Every symbol the model coded, escapes included.
    symbols: Vec<u8>,
    output: Vec<u8>,
    /// Where the range coder's bytes end.
    end: usize,
}

/// Decodes a file's data made of one PPMd block ending the file; None if it is anything else.
fn decode_ppmd_file(d: &[u8], unpacked: usize) -> Option<Decoded> {
    let f = d[0];
    if f & 0x80 == 0 || f & 0x20 == 0 {
        return None;
    }
    let mut at = 1;
    let m = d[at] as u32;
    at += 1;
    let mut esc = 2u8;
    if f & 0x40 != 0 {
        esc = d[at];
        at += 1;
    }
    let mut order = (f as u32 & 0x1F) + 1;
    if order > 16 {
        order = 16 + 3 * (order - 16);
    }
    let mem = (m + 1) << 20;
    let mut model = Model::new(mem).unwrap();
    model.init(order);
    let mut rc = RangeDecoder::new(&d[at..]);
    let mut symbols = Vec::new();
    let mut output = Vec::with_capacity(unpacked);
    let mut next = |rc: &mut RangeDecoder, symbols: &mut Vec<u8>| {
        let s = model.decode(rc).expect("valid PPMd stream");
        symbols.push(s);
        s
    };
    loop {
        let s = next(&mut rc, &mut symbols);
        if s != esc {
            output.push(s);
            continue;
        }
        match next(&mut rc, &mut symbols) {
            0 => return None, // switches to LZ
            2 => break,
            4 => {
                let mut dist = 0usize;
                for _ in 0..3 {
                    dist = dist << 8 | next(&mut rc, &mut symbols) as usize;
                }
                let len = next(&mut rc, &mut symbols) as usize + 32;
                copy(&mut output, dist + 2, len);
            }
            5 => {
                let len = next(&mut rc, &mut symbols) as usize + 4;
                copy(&mut output, 1, len);
            }
            1 => output.push(esc),
            c => panic!("escape code {c}"),
        }
    }
    assert!(!rc.overrun());
    Some(Decoded { order, mem, header: d[..at].to_vec(), symbols, output, end: at + rc.consumed() })
}

fn copy(out: &mut Vec<u8>, dist: usize, len: usize) {
    for _ in 0..len {
        let b = out[out.len() - dist];
        out.push(b);
    }
}

fn encode_again(dec: &Decoded) -> Vec<u8> {
    let mut model = Model::new(dec.mem).unwrap();
    model.init(dec.order);
    let mut rc = RangeEncoder::with_output(dec.header.clone());
    for &s in &dec.symbols {
        model.encode(&mut rc, s);
    }
    rc.flush()
}

/// Checks the PPMd files of an archive; returns how many there were.
fn check_archive(path: &Path, corpus: Option<&Path>) -> usize {
    let name = path.file_name().unwrap().to_string_lossy();
    let a = std::fs::read(path).unwrap();
    let mut checked = 0;
    for e in entries(&a) {
        if e.method == 0x30 || e.data.is_empty() {
            continue;
        }
        let Some(dec) = decode_ppmd_file(&e.data, e.unpacked as usize) else {
            println!("{name}/{}: not one PPMd block, skipped", e.name);
            continue;
        };
        assert_eq!(dec.output.len() as u64, e.unpacked, "{name}/{}", e.name);
        assert_eq!(aqr_common::crc32::hash(&dec.output), e.crc, "{name}/{}", e.name);
        if let Some(Ok(o)) = corpus.map(|c| std::fs::read(c.join(&e.name))) {
            assert!(o == dec.output, "{name}/{}: differs from the original", e.name);
        }
        let again = encode_again(&dec);
        assert!(again[..] == e.data[..dec.end], "{name}/{}: encoded again differs", e.name);
        let tail = &e.data[dec.end..];
        println!(
            "{name}/{}: order {} mem {} MiB, {} symbols -> {} bytes, re-encoded exactly; after the flush: {:02X?}",
            e.name,
            dec.order,
            dec.mem >> 20,
            dec.symbols.len(),
            dec.end,
            tail
        );
        assert_eq!(tail, [0x00, 0xBF, 0x88, 0x67, 0xF6, 0xA9, 0xFF, 0xD4]);
        checked += 1;
    }
    checked
}

#[test]
fn rar_fixtures() {
    let Some(dir) = fixtures() else { return };
    let mut checked = 0;
    for name in ["ppmd_text.rar", "pp_mct.rar", "pp_o2_m1.rar", "pp_o8_m16.rar", "pp_o63_m128.rar"] {
        let path = dir.join(name);
        if path.exists() {
            checked += check_archive(&path, Some(&dir.join("corpus")));
        } else {
            println!("note: {name} absent");
        }
    }
    assert!(checked > 0);
}

/// Every archive in `$AQR_RAR_PPMD_DIR` (made with `rar a -ma4 -mc<order>:<MB>t+`), if set.
#[test]
fn rar_more() {
    let Some(dir) = std::env::var_os("AQR_RAR_PPMD_DIR") else { return };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    paths.retain(|p| p.extension().is_some_and(|x| x == "rar"));
    paths.sort();
    let checked: usize = paths.iter().map(|p| check_archive(p, None)).sum();
    assert!(checked > 0);
}

/// Speed: `cargo test -r -p aqr-rar --test ppmd -- --ignored --nocapture speed`, on generated text or
/// on the file `$AQR_RAR_PPMD_SPEED`; `$AQR_RAR_PPMD_CONF` (`order:MiB`) picks one configuration.
#[test]
#[ignore]
fn speed() {
    let data = match std::env::var_os("AQR_RAR_PPMD_SPEED") {
        Some(p) => std::fs::read(p).unwrap(),
        None => text_like(32 << 20, 17),
    };
    let mut confs = vec![(4u32, 15u32 << 20), (8, 16 << 20), (61, 128 << 20)];
    if let Ok(c) = std::env::var("AQR_RAR_PPMD_CONF") {
        let (o, m) = c.split_once(':').expect("order:MiB");
        confs = vec![(o.parse().unwrap(), m.parse::<u32>().unwrap() << 20)];
    }
    for (order, mem) in confs {
        let mut enc = Model::new(mem).unwrap();
        enc.init(order);
        let t = std::time::Instant::now();
        let packed = encode_all(&mut enc, &data);
        let te = t.elapsed().as_secs_f64();
        let mut dec = Model::new(mem).unwrap();
        dec.init(order);
        let t = std::time::Instant::now();
        let out = decode_all(&mut dec, &packed, data.len());
        let td = t.elapsed().as_secs_f64();
        assert!(out == data);
        let mb = data.len() as f64 / 1e6;
        println!(
            "order {order} mem {} MiB: {} -> {} bytes; encode {:.1} MB/s, decode {:.1} MB/s",
            mem >> 20,
            data.len(),
            packed.len(),
            mb / te,
            mb / td
        );
    }
}
