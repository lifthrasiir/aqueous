use aqr_bundle::packs::mtf_arc::{files, starts};

fn pack(version: u16, items: &[(&str, u32, &[u8], u32)]) -> Vec<u8> {
    let mut v = b"ARC\0".to_vec();
    v.extend_from_slice(&version.to_le_bytes());
    v.extend_from_slice(&(items.len() as u16).to_le_bytes());
    let mut at = 8 + items.len() * 80;
    for (name, hash, data, size) in items {
        let mut n = name.as_bytes().to_vec();
        n.resize(64, 0);
        v.extend_from_slice(&n);
        v.extend_from_slice(&hash.to_le_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(&(at as u32).to_le_bytes());
        at += data.len();
    }
    for i in items {
        v.extend_from_slice(i.2);
    }
    v
}

#[test]
fn arc() {
    for version in [7, 9] {
        let d = pack(
            version,
            &[
                (r"model\em000", 0x2345_6789, b"MOD\0stored", 10),
                (r"tex\t", 0xdead_beef, &[0x78, 0x9c, 1, 2], 0x2000_0100),
            ],
        );
        assert!(starts(&d));
        let f = files(&d).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, b"model/em000.23456789");
        assert_eq!(&d[f[0].extents[0].clone()], b"MOD\0stored");
        assert_eq!(f[1].name, b"tex/t.deadbeef");
        assert_eq!(&d[f[1].extents[0].clone()], &[0x78, 0x9c, 1, 2]);
        assert!(f[1].compressed.is_none());
    }
}

#[test]
fn cut_and_not_a_pack() {
    let d = pack(7, &[("a", 1, b"xyz", 3), ("bb", 2, b"1234567", 99)]);
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            assert!(f.iter().all(|f| f.extents[0].end <= n));
        }
    }
    for i in 0..d.len() {
        for v in [0u8, 1, 0x7f, 0xff] {
            let mut m = d.clone();
            m[i] = v;
            let _ = files(&m);
        }
    }
    assert!(!starts(b"junk"));
    assert!(files(b"junk").is_none());
}
