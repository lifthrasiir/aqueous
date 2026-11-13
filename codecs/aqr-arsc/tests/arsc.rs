//! Resource tables written here (no Android tools needed) through the formula and the module:
//! string pools of UTF-8 and UTF-16 with styles, type chunks of simple and complex entries and
//! entries left out, in several configurations; chunks the module does not take (compact
//! entries, lengths not in their shortest form, chunks of other types) kept as they are;
//! streams corrupted, which never panic.

use aqr_arsc::arsc::{self, s};
use aqr_arsc::{encode, formula};
use aqr_common::rng::Rng;
use aqr_formula::{Apply, Formula};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// How a table is written apart from its contents.
#[derive(Default, Clone, Copy)]
struct Shape {
    /// An entry of a type chunk compact.
    compact: bool,
    /// A string's length in two bytes where one would do.
    long_len: bool,
    /// A chunk of a type the format does not have, in the package.
    unknown: bool,
}

fn chunk(t: u16, header: &[u8], body: &[u8]) -> Vec<u8> {
    let hs = 8 + header.len();
    let mut c = Vec::new();
    c.extend_from_slice(&t.to_le_bytes());
    c.extend_from_slice(&(hs as u16).to_le_bytes());
    c.extend_from_slice(&((hs + body.len()) as u32).to_le_bytes());
    c.extend_from_slice(header);
    c.extend_from_slice(body);
    c
}

fn len8(out: &mut Vec<u8>, n: usize) {
    if n < 0x80 {
        out.push(n as u8);
    } else {
        out.extend_from_slice(&[0x80 | (n >> 8) as u8, n as u8]);
    }
}

/// A string pool of `strings`, UTF-8 or UTF-16, with `styles` styled ones first.
fn pool(strings: &[String], utf8: bool, styles: usize, long_len: bool) -> Vec<u8> {
    let mut data = Vec::new();
    let mut offs = Vec::new();
    for (i, s) in strings.iter().enumerate() {
        offs.push(data.len() as u32);
        if utf8 {
            let u16n = s.encode_utf16().count();
            if long_len && i == 1 {
                data.extend_from_slice(&[0x80, u16n as u8]);
            } else {
                len8(&mut data, u16n);
            }
            len8(&mut data, s.len());
            data.extend_from_slice(s.as_bytes());
            data.push(0);
        } else {
            let u: Vec<u16> = s.encode_utf16().collect();
            data.extend_from_slice(&(u.len() as u16).to_le_bytes());
            for x in u {
                data.extend_from_slice(&x.to_le_bytes());
            }
            data.extend_from_slice(&[0, 0]);
        }
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }
    // (a style: spans of a name, a start and an end, then 0xFFFFFFFF; then the list's end)
    let mut style = Vec::new();
    let mut style_offs = Vec::new();
    for _ in 0..styles {
        style_offs.push(style.len() as u32);
        for v in [0u32, 0, 1, u32::MAX] {
            style.extend_from_slice(&v.to_le_bytes());
        }
    }
    if styles > 0 {
        style.extend_from_slice(&[0xff; 8]);
    }
    let n = strings.len();
    let strings_start = 28 + 4 * (n + styles);
    let styles_start = if styles > 0 { strings_start + data.len() } else { 0 };
    let mut h = Vec::new();
    for v in [n as u32, styles as u32, if utf8 { 0x100 } else { 0 }, strings_start as u32, styles_start as u32] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    let mut body = Vec::new();
    for o in offs.iter().chain(&style_offs) {
        body.extend_from_slice(&o.to_le_bytes());
    }
    body.extend_from_slice(&data);
    body.extend_from_slice(&style);
    chunk(0x0001, &h, &body)
}

/// A table made from `seed`, and how many keys it has.
fn write(seed: u64, shape: Shape) -> (Vec<u8>, usize) {
    let mut rng = Rng::new(seed);
    let words = ["OK", "Cancel", "Annuler", "Abbrechen", "확인", "取消", "\u{1f600} smile", "res/drawable/a.png"];
    let n = 3 + rng.below(30) as usize;
    let values: Vec<String> =
        (0..n).map(|i| format!("{} {i}", words[rng.below(words.len() as u64) as usize])).collect();
    let keys: Vec<String> = (0..1 + rng.below(20) as usize).map(|i| format!("key_{i}")).collect();
    let types = ["attr", "string", "style"].map(String::from);
    let mut pkg = Vec::new();
    pkg.extend(pool(&types, false, 0, false));
    pkg.extend(pool(&keys, true, 0, false));
    if shape.unknown {
        pkg.extend(chunk(0x0203, &[1, 0, 0, 0], b"library"));
    }
    for tid in 1..=3u8 {
        let count = keys.len();
        let mut spec = vec![tid, 0, 0, 0];
        spec.extend_from_slice(&(count as u32).to_le_bytes());
        let flags: Vec<u8> = (0..count).flat_map(|_| 0u32.to_le_bytes()).collect();
        pkg.extend(chunk(0x0202, &spec, &flags));
        for config in 0..1 + rng.below(4) {
            let mut offs = Vec::new();
            let mut entries = Vec::new();
            for i in 0..count {
                if config > 0 && rng.below(3) == 0 {
                    offs.extend_from_slice(&u32::MAX.to_le_bytes());
                    continue;
                }
                offs.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                let key = if rng.below(8) == 0 { rng.below(count as u64) as u32 } else { i as u32 };
                if tid == 3 && shape.compact && i == 0 {
                    // (compact: the key and the value's type in the entry, its data after)
                    entries.extend_from_slice(&[8, 3, 9, 0]);
                    entries.extend_from_slice(&key.to_le_bytes());
                    continue;
                }
                if tid == 3 {
                    entries.extend_from_slice(&16u16.to_le_bytes());
                    entries.extend_from_slice(&1u16.to_le_bytes());
                    entries.extend_from_slice(&key.to_le_bytes());
                    entries.extend_from_slice(&0u32.to_le_bytes());
                    let maps = rng.below(3) as u32;
                    entries.extend_from_slice(&maps.to_le_bytes());
                    for m in 0..maps {
                        entries.extend_from_slice(&(0x0101_0000 + m).to_le_bytes());
                        entries.extend_from_slice(&[8, 0, 0, 0x10]);
                        entries.extend_from_slice(&(rng.u64() as u32).to_le_bytes());
                    }
                } else {
                    entries.extend_from_slice(&8u16.to_le_bytes());
                    entries.extend_from_slice(&((rng.below(2) as u16) << 1).to_le_bytes());
                    entries.extend_from_slice(&key.to_le_bytes());
                    let (vt, data) = match tid {
                        2 => (0x03u8, rng.below(n as u64) as u32),
                        _ => [(0x10u8, rng.below(100) as u32), (0x12, u32::MAX), (0x01, 0x7f01_0000 + i as u32)]
                            [rng.below(3) as usize],
                    };
                    entries.extend_from_slice(&[8, 0, 0, vt]);
                    entries.extend_from_slice(&data.to_le_bytes());
                }
            }
            let mut h = vec![tid, 0, 0, 0];
            h.extend_from_slice(&(count as u32).to_le_bytes());
            h.extend_from_slice(&((20 + 64 + 4 * count) as u32).to_le_bytes());
            // (the configuration: its size, then a language)
            let mut cfg = vec![0u8; 64];
            cfg[..4].copy_from_slice(&64u32.to_le_bytes());
            cfg[8] = b"-efk"[config as usize];
            h.extend_from_slice(&cfg);
            let mut body = offs;
            body.extend_from_slice(&entries);
            pkg.extend(chunk(0x0201, &h, &body));
        }
    }
    let mut ph = Vec::new();
    ph.extend_from_slice(&0x7fu32.to_le_bytes());
    let name: Vec<u8> =
        "app".encode_utf16().chain(std::iter::repeat(0)).take(128).flat_map(|u| u.to_le_bytes()).collect();
    ph.extend_from_slice(&name);
    for v in [288u32, 3, 288 + pool(&types, false, 0, false).len() as u32, keys.len() as u32, 0] {
        ph.extend_from_slice(&v.to_le_bytes());
    }
    let package = chunk(0x0200, &ph, &pkg);
    let mut body = pool(&values, true, 1 + rng.below(2) as usize, shape.long_len);
    body.extend(package);
    (chunk(0x0002, &1u32.to_le_bytes(), &body), keys.len())
}

fn streams(st: &[Vec<u8>]) -> Vec<&[u8]> {
    arsc::unpack(&st.iter().map(|v| v.as_slice()).collect::<Vec<_>>()).unwrap()
}

fn check(d: &[u8]) -> Vec<Vec<u8>> {
    let f = formula::encode(d, 1).expect("taken");
    assert_eq!(eval(&f).unwrap(), d);
    encode::split(d).unwrap()
}

/// Tables of every shape come back; their chunks are taken, but the type specs.
#[test]
fn tables() {
    for seed in 0..300 {
        let (d, keys) = write(seed, Shape::default());
        let st = check(&d);
        // (only the type specs' flags, 4 bytes a key, of the 3 types)
        assert_eq!(streams(&st)[s::RAW].len(), 3 * 4 * keys, "seed {seed}");
    }
}

/// What the module would not rebuild as it is is kept: a type chunk with a compact entry, a pool
/// with a length longer than it needs, a chunk of another type.
#[test]
fn kept() {
    for seed in 0..50 {
        for shape in [
            Shape { compact: true, ..Shape::default() },
            Shape { long_len: true, ..Shape::default() },
            Shape { unknown: true, ..Shape::default() },
        ] {
            let (d, _) = write(seed, shape);
            let plain = streams(&check(&write(seed, Shape::default()).0))[s::RAW].len();
            let st = check(&d);
            assert!(streams(&st)[s::RAW].len() > plain, "seed {seed}");
        }
    }
}

/// Not resource tables, or ones the module does not take.
#[test]
fn not_taken() {
    let (d, _) = write(1, Shape::default());
    assert!(encode::split(&d[..d.len() - 1]).is_none(), "cut short");
    assert!(encode::split(&[2, 0, 12, 0, 12, 0, 0, 0, 0, 0, 0, 0]).is_some(), "empty");
    assert!(encode::split(&[0; 64]).is_none());
}

/// A type chunk whose header is the 8 bytes of any chunk's, at the end of the file, is kept as
/// it is (its type id would lie past the file).
#[test]
fn type_chunk_of_a_bare_header() {
    let d = chunk(arsc::ty::TABLE, &[0; 4], &chunk(arsc::ty::TYPE, &[], &[]));
    let st = encode::split(&d).expect("not taken");
    let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
    assert!(arsc::decode(&refs, d.len()).unwrap() == d);
}

/// Chunks nested without end (a table inside a table, 100000 deep) are kept as they are by the
/// encoder, and refused by the decoder, rather than overflowing the stack.
#[test]
fn nesting_is_bounded() {
    const N: usize = 100_000;
    // (the table of each level takes the rest of the file, the innermost its own header)
    let mut d = Vec::new();
    for i in 0..N {
        d.extend_from_slice(&[2, 0, 12, 0]);
        d.extend_from_slice(&((12 * (N - i)) as u32).to_le_bytes());
        d.extend_from_slice(&[0; 4]);
    }
    let st = encode::split(&d).expect("not taken");
    let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
    assert!(arsc::decode(&refs, d.len()).unwrap() == d);
    // (a head stream of containers of the headers' 8 bytes and the mode)
    let mut head = Vec::new();
    for i in 0..N {
        head.extend_from_slice(&arsc::ty::TABLE.to_le_bytes());
        head.extend_from_slice(&8u16.to_le_bytes());
        head.extend_from_slice(&((8 * (N - i)) as u32).to_le_bytes());
        head.push(arsc::mode::CONTAINER);
    }
    let mut st = vec![Vec::new(); s::N];
    st[s::HEAD] = head;
    let inputs = arsc::pack(&st);
    let refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();
    assert!(arsc::decode(&refs, 8 * N).is_err());
}

/// Corrupted streams make the module fail or give a file of the length, never panic.
#[test]
fn corrupt() {
    let (d, _) = write(3, Shape::default());
    let st = encode::split(&d).unwrap();
    let mut rng = Rng::new(7);
    for _ in 0..20000 {
        let mut bad = st.clone();
        let k = rng.below(bad.len() as u64) as usize;
        if bad[k].is_empty() {
            continue;
        }
        let at = rng.below(bad[k].len() as u64) as usize;
        match rng.below(3) {
            0 => bad[k].truncate(at),
            1 => bad[k][at] ^= 1 << rng.below(8),
            _ => bad[k][at] = rng.u64() as u8,
        }
        let refs: Vec<&[u8]> = bad.iter().map(|v| v.as_slice()).collect();
        if let Ok(m) = formula::decode(&[], &refs, d.len() as u64) {
            assert_eq!(m.len(), d.len());
        }
    }
}
