use aqr_bundle::packs::fmod_bank::{files, starts};

fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn fsb5(version: u32, samples: &[u8], names: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = b"FSB5".to_vec();
    v.extend(version.to_le_bytes());
    v.extend(1u32.to_le_bytes());
    for n in [samples.len(), names.len(), data.len()] {
        v.extend((n as u32).to_le_bytes());
    }
    v.resize(if version == 0 { 64 } else { 60 }, 0);
    v.extend(samples);
    v.extend(names);
    v.extend(data);
    v
}

fn bank(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut body = b"FEV ".to_vec();
    for p in parts {
        body.extend(p);
    }
    let mut v = b"RIFF".to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    v
}

fn snd(pad: usize, f: &[u8]) -> Vec<u8> {
    let mut body = vec![0; pad];
    body.extend(f);
    chunk(b"SND ", &body)
}

fn list() -> Vec<u8> {
    chunk(b"LIST", &[b"PROJ".to_vec(), chunk(b"BNKI", &[0; 5])].concat())
}

#[test]
fn banks() {
    let a = fsb5(1, &[1; 8], &[2; 4], &[3; 100]);
    let b = fsb5(0, &[1; 8], &[], &[4; 33]);
    let d = bank(&[chunk(b"FMT ", &[1, 0, 0, 0]), list(), snd(6, &a), chunk(b"STDT", &[]), snd(0, &b)]);
    assert!(starts(&d));
    let f = files(&d).unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].name, b"000.fsb");
    assert_eq!(&d[f[0].extents[0].clone()], &a[..]);
    assert_eq!(&d[f[1].extents[0].clone()], &b[..]);
    assert_eq!(f[1].name, b"001.fsb");
    // an SND chunk within a LIST, and an FSB5 shorter than its chunk (the trailing bytes are the rest)
    let mut padded = a.clone();
    padded.extend([9; 5]);
    let d = bank(&[chunk(b"LIST", &[b"SNDS".to_vec(), snd(2, &padded)].concat())]);
    let f = files(&d).unwrap();
    assert_eq!(&d[f[0].extents[0].clone()], &a[..]);
    // sizes over the chunk are cut to it; no sound data
    let f = files(&bank(&[snd(0, &a[..70])])).unwrap();
    assert_eq!(f[0].extents[0].len(), 70);
    assert!(files(&bank(&[list()])).unwrap().is_empty());
}

#[test]
fn truncated_and_foreign() {
    let a = fsb5(1, &[1; 8], &[2; 4], &[3; 100]);
    let d = bank(&[list(), snd(6, &a)]);
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            for x in f {
                assert!(x.extents.iter().all(|r| r.end <= n));
            }
        }
    }
    assert!(files(b"RIFF\0\0\0\0WAVEfmt ").is_none());
    assert!(files(&[0xFF; 100]).is_none());
    // hostile sizes
    let mut h = bank(&[snd(0, &a)]);
    h[17..21].copy_from_slice(&u32::MAX.to_le_bytes());
    let _ = files(&h);
}
