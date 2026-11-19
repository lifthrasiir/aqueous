//! The readers of game packs (`aqr_bundle::packs`) on packs written here by their formats'
//! descriptions: the files taken and where, and the formulas rebuilding the packs.

use std::ops::Range;

mod common;

use aqr_bundle::packs::{self, Pack, vpk};
use aqr_formula::Kind;
use common::{contents, godot, pad, rebuilds, vpk_of};

/// The files of pack `d` of `pack` as names and extents.
fn taken(pack: Pack, d: &[u8]) -> Vec<(String, Vec<Range<usize>>)> {
    let files = pack.files(d).expect("read");
    files.into_iter().map(|f| (String::from_utf8(f.name).unwrap(), f.extents)).collect()
}

/// The pack's formula, of kinds by extension (`.txt` text), rebuilding it, and the classes of
/// its streams.
fn formula(pack: Pack, d: &[u8]) -> Vec<String> {
    let kind = |n: &[u8]| if n.ends_with(b".txt") { Kind::Text } else { Kind::Unknown };
    assert_eq!(packs::detect(d), Some(pack));
    let f = packs::encode_with(pack, d, kind).expect("a formula");
    rebuilds(&f, d);
    f.streams.iter().map(|s| s.class.to_string()).collect()
}

#[test]
fn godot_packs() {
    let c = contents();
    for version in 1..=3 {
        let d = godot(version, &c);
        let files = taken(Pack::GodotPck, &d);
        // (the encrypted one left out, from version 2)
        let n = if version == 1 { c.len() } else { c.len() - 1 };
        assert_eq!(files.len(), n, "{version}");
        for ((name, extents), (want, bytes)) in files.iter().zip(&c) {
            assert_eq!(name, want);
            assert_eq!(&d[extents[0].clone()], &bytes[..], "{version} {name}");
        }
        let classes = formula(Pack::GodotPck, &d);
        assert!(
            classes.contains(&"godot-pck/rest".into()) && classes.contains(&"godot-pck/text".into()),
            "{classes:?}"
        );
    }
    // a directory encrypted is not read
    let mut d = godot(2, &c);
    d[20] |= 1;
    assert!(Pack::GodotPck.files(&d).is_none());
}

/// `d` as an LZ4 block of literals only.
fn lz4_literals(d: &[u8]) -> Vec<u8> {
    let mut out = vec![(d.len().min(15) as u8) << 4];
    if d.len() >= 15 {
        let mut n = d.len() - 15;
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
    }
    out.extend_from_slice(d);
    out
}

/// A Unity bundle (format 7) of two blocks, the first stored, holding `files` but the last,
/// the second (of LZ4: its bytes made up) the last; its table compressed by LZ4 where `lz4`.
fn unity(files: &[(&str, Vec<u8>)], lz4: bool) -> Vec<u8> {
    let (head, last) = files.split_at(files.len() - 1);
    let first: Vec<u8> = head.iter().flat_map(|(_, b)| b.iter().copied()).collect();
    let packed = vec![0x5Au8; last[0].1.len() / 2 + 1];
    let mut table = vec![0x11; 16];
    table.extend(2u32.to_be_bytes());
    for (size, packed, flags) in [(first.len(), first.len(), 0u16), (last[0].1.len(), packed.len(), 2)] {
        table.extend((size as u32).to_be_bytes());
        table.extend((packed as u32).to_be_bytes());
        table.extend(flags.to_be_bytes());
    }
    table.extend((files.len() as u32).to_be_bytes());
    let mut at = 0u64;
    for (name, b) in files {
        table.extend(at.to_be_bytes());
        table.extend((b.len() as u64).to_be_bytes());
        table.extend(4u32.to_be_bytes());
        table.extend(name.as_bytes());
        table.push(0);
        at += b.len() as u64;
    }
    let stored = if lz4 { lz4_literals(&table) } else { table.clone() };
    let mut d = b"UnityFS\0".to_vec();
    d.extend(7u32.to_be_bytes());
    d.extend(b"5.x.x\0" as &[u8]);
    d.extend(b"2019.4.1f1\0" as &[u8]);
    let size_at = d.len();
    d.extend([0; 8]);
    d.extend((stored.len() as u32).to_be_bytes());
    d.extend((table.len() as u32).to_be_bytes());
    d.extend((0x40 | if lz4 { 2u32 } else { 0 }).to_be_bytes());
    pad(&mut d, 16);
    d.extend(stored);
    d.extend(first);
    d.extend(packed);
    let n = d.len() as u64;
    d[size_at..size_at + 8].copy_from_slice(&n.to_be_bytes());
    d
}

#[test]
fn unity_bundles() {
    let c = contents();
    for lz4 in [false, true] {
        let d = unity(&c, lz4);
        let files = taken(Pack::UnityBundle, &d);
        // (the empty node and the one in the compressed block left out)
        let names: Vec<&str> = files.iter().map(|f| &f.0[..]).collect();
        assert_eq!(names, ["art/a.png", "notes/readme.txt"], "{lz4}");
        for (name, extents) in &files {
            let want = &c.iter().find(|f| f.0 == name).unwrap().1;
            assert_eq!(&d[extents[0].clone()], &want[..]);
        }
        let classes = formula(Pack::UnityBundle, &d);
        assert!(classes.contains(&"unity-bundle/rest".into()), "{classes:?}");
    }
}

/// An `FString` of `s`, of a NUL.
fn fstring(s: &str) -> Vec<u8> {
    let mut v = ((s.len() + 1) as u32).to_le_bytes().to_vec();
    v.extend(s.as_bytes());
    v.push(0);
    v
}

/// An `FPakEntry` of version 3 to 11 (of a method of u32): stored, or of one zlib block (from
/// 8 the method's index 1) of its bytes made up; flags 1 where `encrypted`.
fn pak_entry(offset: u64, size: usize, compressed: bool, encrypted: bool) -> Vec<u8> {
    let mut e = Vec::new();
    e.extend(offset.to_le_bytes());
    e.extend((size as u64).to_le_bytes());
    e.extend(((size * if compressed { 3 } else { 1 }) as u64).to_le_bytes());
    e.extend(u32::from(compressed).to_le_bytes());
    e.extend([0x5C; 20]);
    if compressed {
        e.extend(1u32.to_le_bytes());
        e.extend(0u64.to_le_bytes());
        e.extend((size as u64).to_le_bytes());
    }
    e.push(u8::from(encrypted));
    e.extend(65536u32.to_le_bytes());
    e
}

/// An Unreal pak of `version` (3 or 11) of `files`: the first compressed (zlib), the last
/// encrypted.
fn unreal(version: u32, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut d = Vec::new();
    let mut entries = Vec::new();
    for (i, (_, b)) in files.iter().enumerate() {
        let (compressed, encrypted) = (i == 0, i == files.len() - 1);
        let offset = d.len() as u64;
        // (the copy before the data: offset 0, as Unreal writes it)
        d.extend(pak_entry(0, b.len(), compressed, encrypted));
        d.extend(b);
        entries.push(pak_entry(offset, b.len(), compressed, encrypted));
    }
    let mount = "../../../Game/";
    let mut index = fstring(mount);
    index.extend((files.len() as u32).to_le_bytes());
    let mut directory = Vec::new();
    if version < 10 {
        for ((name, _), e) in files.iter().zip(&entries) {
            index.extend(fstring(name));
            index.extend(e);
        }
    } else {
        // (stored ones encoded, the others kept: encoded entries can be of zlib blocks too)
        let mut codes = Vec::new();
        let (mut locs, mut plain) = (Vec::new(), Vec::new());
        for (i, ((_, b), e)) in files.iter().zip(&entries).enumerate() {
            if i == 0 || i == files.len() - 1 {
                locs.push(-1 - plain.len() as i32);
                plain.push(e.clone());
            } else {
                locs.push(codes.len() as i32);
                codes.extend((1u32 << 31 | 1 << 30 | 1 << 29).to_le_bytes());
                codes.extend((u64::from_le_bytes(e[..8].try_into().unwrap()) as u32).to_le_bytes());
                codes.extend((b.len() as u32).to_le_bytes());
            }
        }
        index.extend(0x1234u64.to_le_bytes());
        index.extend(0u32.to_le_bytes());
        // (the full directory index's place, filled below)
        index.extend(1u32.to_le_bytes());
        let place = index.len();
        index.extend([0; 36]);
        index.extend((codes.len() as u32).to_le_bytes());
        index.extend(codes);
        index.extend((plain.len() as u32).to_le_bytes());
        plain.iter().for_each(|e| index.extend(e));
        // the full directory index: each directory's files and their entries' places
        let mut dirs: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, (name, _)) in files.iter().enumerate() {
            let dir = name.rfind('/').map_or("", |k| &name[..=k]);
            match dirs.iter_mut().find(|d| d.0 == dir) {
                Some(d) => d.1.push(i),
                None => dirs.push((dir, vec![i])),
            }
        }
        directory.extend((dirs.len() as u32).to_le_bytes());
        for (dir, is) in &dirs {
            directory.extend(fstring(dir));
            directory.extend((is.len() as u32).to_le_bytes());
            for &i in is {
                directory.extend(fstring(&files[i].0[dir.len()..]));
                directory.extend(locs[i].to_le_bytes());
            }
        }
        let at = (d.len() + index.len()) as u64;
        index[place..place + 8].copy_from_slice(&at.to_le_bytes());
        index[place + 8..place + 16].copy_from_slice(&(directory.len() as u64).to_le_bytes());
    }
    let at = d.len() as u64;
    let n = index.len() as u64;
    d.extend(index);
    d.extend(directory);
    if version >= 7 {
        d.extend([0; 17]);
    }
    d.extend([0xE1, 0x12, 0x6F, 0x5A]);
    d.extend(version.to_le_bytes());
    d.extend(at.to_le_bytes());
    d.extend(n.to_le_bytes());
    d.extend([0x3C; 20]);
    if version >= 8 {
        let mut names = [0u8; 160];
        names[..4].copy_from_slice(b"Zlib");
        d.extend(names);
    }
    d
}

#[test]
fn unreal_paks() {
    let c = contents();
    for version in [3, 11] {
        let d = unreal(version, &c);
        let files = taken(Pack::UnrealPak, &d);
        // (the encrypted one and the empty one left out; the zlib one of its compressed bytes)
        let names: Vec<&str> = files.iter().map(|f| &f.0[..]).collect();
        assert_eq!(names, ["../../../Game/art/a.png", "../../../Game/notes/readme.txt"], "{version}");
        for (name, extents) in &files {
            let want = &c.iter().find(|f| name.ends_with(f.0)).unwrap().1;
            assert_eq!(&d[extents[0].clone()], &want[..], "{version} {name}");
        }
        let classes = formula(Pack::UnrealPak, &d);
        assert!(classes.contains(&"unreal-pak/rest".into()), "{classes:?}");
    }
    // an encrypted index is not read
    let mut d = unreal(11, &c);
    let magic = d.len() - 204;
    d[magic - 1] = 1;
    assert!(Pack::UnrealPak.files(&d).is_none());
}

#[test]
fn source_vpks() {
    let c = contents();
    let (d, archive) = vpk_of(&c);
    let files = taken(Pack::SourceVpk, &d);
    let names: Vec<&str> = files.iter().map(|f| &f.0[..]).collect();
    assert_eq!(names, ["art/a.png", "notes/readme.txt"]);
    for (name, extents) in &files {
        let want = &c.iter().find(|f| f.0 == name).unwrap().1;
        let got: Vec<u8> = extents.iter().flat_map(|r| d[r.clone()].iter().copied()).collect();
        assert_eq!(&got[..], &want[..], "{name}");
    }
    formula(Pack::SourceVpk, &d);
    // the archive, by the tree (the empty file left out)
    let (entries, _) = vpk::tree(&d).unwrap();
    assert_eq!(vpk::archive_len(&entries, 0), Some(archive.len()));
    let files = vpk::archive_files(&entries, 0, archive.len());
    let names: Vec<&[u8]> = files.iter().map(|f| &f.name[..]).collect();
    assert_eq!(names, [b"data/b.bin"]);
    assert_eq!(&archive[files[0].extents[0].clone()], &c[3].1[..]);
    // (its one file all of it: no formula, which would give the same data again)
    assert!(packs::encode_files(Pack::SourceVpk, &archive, &files, |_| Kind::Unknown).is_none());
    // with bytes the tree tells of no file after it, the file a content of its own
    let mut longer = archive.clone();
    longer.extend_from_slice(b"padding no file holds");
    let f = packs::encode_files(Pack::SourceVpk, &longer, &files, |_| Kind::Unknown).unwrap();
    rebuilds(&f, &longer);
}

/// No pack is read of data cut short or of no pack.
#[test]
fn not_packs() {
    let c = contents();
    for d in [godot(3, &c), unity(&c, true), unreal(11, &c), vpk_of(&c).0] {
        for cut in [0, 7, 20, 40, d.len() / 2, d.len() - 1] {
            let d = &d[..cut];
            for p in Pack::ALL {
                // (whatever it starts like, cut short it reads nothing, or what fits)
                if let Some(files) = p.files(d) {
                    assert!(files.iter().all(|f| f.extents.iter().all(|r| r.end <= d.len())), "{p:?} {cut}");
                }
            }
        }
    }
    assert_eq!(packs::detect(b"GDPC\x09\0\0\0 not a pack"), None);
}
