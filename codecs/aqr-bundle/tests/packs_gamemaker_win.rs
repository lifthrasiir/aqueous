use aqr_bundle::packs::gamemaker_win;

fn png(extra: usize) -> Vec<u8> {
    let mut o = vec![0x89, b'P', b'N', b'G', 13, 10, 0x1a, 10];
    for (t, n) in [(b"IHDR", 13usize), (b"IDAT", 5 + extra), (b"IEND", 0)] {
        o.extend_from_slice(&(n as u32).to_be_bytes());
        o.extend_from_slice(t);
        o.extend(std::iter::repeat_n(7u8, n));
        o.extend_from_slice(&[0; 4]);
    }
    o
}

fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut o = tag.to_vec();
    o.extend_from_slice(&(body.len() as u32).to_le_bytes());
    o.extend_from_slice(body);
    o
}

/// A game: GEN8, TXTR (entries of 28 bytes where `wide`, else of 8: a flag and the pointer)
/// and AUDO.
fn game(wide: bool, blobs: &[Vec<u8>], audio: &[&[u8]]) -> Vec<u8> {
    let gen8 = chunk(b"GEN8", &[0; 16]);
    let esize = if wide { 28 } else { 8 };
    let txtr_at = 8 + gen8.len();
    let mut txtr = (blobs.len() as u32).to_le_bytes().to_vec();
    let list = 4 + 4 * blobs.len();
    let mut at = txtr_at + 8 + list + esize * blobs.len();
    let mut entries = Vec::new();
    let mut data = Vec::new();
    for (i, b) in blobs.iter().enumerate() {
        txtr.extend_from_slice(&((txtr_at + 8 + list + esize * i) as u32).to_le_bytes());
        let e: Vec<u32> = if wide { vec![1, 0, b.len() as u32, 64, 64, 0, at as u32] } else { vec![1, at as u32] };
        for w in e {
            entries.extend_from_slice(&w.to_le_bytes());
        }
        data.extend_from_slice(b);
        data.extend_from_slice(&[0; 3]);
        at += b.len() + 3;
    }
    txtr.extend(entries);
    txtr.extend(data);
    let txtr = chunk(b"TXTR", &txtr);
    let audo_at = txtr_at + txtr.len() + 8;
    let mut audo = (audio.len() as u32).to_le_bytes().to_vec();
    let mut p = audo_at + 4 + 4 * audio.len();
    let mut body = Vec::new();
    for a in audio {
        audo.extend_from_slice(&(p as u32).to_le_bytes());
        body.extend_from_slice(&(a.len() as u32).to_le_bytes());
        body.extend_from_slice(a);
        p += 4 + a.len();
    }
    audo.extend(body);
    let mut inner = [gen8, txtr, chunk(b"AUDO", &audo)].concat();
    let mut o = b"FORM".to_vec();
    o.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    o.append(&mut inner);
    o
}

fn names(f: &[aqr_disk::File]) -> Vec<String> {
    f.iter().map(|f| String::from_utf8(f.name.clone()).unwrap()).collect()
}

#[test]
fn reads() {
    let audio: [&[u8]; 3] = [b"RIFF0000WAVE", b"OggSxxxx", b"????"];
    for wide in [false, true] {
        let blobs = vec![png(0), png(40)];
        let d = game(wide, &blobs, &audio);
        assert!(gamemaker_win::starts(&d));
        let f = gamemaker_win::files(&d).unwrap();
        assert_eq!(names(&f), ["txtr/000.png", "txtr/001.png", "audo/000.wav", "audo/001.ogg", "audo/002.bin"]);
        assert_eq!(&d[f[1].extents[0].clone()], &blobs[1][..]);
        assert_eq!(&d[f[3].extents[0].clone()], b"OggSxxxx");
        for n in 0..d.len() {
            if let Some(f) = gamemaker_win::files(&d[..n]) {
                assert!(f.iter().all(|f| f.extents.iter().all(|e| e.end <= n)));
            }
        }
    }
    // QOI variants: the size the entry tells
    let q = |m: &[u8; 4], n: usize| [m.as_slice(), &[64, 0, 64, 0], &vec![9u8; n]].concat();
    let d = game(true, &[q(b"2zoq", 30), q(b"fioq", 20)], &[]);
    let f = gamemaker_win::files(&d).unwrap();
    assert_eq!(names(&f), ["txtr/000.zqoi", "txtr/001.qoi"]);
    assert_eq!(f[0].extents[0].len(), 38);
    assert_eq!(f[1].extents[0].len(), 28);
    assert!(gamemaker_win::files(b"FORM\0\0\0\0").is_none() && gamemaker_win::files(&[0; 40]).is_none());
}

#[test]
fn audiogroup() {
    let mut audo = 2u32.to_le_bytes().to_vec();
    audo.extend_from_slice(&28u32.to_le_bytes());
    audo.extend_from_slice(&40u32.to_le_bytes());
    for a in [b"RIFFabcd", b"OggS...."] {
        audo.extend_from_slice(&8u32.to_le_bytes());
        audo.extend_from_slice(a);
    }
    let mut inner = chunk(b"AUDO", &audo);
    let mut d = b"FORM".to_vec();
    d.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    d.append(&mut inner);
    let f = gamemaker_win::files(&d).unwrap();
    assert_eq!(names(&f), ["audo/000.wav", "audo/001.ogg"]);
    assert_eq!(&d[f[0].extents[0].clone()], b"RIFFabcd");
}
