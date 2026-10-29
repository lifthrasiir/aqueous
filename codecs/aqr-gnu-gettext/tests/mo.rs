//! `.mo` files through the formula and the module: as msgfmt lays them out, in either byte
//! order, with and without a hash table, with plural forms; those laid out otherwise not taken.

use aqr_formula::{Apply, Formula};
use aqr_gnu_gettext::{formula, mo};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// A `.mo` file of `pairs` (original, translation), sorted by the original as msgfmt sorts
/// them, `extra` bytes after the header, a hash table of `hash` slots.
fn mo_file(pairs: &[(&[u8], &[u8])], big: bool, hash: usize, extra: usize) -> Vec<u8> {
    let mut pairs = pairs.to_vec();
    pairs.sort();
    let w = |v: usize| if big { (v as u32).to_be_bytes() } else { (v as u32).to_le_bytes() };
    let n = pairs.len();
    let (o, t) = (28 + extra, 28 + extra + 8 * n);
    let h = t + 8 * n;
    let mut d: Vec<u8> = [0x9504_12de, 0, n, o, t, hash, h].iter().flat_map(|&v| w(v)).collect();
    d.resize(o, 0xee);
    let mut at = h + 4 * hash;
    let mut tables = [Vec::new(), Vec::new()];
    let mut strings = Vec::new();
    for (k, table) in tables.iter_mut().enumerate() {
        for p in &pairs {
            let s = if k == 0 { p.0 } else { p.1 };
            table.extend_from_slice(&w(s.len()));
            table.extend_from_slice(&w(at));
            strings.extend_from_slice(s);
            strings.push(0);
            at += s.len() + 1;
        }
    }
    d.extend_from_slice(&tables[0]);
    d.extend_from_slice(&tables[1]);
    if hash > 0 {
        for v in mo::hash_table(pairs.iter().map(|p| p.0), hash).unwrap() {
            d.extend_from_slice(&w(v as usize));
        }
    }
    d.extend_from_slice(&strings);
    d
}

fn catalog() -> Vec<(&'static [u8], &'static [u8])> {
    vec![
        (b"", b"Content-Type: text/plain; charset=UTF-8\nPlural-Forms: nplurals=2; plural=(n != 1);\n"),
        (b"file", b"Datei"),
        (b"%d file\0%d files", b"%d Datei\0%d Dateien"),
        (b"Open", b"\xc3\x96ffnen"),
        (b"Close", b"Schlie\xc3\x9fen"),
        (b"Quit", b"Beenden"),
        (b"a much longer message of several words to hash", b"eine viel l\xc3\xa4ngere Nachricht"),
    ]
}

#[test]
fn round_trip() {
    for big in [false, true] {
        for (hash, extra) in [(11, 0), (0, 0), (17, 20), (8, 0)] {
            let d = mo_file(&catalog(), big, hash, extra);
            let f = formula::encode(&d, 1).unwrap_or_else(|| panic!("{big} {hash} {extra}"));
            assert!(eval(&f).unwrap() == d);
            assert_eq!(formula::extent(&d), Some(d.len()));
            let mut longer = d.clone();
            longer.extend_from_slice(b"after");
            assert_eq!(formula::extent(&longer), Some(d.len()));
            // what follows the strings is kept
            assert!(eval(&formula::encode(&longer, 1).unwrap()).unwrap() == longer);
        }
        let empty = mo_file(&[], big, 0, 0);
        assert!(eval(&formula::encode(&empty, 1).unwrap()).unwrap() == empty);
    }
}

#[test]
fn not_taken() {
    let d = mo_file(&catalog(), false, 11, 0);
    // a hash table not msgfmt's, strings not one after another, the magic, the tables' order
    let mut other_hash = d.clone();
    let h = 28 + 16 * 7;
    other_hash.swap(h, h + 4);
    other_hash.swap(h + 1, h + 5);
    let mut gap = d.clone();
    let t = 28 + 8 * 7;
    gap[t + 4] += 1;
    let mut magic = d.clone();
    magic[0] ^= 1;
    let mut order = d.clone();
    order[16..20].copy_from_slice(&20u32.to_le_bytes());
    for (name, x) in [("hash", other_hash), ("gap", gap), ("magic", magic), ("order", order)] {
        assert!(x == d || formula::encode(&x, 1).is_none(), "{name}");
    }
}

#[test]
fn corrupt_inputs() {
    let d = mo_file(&catalog(), false, 11, 0);
    let (rest, start, nuls) = mo::split(&d).unwrap();
    assert_eq!(mo::build(&rest, start, &nuls, d.len()).unwrap(), d);
    assert!(mo::build(&rest, start, &nuls, d.len() + 1).is_err());
    assert!(mo::build(&rest, start, &nuls[1..], d.len()).is_err());
    assert!(mo::build(&rest, start, &[nuls.clone(), vec![0]].concat(), d.len()).is_err());
    assert!(mo::build(&rest[..rest.len() - 3], start, &nuls, d.len() - 3).is_err());
    let mut many = nuls.clone();
    many[0] = 100;
    assert!(mo::build(&rest, start, &many, d.len()).is_err());
    // (strings from elsewhere: another file, or none)
    assert!(mo::build(&rest, 0, &nuls, d.len()).ok().as_ref() != Some(&d));
}
