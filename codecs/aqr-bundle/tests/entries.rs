//! Entries that overlap, repeat or are empty, in the packs of Godot, Source (VPK) and Doom
//! (as `edge.rs` has for Quake's): the reader takes what lies within the file, and the formula
//! rebuilds the pack, archived too, whatever the entries.

mod common;

use std::time::Duration;

use aqr_bundle::packs::{self, Pack};
use aqr_testkit::{damaged, within};
use common::{check, kind, rebuilds};

const LIMIT: Duration = Duration::from_secs(20);

fn data() -> Vec<u8> {
    (0..200u32).map(|i| (i * 7 % 251) as u8).collect()
}

/// Entries `(name, offset, size)` of the data (offsets from its start, as the cases tell them).
type Entries<'a> = &'a [(&'a str, u64, u64)];

/// The cases, as offsets within 200 bytes of data.
fn cases() -> [Vec<(&'static str, u64, u64)>; 8] {
    [
        vec![("a", 0, 100), ("b", 50, 100)],
        vec![("a", 0, 100), ("b", 0, 100)],
        vec![("a", 0, 100), ("b", 0, 50), ("c", 28, 10)],
        vec![("a", 0, 0), ("b", 8, 0), ("c", 20, 50)],
        vec![("a", 0, 100), ("a", 100, 100)],
        vec![("a", 0, 200), ("b", 199, 1)],
        vec![("a", 150, 100), ("b", 0, 10)],
        vec![("a", 0, 200), ("b", 0, 200), ("c", 0, 200)],
    ]
}

/// A Godot pack of version 1: the directory, then `data()`, the entries' offsets absolute.
fn godot1(entries: Entries) -> Vec<u8> {
    let mut d = b"GDPC".to_vec();
    for v in [1u32, 3, 2, 1] {
        d.extend(v.to_le_bytes());
    }
    d.extend([0; 64]);
    let paths: Vec<String> = entries.iter().map(|(n, ..)| format!("res://{n}")).collect();
    let dir = 4 + paths.iter().map(|p| 4 + p.len().next_multiple_of(4) + 32).sum::<usize>();
    let base = (d.len() + dir) as u64;
    d.extend((entries.len() as u32).to_le_bytes());
    for (p, (_, o, s)) in paths.iter().zip(entries) {
        let mut path = p.clone().into_bytes();
        path.resize(path.len().next_multiple_of(4), 0);
        d.extend((path.len() as u32).to_le_bytes());
        d.extend(path);
        d.extend((base + o).to_le_bytes());
        d.extend(s.to_le_bytes());
        d.extend([0xA5; 16]);
    }
    assert_eq!(d.len() as u64, base);
    d.extend(data());
    d
}

/// A VPK directory file (version 1) whose files, all in it, are `entries` of `data()`.
fn vpk1(entries: Entries) -> Vec<u8> {
    let mut tree = Vec::new();
    for (name, o, s) in entries {
        for part in ["bin", " ", name] {
            tree.extend(part.as_bytes());
            tree.push(0);
        }
        tree.extend(0xDEADBEEFu32.to_le_bytes());
        tree.extend(0u16.to_le_bytes());
        tree.extend(0x7fffu16.to_le_bytes());
        tree.extend((*o as u32).to_le_bytes());
        tree.extend((*s as u32).to_le_bytes());
        tree.extend([0xff, 0xff]);
        tree.extend([0, 0]);
    }
    tree.push(0);
    let mut d = vec![0x34, 0x12, 0xAA, 0x55, 1, 0, 0, 0];
    d.extend((tree.len() as u32).to_le_bytes());
    d.extend(tree);
    d.extend(data());
    d
}

/// A Doom WAD whose lumps are `entries` of `data()` (at 12), the directory after it.
fn doom(entries: Entries) -> Vec<u8> {
    let data = data();
    let mut d = b"PWAD".to_vec();
    d.extend((entries.len() as u32).to_le_bytes());
    d.extend((12 + data.len() as u32).to_le_bytes());
    d.extend(&data);
    for (n, o, s) in entries {
        d.extend((12 + *o as u32).to_le_bytes());
        d.extend((*s as u32).to_le_bytes());
        let mut name = [0u8; 8];
        name[..n.len()].copy_from_slice(n.as_bytes());
        d.extend(name);
    }
    d
}

fn run(pack: Pack, build: fn(Entries) -> Vec<u8>) {
    for (i, entries) in cases().iter().enumerate() {
        let d = build(entries);
        assert!(pack.starts(&d), "{} {i}", pack.name());
        within(LIMIT, || {
            assert!(check(pack, &d), "{} {i} read", pack.name());
            if let Some(f) = packs::encode_with(pack, &d, &aqr_formula::Kinds::new(kind)) {
                rebuilds(&f, &d);
            }
        })
        .unwrap_or_else(|e| panic!("{} case {i}: {e}", pack.name()));
        damaged(&d, 40, 40, LIMIT, |v| {
            check(pack, v);
        });
    }
}

#[test]
fn godot_entries() {
    run(Pack::GodotPck, godot1);
}

#[test]
fn vpk_entries() {
    run(Pack::SourceVpk, vpk1);
}

#[test]
fn doom_entries() {
    run(Pack::DoomWad, doom);
}

/// What the readers take of them: every entry within the file (Godot's and the VPK's empty
/// ones too, as files; the Doom WAD's empty lumps are markers, left out), each as it lies, so
/// that entries overlapping or repeating are taken as they are.
#[test]
fn what_is_taken() {
    let n = |pack: Pack, b: fn(Entries) -> Vec<u8>| -> Vec<usize> {
        cases().iter().map(|e| pack.files(&b(e)).expect("read").len()).collect()
    };
    // (case 6's first entry lies past the end of the file)
    assert_eq!(n(Pack::GodotPck, godot1), [2, 2, 3, 3, 2, 2, 1, 3]);
    assert_eq!(n(Pack::SourceVpk, vpk1), [2, 2, 3, 3, 2, 2, 1, 3]);
    assert_eq!(n(Pack::DoomWad, doom), [2, 2, 3, 1, 2, 2, 1, 3]);
    // the extents are those the entries tell
    let d = godot1(&cases()[2]);
    let f = Pack::GodotPck.files(&d).unwrap();
    let base = d.len() - 200;
    let got: Vec<_> = f.iter().map(|f| f.extents[0].clone()).collect();
    assert_eq!(got, [base..base + 100, base..base + 50, base + 28..base + 38]);
}
