use aqr_bundle::packs::xact_xwb::{files, starts};

struct Wave {
    format: u32,
    data: Vec<u8>,
}

/// A bank of version 46: the header (52 bytes), bank data (96), entry metadata (24 each, or 4
/// of a compact bank of `compact`'s format) and the waves from `align`-aligned offsets.
fn bank(waves: &[Wave], compact: Option<u32>, align: u32) -> Vec<u8> {
    let (mut meta, mut data) = (Vec::new(), Vec::new());
    for w in waves {
        data.resize(data.len().next_multiple_of(align as usize), 0);
        if compact.is_some() {
            // (the deviation: the padding up to the next wave's offset)
            let dev = (align - w.data.len() as u32 % align) % align;
            meta.extend(((data.len() as u32 / align) | (dev << 21)).to_le_bytes());
        } else {
            for n in [3, w.format, data.len() as u32, w.data.len() as u32, 0, 0] {
                meta.extend(n.to_le_bytes());
            }
        }
        data.extend(&w.data);
    }
    if compact.is_some() {
        data.resize(data.len().next_multiple_of(align as usize), 0);
    }
    let mut bd = Vec::new();
    for n in [if compact.is_some() { 0x20000 } else { 0x80000 }, waves.len() as u32] {
        bd.extend(n.to_le_bytes());
    }
    bd.extend(b"Test");
    bd.resize(72, 0);
    for n in [if compact.is_some() { 4 } else { 24 }, 64, align, compact.unwrap_or(0)] {
        bd.extend(n.to_le_bytes());
    }
    bd.resize(96, 0);
    let mut v = b"WBND".to_vec();
    for n in [46u32, 44, 52, 96, 148, meta.len() as u32] {
        v.extend(n.to_le_bytes());
    }
    let wd = (148 + meta.len()) as u32;
    for n in [wd, 0, 0, 0, wd, data.len() as u32] {
        v.extend(n.to_le_bytes());
    }
    v.extend(bd);
    v.extend(meta);
    v.extend(data);
    v
}

#[test]
fn banks() {
    let waves = vec![
        Wave { format: 0, data: vec![1; 100] },
        Wave { format: 2, data: vec![2; 37] },
        Wave { format: 3, data: vec![3; 64] },
        Wave { format: 1, data: vec![4; 5] },
    ];
    let d = bank(&waves, None, 4);
    assert!(starts(&d));
    let f = files(&d).unwrap();
    assert_eq!(f.len(), 4);
    for (i, (w, x)) in waves.iter().zip(&f).enumerate() {
        let ext = ["pcm", "xma", "adpcm", "xwma"][w.format as usize];
        assert_eq!(x.name, format!("{i:03}.{ext}").into_bytes());
        assert_eq!(&d[x.extents[0].clone()], &w.data[..]);
    }
    // compact: every wave of the bank's format; lengths to the next offset less the deviation
    let d = bank(&waves, Some(2), 16);
    let f = files(&d).unwrap();
    assert_eq!(f.len(), 4);
    for (w, x) in waves.iter().zip(&f) {
        assert!(x.name.ends_with(b".adpcm"));
        assert_eq!(&d[x.extents[0].clone()], &w.data[..]);
    }
    assert!(files(&bank(&[], None, 4)).unwrap().is_empty());
}

#[test]
fn truncated_and_foreign() {
    let waves = vec![Wave { format: 0, data: vec![1; 100] }, Wave { format: 2, data: vec![2; 37] }];
    for d in [bank(&waves, None, 4), bank(&waves, Some(0), 16)] {
        for n in 0..d.len() {
            if let Some(f) = files(&d[..n]) {
                for x in f {
                    assert!(x.extents.iter().all(|r| r.end <= n));
                }
            }
        }
        // hostile fields: every u32 of the headers set to the maximum in turn
        for at in (4..148).step_by(4) {
            let mut h = d.clone();
            h[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let _ = files(&h);
        }
    }
    assert!(files(b"WBND").is_none());
    assert!(files(&[0; 500]).is_none());
}

/// A bank of ADPCM waves (stereo blocks of 140 bytes, mono of 70) and a PCM one: the ADPCM
/// waves' whole blocks rebuilt by `msadpcm-recons`, a partial block and the PCM wave kept; the
/// bank given back. A bank of no ADPCM has no such formula.
#[test]
fn adpcm_waves() {
    use aqr_bundle::packs::xact_xwb::{adpcm_layout, encode_with};
    let mut rng = aqr_common::rng::Rng::new(4);
    let mut noise = |n: usize| -> Vec<u8> { (0..n).map(|_| rng.below(256) as u8).collect() };
    // (the format: ADPCM, channels, the block alignment's field: 140 = (48 + 22) x 2)
    let adpcm = |ch: u32| 2 | ch << 2 | 44100 << 5 | 48 << 23;
    let waves = [
        Wave { format: adpcm(2), data: noise(140 * 30) },
        Wave { format: 1 << 2 | 44100 << 5, data: noise(1000) },
        Wave { format: adpcm(1), data: noise(70 * 12 + 9) },
    ];
    assert_eq!(adpcm_layout(waves[0].format), aqr_msadpcm::codec::Layout::new(2, 140));
    let d = bank(&waves, None, 4);
    let f = encode_with(&d, |_| aqr_formula::Kind::Unknown).unwrap();
    assert_eq!(f.applies().len(), 2);
    let out = f.eval(&|a: &aqr_formula::Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, aqr_msadpcm::formula::MODULE);
        aqr_msadpcm::formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    });
    assert!(out.unwrap() == d);
    assert!(f.streams[1].class == "xact-xwb/rest");
    let pcm = [Wave { format: 1 << 2 | 44100 << 5, data: noise(1000) }];
    assert!(encode_with(&bank(&pcm, None, 4), |_| aqr_formula::Kind::Unknown).is_none());
}
