//! What the tests of the packs share: the checks of what a reader makes of bytes, and the
//! builders of valid packs that several tests start from.
#![allow(dead_code)]

use aqr_archive::Evaluate;
use aqr_archive::Level;
use aqr_bundle::packs::{self, Pack};
use aqr_formula::{Formula, Kind};
use aqr_testkit::Refusing;

pub const LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

pub fn none(_: &aqr_formula::Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!("no module")
}

/// `f` evaluated, and archived and decoded, both `d`.
pub fn rebuilds(f: &Formula, d: &[u8]) {
    assert!(f.eval(&none).unwrap() == d, "evaluated");
    let c = aqr_archive::encode(f, None, Level::LOW).unwrap();
    let runner = aqr_archive::natives(&[], &none);
    assert!(aqr_archive::decode(&c, &runner).unwrap() == d, "archived");
}

pub fn kind(_: &[u8]) -> Kind {
    Kind::Unknown
}

/// What a reader makes of `d` must be right: extents within `d`, a formula rebuilding it.
/// Whether it read it.
pub fn check(pack: Pack, d: &[u8]) -> bool {
    let Some(files) = pack.files(d) else { return false };
    for f in &files {
        for e in &f.extents {
            assert!(e.start <= e.end && e.end <= d.len(), "{} extent {e:?} of {}", pack.name(), d.len());
        }
    }
    if let Some(f) = packs::encode_with(pack, d, &aqr_formula::Kinds::new(kind)) {
        assert!(f.eval(&none).unwrap() == d, "{} formula", pack.name());
    }
    true
}

/// Contents to pack: their names and bytes.
pub fn contents() -> Vec<(&'static str, Vec<u8>)> {
    let text =
        |n: usize, seed: u8| -> Vec<u8> { (0..n).map(|i| b"lorem ipsum "[(i * 7 + seed as usize) % 12]).collect() };
    vec![
        ("art/a.png", (0..300u32).map(|i| (i * 31 % 251) as u8).collect()),
        ("notes/readme.txt", text(77, 1)),
        ("empty.bin", Vec::new()),
        ("data/b.bin", (0..1000u32).map(|i| (i * i % 253) as u8).collect()),
    ]
}

pub fn pad(v: &mut Vec<u8>, to: usize) {
    v.resize(v.len().next_multiple_of(to), 0);
}

/// A Godot pack of `version` (1 to 3) of `files`, the last of version 2 on encrypted (flags 1).
pub fn godot(version: u32, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut d = b"GDPC".to_vec();
    for v in [version, if version == 1 { 3 } else { 4 }, 2, 1] {
        d.extend(v.to_le_bytes());
    }
    // (where the base, and from version 3 the directory's offset, go)
    let fields = d.len();
    if version >= 2 {
        d.extend((if version >= 3 { 2u32 } else { 0 }).to_le_bytes());
        d.extend([0; 8]);
    }
    if version >= 3 {
        d.extend([0; 8]);
    }
    d.extend([0; 64]);
    let entry_len =
        |name: &str| 4 + format!("res://{name}").len().next_multiple_of(4) + 32 + 4 * (version >= 2) as usize;
    let dir_len = 4 + files.iter().map(|(n, _)| entry_len(n)).sum::<usize>();
    let base = match version {
        1 => 0,
        2 => (d.len() + dir_len).next_multiple_of(16),
        _ => d.len().next_multiple_of(16),
    };
    // the data, each file's offset from the base, then the directory
    let at_data = if version >= 3 { base } else { (d.len() + dir_len).next_multiple_of(16) };
    let (mut data, mut offsets) = (Vec::new(), Vec::new());
    for (_, b) in files {
        offsets.push(at_data + data.len() - base);
        data.extend_from_slice(b);
        pad(&mut data, 16);
    }
    let mut dir = (files.len() as u32).to_le_bytes().to_vec();
    for (i, (n, b)) in files.iter().enumerate() {
        let mut path = format!("res://{n}").into_bytes();
        pad(&mut path, 4);
        dir.extend((path.len() as u32).to_le_bytes());
        dir.extend(&path);
        dir.extend((offsets[i] as u64).to_le_bytes());
        dir.extend((b.len() as u64).to_le_bytes());
        dir.extend([0xA5; 16]);
        if version >= 2 {
            dir.extend((u32::from(i == files.len() - 1)).to_le_bytes());
        }
    }
    if version >= 2 {
        d[fields + 4..fields + 12].copy_from_slice(&(base as u64).to_le_bytes());
    }
    if version >= 3 {
        let at = (at_data + data.len()) as u64;
        d[fields + 12..fields + 20].copy_from_slice(&at.to_le_bytes());
        pad(&mut d, 16);
        d.extend(data);
        d.extend(dir);
    } else {
        d.extend(dir);
        pad(&mut d, 16);
        d.extend(data);
    }
    d
}

/// A VPK directory file (version 1) of `files`: the first two of their data in it (the second
/// with 3 preload bytes), the others in archive 0, made too.
pub fn vpk_of(files: &[(&str, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
    let (mut tree, mut here, mut archive) = (Vec::new(), Vec::<u8>::new(), Vec::<u8>::new());
    for (i, (name, b)) in files.iter().enumerate() {
        let (path, ext) = name.rsplit_once('.').unwrap();
        let (dir, base) = path.rsplit_once('/').unwrap_or((" ", path));
        // (each file an extension and a directory of its own)
        for s in [ext, dir, base] {
            tree.extend(s.as_bytes());
            tree.push(0);
        }
        let preload = if i == 1 { 3 } else { 0 };
        let (index, at) = match i < 2 {
            true => (0x7fffu16, here.len()),
            false => (0, archive.len()),
        };
        tree.extend(0xDEADBEEFu32.to_le_bytes());
        tree.extend((preload as u16).to_le_bytes());
        tree.extend(index.to_le_bytes());
        tree.extend((at as u32).to_le_bytes());
        tree.extend(((b.len() - preload) as u32).to_le_bytes());
        tree.extend([0xff, 0xff]);
        tree.extend(&b[..preload]);
        match i < 2 {
            true => here.extend(&b[preload..]),
            false => archive.extend(&b[preload..]),
        }
        tree.extend([0, 0]);
    }
    tree.push(0);
    let mut d = vec![0x34, 0x12, 0xAA, 0x55, 1, 0, 0, 0];
    d.extend((tree.len() as u32).to_le_bytes());
    d.extend(tree);
    d.extend(here);
    (d, archive)
}

pub fn wad(magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut dir = Vec::new();
    for (n, b) in lumps {
        dir.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
        dir.extend_from_slice(&(b.len() as u32).to_le_bytes());
        let mut name = [0u8; 8];
        name[..n.len()].copy_from_slice(n.as_bytes());
        dir.extend_from_slice(&name);
        data.extend_from_slice(b);
    }
    let mut out = magic.to_vec();
    out.extend_from_slice(&(lumps.len() as u32).to_le_bytes());
    out.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
    out.extend(data);
    out.extend(dir);
    out
}

/// `check` of `pack` on `d` damaged in every way `aqr_testkit::damages` lists (`per` places for
/// each way, the first `head` bytes all): each must end in a read that is right, or none.
pub fn damaged_pack(pack: Pack, d: &[u8], per: usize, head: usize) {
    assert!(check(pack, d), "{} reads its valid pack", pack.name());
    aqr_testkit::damaged(d, per, head, LIMIT, |v| {
        check(pack, v);
    });
}
