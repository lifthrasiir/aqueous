use aqr_bundle::packs::gmod_gma;

fn gma(version: u8, files: &[(&str, &[u8])], crc: bool) -> Vec<u8> {
    let mut o = b"GMAD".to_vec();
    o.push(version);
    o.extend_from_slice(&76561198000000000u64.to_le_bytes());
    o.extend_from_slice(&1600000000u64.to_le_bytes());
    if version > 1 {
        o.extend_from_slice(b"content\0\0");
    }
    o.extend_from_slice(b"Name\0{\"description\":1}\0Author\0");
    o.extend_from_slice(&1i32.to_le_bytes());
    for (i, (n, b)) in files.iter().enumerate() {
        o.extend_from_slice(&(i as u32 + 1).to_le_bytes());
        o.extend_from_slice(n.as_bytes());
        o.push(0);
        o.extend_from_slice(&(b.len() as i64).to_le_bytes());
        o.extend_from_slice(&0xdeadbeefu32.to_le_bytes());
    }
    o.extend_from_slice(&0u32.to_le_bytes());
    for (_, b) in files {
        o.extend_from_slice(b);
    }
    if crc {
        o.extend_from_slice(&[1, 2, 3, 4]);
    }
    o
}

#[test]
fn reads() {
    for (version, crc) in [(1, false), (2, true), (3, true)] {
        let fs: &[(&str, &[u8])] =
            &[("lua/autorun/a.lua", b"print(1)"), ("materials/x.vmt", b""), ("sound/y.wav", b"RIFFabcd")];
        let d = gma(version, fs, crc);
        assert!(gmod_gma::starts(&d));
        let f = gmod_gma::files(&d).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, b"lua/autorun/a.lua");
        assert_eq!(&d[f[0].extents[0].clone()], b"print(1)");
        assert_eq!(&d[f[1].extents[0].clone()], b"RIFFabcd");
        for n in 0..d.len() {
            if let Some(f) = gmod_gma::files(&d[..n]) {
                assert!(f.iter().all(|f| f.extents.iter().all(|e| e.end <= n)));
            }
        }
    }
    assert!(gmod_gma::files(b"GMAD\x09").is_none() && gmod_gma::files(b"GMAD").is_none());
    assert!(gmod_gma::files(&[0; 90]).is_none());
}
