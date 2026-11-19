//! `.mo` files through the formula and the module: as msgfmt lays them out, in either byte
//! order, with and without a hash table, with plural forms; those laid out otherwise not taken.

use aqr_archive::Evaluate;
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

/// A table whose size and probe step are not coprime has slots the probing never reaches: it
/// gives up instead of looping (the keys "a", "c", "e" hash to odd numbers: step 2 in 4 slots).
#[test]
fn hash_table_with_unreachable_slots_ends() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let keys: [&[u8]; 3] = [b"a", b"c", b"e"];
        tx.send(mo::hash_table(keys.iter(), 4)).ok();
    });
    let t = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("hash_table hangs");
    assert!(t.is_none());
}

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

/// Every damage of the inputs of `build` (the file without its tables, the NULs, the length,
/// the start) ends in a file or an error, within a time, with no allocation the data cannot hold.
#[test]
fn build_of_damaged_inputs() {
    use std::time::Duration;
    for big in [false, true] {
        let d = mo_file(&catalog(), big, 11, 0);
        let (rest, start, nuls) = mo::split(&d).unwrap();
        let t = Duration::from_secs(10);
        aqr_testkit::damaged(&rest, 64, 64, t, |r| {
            let _ = mo::build(r, start, &nuls, d.len());
            let _ = mo::build(r, start, &nuls, r.len() + 3 * 8 * 7 + 44);
        });
        aqr_testkit::damaged(&nuls, 64, 64, t, |n| {
            let _ = mo::build(&rest, start, n, d.len());
        });
        for s in [0, 1, 27, 28, start - 1, start + 1, d.len(), usize::MAX, u32::MAX as usize] {
            let _ = mo::build(&rest, s, &nuls, d.len());
        }
        for len in [0, 1, 28, d.len() - 1, d.len() + 1, u32::MAX as usize, usize::MAX] {
            assert!(len == d.len() || mo::build(&rest, start, &nuls, len).is_err());
        }
        // the whole file damaged: what encode takes, it rebuilds
        aqr_testkit::damaged(&d, 64, 64, t, |x| {
            if let Some(f) = formula::encode(x, 1) {
                assert!(eval(&f).unwrap() == x);
            }
            let _ = formula::extent(x);
        });
    }
}

/// Duplicate originals (the hash table of a catalogue msgfmt would refuse still has to be
/// reproduced or not taken), and keys of a context (`CTX\x04id`).
#[test]
fn duplicates_and_contexts() {
    let dup: Vec<(&[u8], &[u8])> = vec![(b"a", b"x"), (b"a", b"y"), (b"b", b"z")];
    for hash in [0, 7] {
        let d = mo_file(&dup, false, hash, 0);
        if let Some(f) = formula::encode(&d, 1) {
            assert!(eval(&f).unwrap() == d);
        }
    }
    let ctx: Vec<(&[u8], &[u8])> =
        vec![(b"menu\x04Open", b"A"), (b"dialog\x04Open", b"B"), (b"Open", b"C"), (b"\x04", b"D"), (b"\x04\x04", b"E")];
    for big in [false, true] {
        for hash in [0, 11, 13] {
            let d = mo_file(&ctx, big, hash, 0);
            let f = formula::encode(&d, 1).unwrap();
            assert!(eval(&f).unwrap() == d);
        }
    }
    // empty strings only; translations empty
    let e: Vec<(&[u8], &[u8])> = vec![(b"", b""), (b"k", b"")];
    let d = mo_file(&e, false, 5, 0);
    assert!(eval(&formula::encode(&d, 1).unwrap()).unwrap() == d);
}

/// Hash tables of size 0, 1, 2 (too small for the probing's step, `size - 2`) and 3.
#[test]
fn hash_table_tiny_sizes() {
    let none: [&[u8]; 0] = [];
    let one: [&[u8]; 1] = [b"a"];
    for size in 0..=3 {
        for keys in [&none[..], &one[..]] {
            let r = std::panic::catch_unwind(|| mo::hash_table(keys.iter(), size));
            let r = r.unwrap_or_else(|_| panic!("size {size}, {} keys panics", keys.len()));
            if size < 3 || keys.len() >= size {
                assert!(r.is_none() || (keys.is_empty() && size > 0), "size {size}");
            }
        }
    }
    assert_eq!(mo::hash_table(one.iter(), 3).unwrap().iter().filter(|&&v| v == 1).count(), 1);
    // headers of such sizes are not taken
    for size in [1usize, 2] {
        let mut d = mo_file(&catalog(), false, 11, 0);
        d[20..24].copy_from_slice(&(size as u32).to_le_bytes());
        assert!(formula::encode(&d, 1).is_none());
    }
    let mut d = mo_file(&[], false, 0, 0);
    d[20..24].copy_from_slice(&2u32.to_le_bytes());
    d.resize(d.len() + 8, 0);
    assert!(formula::encode(&d, 1).is_none());
}
