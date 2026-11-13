//! unreal_upk on packages written here from the format's description.

use aqr_bundle::packs::unreal_upk::{files, starts};
use aqr_disk::Compressed;

const MAGIC: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];

fn i32s(out: &mut Vec<u8>, v: &[i32]) {
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
}

fn fstring(out: &mut Vec<u8>, s: &str) {
    i32s(out, &[s.len() as i32 + 1]);
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

fn fstring16(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
    i32s(out, &[-(units.len() as i32)]);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

/// Offsets of the summary's fields to patch once the tables are laid out.
struct Summary {
    header: usize,
    tables: usize,
    depends: usize,
    thumbnails: Option<usize>,
    chunks: usize,
}

/// A summary of `version` with `chunks` (4 i32 each), its offsets 0 for now.
fn summary(version: u32, method: u32, chunks: &[[i32; 4]]) -> (Vec<u8>, Summary) {
    let mut d = MAGIC.to_vec();
    d.extend_from_slice(&(version | 3 << 16).to_le_bytes());
    let header = d.len();
    i32s(&mut d, &[0]);
    fstring(&mut d, "None");
    i32s(&mut d, &[0x8]);
    let tables = d.len();
    i32s(&mut d, &[0; 6]);
    let depends = d.len();
    i32s(&mut d, &[0]);
    if version >= 623 {
        i32s(&mut d, &[0, 0, 0]);
    }
    let thumbnails = (version >= 584).then(|| {
        i32s(&mut d, &[0]);
        d.len() - 4
    });
    d.extend_from_slice(&[0x5a; 16]);
    i32s(&mut d, &[2, 1, 2, 3, 4, 5, 6]);
    i32s(&mut d, &[10897, 136, method as i32, chunks.len() as i32]);
    let at = d.len();
    for c in chunks {
        i32s(&mut d, c);
    }
    (d, Summary { header, tables, depends, thumbnails, chunks: at })
}

fn put(d: &mut [u8], at: usize, v: usize) {
    d[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
}

/// An uncompressed package of `version`: names, imports of classes `Texture2D` and
/// `SoundNodeWave`, exports `Pack` (a Package), `Pack/Tex` (Texture2D), `Pack/Tex_1`
/// (SoundNodeWave), `Def` of class export `Pack`, an empty one, and a thumbnail where the version
/// has them. Returns it and the exports' expected (name, data).
fn package(version: u32) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let (mut d, s) = summary(version, 0, &[]);
    let names = ["None", "Core", "Class", "Package", "Engine", "Texture2D", "SoundNodeWave", "Pack", "Tex", "Def"];
    let names_at = d.len();
    for (k, n) in names.iter().enumerate() {
        if k == 9 {
            fstring16(&mut d, n);
        } else {
            fstring(&mut d, n);
        }
        d.extend_from_slice(&0x0007_0010_0000_0000u64.to_le_bytes());
    }
    let imports_at = d.len();
    for class in [5, 6] {
        i32s(&mut d, &[1, 0, 2, 0, 0, class, 0]);
    }
    let exports_at = d.len();
    let datas: Vec<Vec<u8>> = vec![vec![1; 12], vec![2; 300], vec![3; 50], vec![4; 7], vec![]];
    // (class, outer, name, number)
    let shape = [(0, 0, 7, 0), (-1, 1, 8, 0), (-2, 1, 8, 2), (1, 0, 9, 0), (-1, 0, 8, 0)];
    let mut fixups = Vec::new();
    for (k, &(class, outer, name, number)) in shape.iter().enumerate() {
        i32s(&mut d, &[class, 0, outer, name, number, 0]);
        d.extend_from_slice(&0x0007_0004_0000_0000u64.to_le_bytes());
        i32s(&mut d, &[datas[k].len() as i32]);
        fixups.push(d.len());
        i32s(&mut d, &[0, 0]);
        let net: &[i32] = if k == 1 { &[5, 6, 7] } else { &[] };
        i32s(&mut d, &[net.len() as i32]);
        i32s(&mut d, net);
        d.extend_from_slice(&[0; 16]);
        i32s(&mut d, &[0]);
    }
    let depends = d.len();
    i32s(&mut d, &[0; 5]);
    if let Some(at) = s.thumbnails {
        let data = d.len();
        i32s(&mut d, &[16, 16, 12]);
        d.extend_from_slice(b"\x89PNG\r\n\x1a\nIEND");
        let table = d.len();
        i32s(&mut d, &[1]);
        fstring(&mut d, "Texture2D");
        fstring(&mut d, "Pack.Tex");
        i32s(&mut d, &[data as i32]);
        put(&mut d, at, table);
    }
    let header = d.len();
    put(&mut d, s.header, header);
    for (k, v) in [names.len(), names_at, shape.len(), exports_at, 2, imports_at].into_iter().enumerate() {
        put(&mut d, s.tables + 4 * k, v);
    }
    put(&mut d, s.depends, depends);
    let mut expected = Vec::new();
    let paths = ["Pack.Class", "Pack/Tex.Texture2D", "Pack/Tex_1.SoundNodeWave", "Def.Pack", ""];
    for (k, data) in datas.iter().enumerate() {
        let at = d.len();
        put(&mut d, fixups[k], at);
        if !data.is_empty() {
            expected.push((paths[k].to_string(), data.clone()));
        }
        d.extend_from_slice(data);
    }
    if s.thumbnails.is_some() {
        expected.push(("thumbnails/Pack.Tex.png".into(), b"\x89PNG\r\n\x1a\nIEND".to_vec()));
    }
    (d, expected)
}

fn names_and_data(d: &[u8]) -> Vec<(String, Vec<u8>)> {
    let fs = files(d).expect("read");
    fs.iter()
        .map(|f| {
            assert_eq!(f.extents.len(), 1);
            (String::from_utf8(f.name.clone()).unwrap(), d[f.extents[0].clone()].to_vec())
        })
        .collect()
}

#[test]
fn uncompressed_packages() {
    for version in [500, 584, 623, 868, 893] {
        let (d, expected) = package(version);
        assert!(starts(&d), "{version}");
        let mut got = names_and_data(&d);
        let mut expected = expected;
        got.sort();
        expected.sort();
        assert_eq!(got, expected, "{version}");
    }
}

#[test]
fn a_table_of_other_fields_is_not_read() {
    let (mut d, _) = package(868);
    // the depends map moved: the exports' table does not end where it starts
    let at = 8 + 4 + 9 + 4 + 24;
    let v = u32::from_le_bytes(d[at..at + 4].try_into().unwrap());
    d[at..at + 4].copy_from_slice(&(v + 4).to_le_bytes());
    assert!(files(&d).is_none());
    // an export past the end
    let (mut d, _) = package(868);
    d.truncate(d.len() - 1);
    assert!(files(&d).is_none());
}

/// A chunk of `blocks` (each its compressed bytes and its size uncompressed) of block size `bs`.
fn chunk(bs: u32, blocks: &[(&[u8], u32)]) -> Vec<u8> {
    let mut c = MAGIC.to_vec();
    let size: usize = blocks.iter().map(|b| b.0.len()).sum();
    let raw: u32 = blocks.iter().map(|b| b.1).sum();
    i32s(&mut c, &[bs as i32, size as i32, raw as i32]);
    for b in blocks {
        i32s(&mut c, &[b.0.len() as i32, b.1 as i32]);
    }
    for b in blocks {
        c.extend_from_slice(b.0);
    }
    c
}

#[test]
fn compressed_packages() {
    let zlib: &[u8] = &[0x78, 0x9c, 3, 0, 0, 0, 0, 1];
    let lzo: &[u8] = &[0x11, 0, 0, 5, 6];
    for (method, block) in [(1, zlib), (2, lzo), (4, lzo)] {
        let chunks = [chunk(0x20000, &[(block, 0x20000), (block, 100)]), chunk(0x20000, &[(block, 0x400)])];
        let c = |k: usize, at: usize| [0x100 * k as i32, 0x20000, at as i32, chunks[k].len() as i32];
        let (mut d, s) = summary(868, method, &[c(0, 0), c(1, 0)]);
        let first = d.len() + 8;
        let second = first + chunks[0].len();
        put(&mut d, s.chunks + 8, first);
        put(&mut d, s.chunks + 24, second);
        d.extend_from_slice(&[0; 8]);
        for c in &chunks {
            d.extend_from_slice(c);
        }
        assert!(starts(&d));
        let fs = files(&d).unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].extents, vec![first..second]);
        assert_eq!(fs[1].extents, vec![second..d.len()]);
        if method == 1 {
            assert_eq!(fs[0].name, b"chunk0.zlib");
            assert!(fs.iter().all(|f| f.compressed.is_none()));
        } else {
            assert_eq!(fs[1].name, b"chunk1");
            assert_eq!(fs[0].compressed, Some(Compressed::Opaque { len: 0x20000 + 100 }));
            assert_eq!(fs[1].compressed, Some(Compressed::Opaque { len: 0x400 }));
        }
    }
}

#[test]
fn texture_file_caches() {
    let zlib: &[u8] = &[0x78, 0x9c, 3, 0, 0, 0, 0, 1];
    let lzo: &[u8] = &[0x11, 0, 0];
    let mut d = Vec::new();
    let parts =
        [chunk(0x20000, &[(lzo, 0x2000)]), chunk(0x20000, &[(zlib, 0x20000), (zlib, 9)]), chunk(0x20000, &[(lzo, 1)])];
    for p in &parts {
        d.extend_from_slice(p);
    }
    assert!(starts(&d));
    let fs = files(&d).unwrap();
    assert_eq!(fs.len(), 3);
    assert_eq!(fs[0].compressed, Some(Compressed::Opaque { len: 0x2000 }));
    assert_eq!(fs[1].name, b"chunk1.zlib");
    assert_eq!(fs[1].compressed, None);
    assert_eq!(fs[2].extents, vec![d.len() - parts[2].len()..d.len()]);
    // what follows the last chunk that is none is the rest
    d.extend_from_slice(&[0; 5]);
    assert_eq!(files(&d).unwrap().len(), 3);
}

#[test]
fn cut_short_and_not_packages() {
    let (pkg, _) = package(868);
    let lzo: &[u8] = &[0x11, 0, 0];
    let (mut comp, s) = summary(868, 2, &[[0, 0x2000, 0, 0]]);
    let c = chunk(0x20000, &[(lzo, 0x2000)]);
    let at = comp.len();
    put(&mut comp, s.chunks + 8, at);
    put(&mut comp, s.chunks + 12, c.len());
    comp.extend_from_slice(&c);
    let mut tfc = chunk(0x20000, &[(lzo, 0x2000)]);
    tfc.extend_from_slice(&chunk(0x20000, &[(lzo, 0x2000), (lzo, 7)]));
    for d in [&pkg, &comp, &tfc] {
        for n in 0..d.len() {
            let d = &d[..n];
            if let Some(fs) = files(d) {
                assert!(fs.iter().all(|f| f.extents.iter().all(|e| e.end <= d.len())));
            }
        }
    }
    assert!(!starts(b"PK\x03\x04 not a package at all"));
    assert!(files(b"PK\x03\x04 not a package at all").is_none());
    // UE4's legacy version, and a version out of range
    let mut ue4 = MAGIC.to_vec();
    ue4.extend_from_slice(&(-7i32).to_le_bytes());
    ue4.extend_from_slice(&[0; 64]);
    assert!(!starts(&ue4) && files(&ue4).is_none());
    let mut old = pkg.clone();
    old[4..6].copy_from_slice(&100u16.to_le_bytes());
    assert!(!starts(&old) && files(&old).is_none());
    // garbage after the magic
    for seed in 0..200u32 {
        let mut d = MAGIC.to_vec();
        d.extend_from_slice(&868u32.to_le_bytes());
        let mut x = seed.wrapping_mul(2654435761) | 1;
        for _ in 0..300 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            d.push(x as u8 & 0x0f);
        }
        let _ = files(&d);
    }
}

/// A chunk alone (a texture file cache of one, or a chunk taken from one as a content) is one
/// file of all of it: it has no formula, or its contents would be it again, without end.
#[test]
fn a_chunk_alone_has_no_formula() {
    use aqr_bundle::packs::{self, Pack};
    let lzo: &[u8] = &[0x11; 300];
    let one = chunk(0x20000, &[(lzo, 0x2000)]);
    assert_eq!(files(&one).map(|f| f.len()), Some(1));
    assert!(packs::encode_with(Pack::UnrealUpk, &one, |_| aqr_formula::Kind::Unknown).is_none());
    let mut two = one.clone();
    two.extend_from_slice(&chunk(0x20000, &[(lzo, 0x2000)]));
    assert!(packs::encode_with(Pack::UnrealUpk, &two, |_| aqr_formula::Kind::Unknown).is_some());
}
