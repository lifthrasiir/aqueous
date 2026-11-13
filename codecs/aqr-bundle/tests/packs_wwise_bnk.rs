use aqr_bundle::packs::wwise_bnk::{files, starts};

fn u(n: u32, be: bool) -> [u8; 4] {
    if be { n.to_be_bytes() } else { n.to_le_bytes() }
}

fn chunk(tag: &[u8; 4], body: &[u8], be: bool) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend(u(body.len() as u32, be));
    v.extend(body);
    v
}

/// A bank of the media (id, bytes), each aligned to 16 bytes in DATA.
fn bank(version: u32, media: &[(u32, Vec<u8>)], be: bool) -> Vec<u8> {
    let mut hdr = Vec::new();
    hdr.extend(u(version, be));
    hdr.extend(u(77, be));
    hdr.extend([0; 16]);
    let mut v = chunk(b"BKHD", &hdr, be);
    v.extend(chunk(b"INIT", &[1, 2, 3], be));
    if !media.is_empty() {
        let (mut didx, mut data) = (Vec::new(), Vec::new());
        for (id, m) in media {
            data.resize(data.len().next_multiple_of(16), 0);
            didx.extend(u(*id, be));
            didx.extend(u(data.len() as u32, be));
            didx.extend(u(m.len() as u32, be));
            data.extend(m);
        }
        v.extend(chunk(b"DIDX", &didx, be));
        v.extend(chunk(b"DATA", &data, be));
    }
    v.extend(chunk(b"HIRC", &[0; 10], be));
    v
}

/// A medium of `n` bytes: RIFF WAVE, its `fmt ` chunk of format `codec` (Wwise's Vorbis
/// `FFFF`).
fn wem(n: usize, codec: u16) -> Vec<u8> {
    let mut v = b"RIFF\0\0\0\0WAVEfmt \x18\0\0\0".to_vec();
    v.extend(codec.to_le_bytes());
    v.resize(n, 7);
    v
}

#[test]
fn banks() {
    // (Vorbis `.wem`, PCM `.wav`, a plugin's medium `.bin`)
    let media = vec![(5, wem(40, 0xffff)), (77551, wem(26, 1)), (9, vec![0, 4, 2, 0, 0, 8])];
    for (version, be) in [(88, false), (145, false), (140, true)] {
        let d = bank(version, &media, be);
        assert!(starts(&d));
        let f = files(&d).unwrap();
        assert_eq!(f.len(), 3);
        for ((id, m), (x, ext)) in media.iter().zip(f.iter().zip(["wem", "wav", "bin"])) {
            assert_eq!(x.name, format!("{id}.{ext}").into_bytes());
            assert_eq!(&d[x.extents[0].clone()], &m[..]);
        }
    }
    assert!(files(&bank(145, &[], false)).unwrap().is_empty());
}

#[test]
fn truncated_and_foreign() {
    let d = bank(145, &[(5, wem(40, 0xffff)), (6, wem(100, 2))], false);
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            for x in f {
                assert!(x.extents.iter().all(|r| r.end <= n));
            }
        }
    }
    assert!(files(b"RIFF\0\0\0\0WAVE").is_none());
    assert!(files(b"BKHD").is_none());
    let mut h = d.clone();
    h[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let _ = files(&h);
    let mut h = d.clone();
    let at = h.windows(4).position(|w| w == b"DIDX").unwrap();
    h[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let _ = files(&h);
}
