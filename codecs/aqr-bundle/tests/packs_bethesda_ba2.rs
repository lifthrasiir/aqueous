use aqr_bundle::packs::bethesda_ba2::{files, starts};
use aqr_disk::Compressed;

fn p32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// A GNRL archive: (name, ext, data, packed?) files; the data is the stored bytes.
fn general(version: u32, items: &[(&str, &str, &[u8], Option<u32>)], names: bool) -> Vec<u8> {
    let header = match version {
        2 => 32,
        3 => 36,
        _ => 24,
    };
    let mut at = header + items.len() * 36;
    let mut records = Vec::new();
    let mut data = Vec::new();
    for (_, ext, d, unpacked) in items {
        p32(&mut records, 0);
        let mut e = ext.as_bytes().to_vec();
        e.resize(4, 0);
        records.extend_from_slice(&e);
        p32(&mut records, 0);
        p32(&mut records, 0);
        records.extend_from_slice(&(at as u64).to_le_bytes());
        match unpacked {
            Some(u) => {
                p32(&mut records, d.len() as u32);
                p32(&mut records, *u);
            }
            None => {
                p32(&mut records, 0);
                p32(&mut records, d.len() as u32);
            }
        }
        p32(&mut records, 0xBAAD_F00D);
        at += d.len();
        data.extend_from_slice(d);
    }
    let mut v = b"BTDX".to_vec();
    p32(&mut v, version);
    v.extend_from_slice(b"GNRL");
    p32(&mut v, items.len() as u32);
    v.extend_from_slice(&(if names { at as u64 } else { 0 }).to_le_bytes());
    if version == 2 || version == 3 {
        v.extend_from_slice(&1u64.to_le_bytes());
    }
    if version == 3 {
        p32(&mut v, 3);
    }
    v.extend_from_slice(&records);
    v.extend_from_slice(&data);
    if names {
        for (n, _, _, _) in items {
            v.extend_from_slice(&(n.len() as u16).to_le_bytes());
            v.extend_from_slice(n.as_bytes());
        }
    }
    v
}

#[test]
fn general_versions() {
    let items: [(&str, &str, &[u8], Option<u32>); 3] =
        [(r"a\b.txt", "txt", b"hello", None), ("c.bin", "bin", b"ZZZZ", Some(100)), ("d.x", "x", b"", None)];
    for version in [1, 2, 3, 7, 8] {
        let d = general(version, &items, true);
        assert!(starts(&d));
        let f = files(&d).unwrap_or_else(|| panic!("version {version}"));
        assert_eq!(f.len(), 2, "{version}");
        assert_eq!(f[0].name, b"a/b.txt");
        assert_eq!(&d[f[0].extents[0].clone()], b"hello");
        assert!(f[0].compressed.is_none());
        assert_eq!(&d[f[1].extents[0].clone()], b"ZZZZ");
        // Version 3 is LZ4 here; the others zlib.
        match f[1].compressed {
            Some(Compressed::Opaque { len: 100 }) => assert_eq!(version, 3),
            None => assert_ne!(version, 3),
            _ => panic!(),
        }
    }
    let d = general(1, &items, false);
    assert_eq!(files(&d).unwrap()[0].name, b"00000000.txt");
}

fn texture(version: u32, chunks: &[(Option<u32>, &[u8])]) -> Vec<u8> {
    let mut v = b"BTDX".to_vec();
    p32(&mut v, version);
    v.extend_from_slice(b"DX10");
    p32(&mut v, 1);
    let name = br"t\x.dds";
    let header = 24 + if version == 2 { 8 } else { 0 };
    let data_at = header + 24 + chunks.len() * 24;
    let total: usize = chunks.iter().map(|c| c.1.len()).sum();
    v.extend_from_slice(&((data_at + total) as u64).to_le_bytes());
    if version == 2 {
        v.extend_from_slice(&1u64.to_le_bytes());
    }
    p32(&mut v, 0);
    v.extend_from_slice(b"dds\0");
    p32(&mut v, 0);
    v.extend_from_slice(&[0, chunks.len() as u8, 24, 0, 4, 0, 4, 0, 3, 71, 0, 8]);
    let mut at = data_at;
    for (packed, c) in chunks {
        v.extend_from_slice(&(at as u64).to_le_bytes());
        p32(&mut v, packed.map_or(0, |_| c.len() as u32));
        p32(&mut v, packed.unwrap_or(c.len() as u32));
        v.extend_from_slice(&[0, 0, 2, 0]);
        p32(&mut v, 0xBAAD_F00D);
        at += c.len();
    }
    for (_, c) in chunks {
        v.extend_from_slice(c);
    }
    v.extend_from_slice(&(name.len() as u16).to_le_bytes());
    v.extend_from_slice(name);
    v
}

#[test]
fn textures() {
    for version in [1, 2] {
        let d = texture(version, &[(None, b"MIP0MIP0"), (None, b"MIP1")]);
        let f = files(&d).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, b"t/x.dds.dx10chunks");
        assert_eq!(f[0].extents.len(), 2);
        assert_eq!(&d[f[0].extents[0].clone()], b"MIP0MIP0");
        assert_eq!(&d[f[0].extents[1].clone()], b"MIP1");
        // A packed chunk leaves the texture out.
        let d = texture(version, &[(None, b"MIP0MIP0"), (Some(40), b"MIP1")]);
        assert!(files(&d).unwrap().is_empty());
    }
}

#[test]
fn cut_and_not_a_pack() {
    let packs = [
        general(3, &[(r"a\b.txt", "txt", b"hello", None), ("c.bin", "bin", b"ZZZZ", Some(100))], true),
        texture(1, &[(None, b"MIP0MIP0"), (None, b"MIP1")]),
    ];
    for d in packs {
        for i in 0..d.len() {
            for v in [0u8, 1, 0x7f, 0xff] {
                let mut m = d.clone();
                m[i] = v;
                let _ = files(&m);
            }
        }
        for n in 0..d.len() {
            if let Some(f) = files(&d[..n]) {
                for file in f {
                    assert!(file.extents.iter().all(|e| e.end <= n));
                }
            }
        }
    }
    assert!(!starts(b"BTDX\x09\0\0\0GNRL"));
    assert!(files(b"nothing here at all").is_none());
}
