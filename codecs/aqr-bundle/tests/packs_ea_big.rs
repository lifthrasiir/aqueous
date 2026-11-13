use aqr_bundle::packs::ea_big::{files, starts};
use aqr_disk::Compressed;

fn pack(magic: &[u8; 4], items: &[(&str, &[u8])]) -> Vec<u8> {
    let header = 16 + items.iter().map(|i| 9 + i.0.len()).sum::<usize>();
    let header = (header + 7) & !7;
    let total = header + items.iter().map(|i| i.1.len()).sum::<usize>();
    let mut v = magic.to_vec();
    v.extend_from_slice(&(total as u32).to_le_bytes());
    v.extend_from_slice(&(items.len() as u32).to_be_bytes());
    v.extend_from_slice(&(header as u32).to_be_bytes());
    let mut at = header;
    for (n, d) in items {
        v.extend_from_slice(&(at as u32).to_be_bytes());
        v.extend_from_slice(&(d.len() as u32).to_be_bytes());
        v.extend_from_slice(n.as_bytes());
        v.push(0);
        at += d.len();
    }
    v.resize(header, 0);
    for (_, d) in items {
        v.extend_from_slice(d);
    }
    v
}

#[test]
fn big() {
    let items: [(&str, &[u8]); 3] = [
        (r"data\a.txt", b"hello"),
        ("b.ref", &[0x10, 0xfb, 0, 1, 0, 9, 9, 9]),
        ("c.ref", &[0x90, 0xfb, 0, 0, 2, 0, 1]),
    ];
    for magic in [b"BIGF", b"BIG4"] {
        let d = pack(magic, &items);
        assert!(starts(&d));
        let f = files(&d).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].name, b"data/a.txt");
        assert_eq!(&d[f[0].extents[0].clone()], b"hello");
        assert!(f[0].compressed.is_none());
        assert!(matches!(f[1].compressed, Some(Compressed::Opaque { len: 256 })));
        assert!(matches!(f[2].compressed, Some(Compressed::Opaque { len: 512 })), "{:?}", f[2].compressed);
    }
}

#[test]
fn cut_and_not_a_pack() {
    let d = pack(b"BIGF", &[("a", b"xyz"), ("bb", b"1234567")]);
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
