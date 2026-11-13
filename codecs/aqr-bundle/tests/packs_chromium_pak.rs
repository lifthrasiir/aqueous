use aqr_bundle::packs::chromium_pak;

fn pak(version: u32, res: &[(u16, &[u8])], aliases: &[(u16, u16)]) -> Vec<u8> {
    let header = if version == 4 { 9 } else { 12 };
    let table = header + 6 * (res.len() + 1) + 4 * aliases.len();
    let mut o = version.to_le_bytes().to_vec();
    if version == 4 {
        o.extend_from_slice(&(res.len() as u32).to_le_bytes());
        o.push(1);
    } else {
        o.extend_from_slice(&[1, 0, 0, 0]);
        o.extend_from_slice(&(res.len() as u16).to_le_bytes());
        o.extend_from_slice(&(aliases.len() as u16).to_le_bytes());
    }
    let mut at = table;
    for (id, b) in res {
        o.extend_from_slice(&id.to_le_bytes());
        o.extend_from_slice(&(at as u32).to_le_bytes());
        at += b.len();
    }
    o.extend_from_slice(&0u16.to_le_bytes());
    o.extend_from_slice(&(at as u32).to_le_bytes());
    for (id, i) in aliases {
        o.extend_from_slice(&id.to_le_bytes());
        o.extend_from_slice(&i.to_le_bytes());
    }
    for (_, b) in res {
        o.extend_from_slice(b);
    }
    o
}

#[test]
fn reads() {
    let mut br = vec![0x1e, 0x9b, 0x10, 0x27, 0, 0, 0, 0];
    br.extend_from_slice(&[0x1b; 20]);
    let res: Vec<(u16, &[u8])> = vec![
        (100, b"\x89PNG\r\n\x1a\nxx"),
        (101, b"<!doctype html><p>"),
        (102, b"hello world"),
        (103, &br),
        (105, b"\x1f\x8b\x08\x00zz"),
        (106, b""),
        (107, b"\x01\x02\x03"),
    ];
    for v in [4, 5] {
        let d = pak(v, &res, if v == 5 { &[(200, 1), (201, 2)] } else { &[] });
        assert!(chromium_pak::starts(&d));
        let f = chromium_pak::files(&d).unwrap();
        let names: Vec<_> = f.iter().map(|f| String::from_utf8(f.name.clone()).unwrap()).collect();
        assert_eq!(names, ["100.png", "101.html", "102.txt", "103.br", "105.gz", "107.bin"]);
        assert_eq!(&d[f[0].extents[0].clone()], res[0].1);
        assert!(matches!(f[3].compressed, Some(aqr_disk::Compressed::Opaque { len: 10000 })));
        assert!(f.iter().enumerate().all(|(i, f)| i == 3 || f.compressed.is_none()));
        for n in 0..d.len() {
            if let Some(f) = chromium_pak::files(&d[..n]) {
                assert!(f.iter().all(|f| f.extents.iter().all(|e| e.end <= n)));
            }
        }
    }
    assert!(chromium_pak::files(b"").is_none());
    assert!(chromium_pak::files(&[5, 0, 0, 0, 1, 9, 9, 9, 1, 0, 0, 0]).is_none());
    assert!(chromium_pak::files(b"PACKxxxxxxxxxxxxxxxxxxxxxxx").is_none());
}
