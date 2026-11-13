use aqr_bundle::packs::{doom_wad, hl_wad, quake_wad};

fn wad(magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> Vec<u8> {
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

type Lump<'a> = (&'a str, u8, u8, &'a [u8], u32);

fn wad23(magic: &[u8; 4], lumps: &[Lump]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut dir = Vec::new();
    for (n, ty, comp, b, size) in lumps {
        dir.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
        dir.extend_from_slice(&(b.len() as u32).to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes());
        dir.extend_from_slice(&[*ty, *comp, 0, 0]);
        let mut name = [0u8; 16];
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

#[test]
fn doom() {
    for magic in [b"IWAD", b"PWAD"] {
        let lumps: &[(&str, &[u8])] = &[
            ("E1M1", b""),
            ("THINGS", b"abcdefgh"),
            ("D_E1M1", b"MUS\x1a....."),
            ("P/X", b"\x89PNG1234"),
            ("THINGS", b"zz"),
        ];
        let d = wad(magic, lumps);
        assert!(doom_wad::starts(&d));
        let f = doom_wad::files(&d).unwrap();
        let got: Vec<_> =
            f.iter().map(|f| (String::from_utf8(f.name.clone()).unwrap(), f.extents[0].clone())).collect();
        assert_eq!(
            got,
            [
                ("0001_THINGS.lmp".to_string(), 12..20),
                ("0002_D_E1M1.mus".into(), 20..29),
                ("0003_P_X.png".into(), 29..37),
                ("0004_THINGS.lmp".into(), 37..39)
            ]
        );
        for n in 0..d.len() {
            if let Some(f) = doom_wad::files(&d[..n]) {
                assert!(f.iter().all(|f| f.extents.iter().all(|e| e.end <= n)));
            }
        }
    }
    assert!(doom_wad::files(b"IWAD").is_none() && doom_wad::files(&[7; 64]).is_none());
}

#[test]
fn wad2_and_wad3() {
    let l: &[Lump] = &[
        ("pal", 0x40, 0, b"0123456789ab", 12),
        ("+0water", 0x44, 0, b"MIPS", 4),
        ("z", 0x43, 2, b"LZ", 100),
        ("e", 0x42, 0, b"", 0),
    ];
    let d = wad23(b"WAD2", l);
    assert!(quake_wad::starts(&d) && !hl_wad::starts(&d));
    let f = quake_wad::files(&d).unwrap();
    let names: Vec<_> = f.iter().map(|f| String::from_utf8(f.name.clone()).unwrap()).collect();
    assert_eq!(names, ["0000_pal.pal", "0001_+0water.mip", "0002_z.lmp"]);
    assert_eq!(f[1].extents[0], 24..28);
    assert!(f[0].compressed.is_none());
    assert!(matches!(f[2].compressed, Some(aqr_disk::Compressed::Opaque { len: 100 })));
    let d3 = wad23(b"WAD3", l);
    assert!(hl_wad::starts(&d3) && !quake_wad::starts(&d3) && hl_wad::files(&d3).unwrap().len() == 3);
    assert_eq!(hl_wad::files(&d3).unwrap()[1].name, b"0001_+0water.lmp");
    for d in [&d, &d3] {
        for n in 0..d.len() {
            for f in [quake_wad::files(&d[..n]), hl_wad::files(&d[..n])].into_iter().flatten() {
                assert!(f.iter().all(|f| f.extents.iter().all(|e| e.end <= n)));
            }
        }
    }
    assert!(quake_wad::files(b"WAD2").is_none() && hl_wad::files(&[1; 40]).is_none());
}
