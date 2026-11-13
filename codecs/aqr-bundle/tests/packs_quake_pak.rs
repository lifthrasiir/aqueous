use aqr_bundle::packs::quake_pak;

fn pack(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut dir = Vec::new();
    for (n, b) in files {
        let mut name = [0u8; 56];
        name[..n.len()].copy_from_slice(n.as_bytes());
        dir.extend_from_slice(&name);
        dir.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
        dir.extend_from_slice(&(b.len() as u32).to_le_bytes());
        data.extend_from_slice(b);
    }
    let mut out = b"PACK".to_vec();
    out.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend(data);
    out.extend(dir);
    out
}

#[test]
fn reads() {
    let d = pack(&[("maps/e1m1.bsp", b"BSPDATA"), ("progs.dat", b"xyz"), ("empty", b""), ("sound/a.wav", b"RIFF1234")]);
    assert!(quake_pak::starts(&d));
    let f = quake_pak::files(&d).unwrap();
    let got: Vec<_> = f.iter().map(|f| (String::from_utf8(f.name.clone()).unwrap(), f.extents[0].clone())).collect();
    assert_eq!(
        got,
        [("maps/e1m1.bsp".to_string(), 12..19), ("progs.dat".into(), 19..22), ("sound/a.wav".into(), 22..30)]
    );
    assert!(f.iter().all(|f| f.compressed.is_none()));
}

#[test]
fn truncated_and_foreign() {
    let d = pack(&[("a/b.txt", b"hello"), ("c.dat", b"world!")]);
    for n in 0..d.len() {
        if let Some(f) = quake_pak::files(&d[..n]) {
            assert!(f.iter().all(|f| f.extents.iter().all(|e| e.end <= n)));
        }
    }
    assert!(quake_pak::files(b"").is_none() && quake_pak::files(b"PACKxxxxxxxxxxxxxxxx").is_none());
    assert!(!quake_pak::starts(b"WAD2....") && quake_pak::files(&[0xff; 100]).is_none());
}
