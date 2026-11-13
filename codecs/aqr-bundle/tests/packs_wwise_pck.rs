use aqr_bundle::packs::wwise_pck::{files, starts};

fn u(n: usize) -> [u8; 4] {
    (n as u32).to_le_bytes()
}

struct Entry {
    id: u64,
    block: usize,
    data: Vec<u8>,
    lang: usize,
}

/// A table of `entries`: whether the ids are 64-bit, and the files placed from `*end` on, each
/// at a multiple of its block size.
fn table(entries: &[Entry], wide: bool, body: &mut Vec<u8>, base: usize) -> Vec<u8> {
    let mut t = u(entries.len()).to_vec();
    for e in entries {
        body.resize((base + body.len()).next_multiple_of(e.block) - base, 0);
        let offset = (base + body.len()) / e.block;
        if wide {
            t.extend(e.id.to_le_bytes());
        } else {
            t.extend(u(e.id as usize));
        }
        for n in [e.block, e.data.len(), offset, e.lang] {
            t.extend(u(n));
        }
        body.extend(&e.data);
    }
    t
}

fn langs() -> Vec<u8> {
    let mut m = u(2).to_vec();
    let names: Vec<Vec<u8>> = ["sfx", "english(us)"]
        .iter()
        .map(|s| s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect())
        .collect();
    let mut at = 4 + 16;
    for (i, n) in names.iter().enumerate() {
        m.extend(u(at));
        m.extend(u(i));
        at += n.len();
    }
    for n in names {
        m.extend(n);
    }
    m
}

/// A package with the three tables (and a fourth where `externals` is given).
fn package(banks: &[Entry], sounds: &[Entry], externals: Option<&[Entry]>) -> Vec<u8> {
    // (the tables' sizes are known before the files are placed: the files go after them)
    let langs = langs();
    let wide_ext = externals.is_some();
    let size = |n: usize, w: usize| 4 + n * w;
    let tables = size(banks.len(), 20) + size(sounds.len(), 20) + externals.map_or(0, |e| size(e.len(), 24));
    let header = 8 + 4 * if wide_ext { 5 } else { 4 } + langs.len() + tables;
    let mut body = Vec::new();
    let b = table(banks, false, &mut body, header);
    let s = table(sounds, false, &mut body, header);
    let x = externals.map(|e| table(e, true, &mut body, header)).unwrap_or_default();
    let mut v = b"AKPK".to_vec();
    v.extend(u(header - 8));
    v.extend(u(1));
    v.extend(u(langs.len()));
    v.extend(u(b.len()));
    v.extend(u(s.len()));
    if wide_ext {
        v.extend(u(x.len()));
    }
    for t in [langs, b, s, x] {
        v.extend(t);
    }
    assert_eq!(v.len(), header);
    v.extend(body);
    v
}

fn e(id: u64, block: usize, n: usize, lang: usize) -> Entry {
    Entry { id, block, data: vec![id as u8; n], lang }
}

#[test]
fn packages() {
    let banks = [e(1, 1, 30, 0), e(2, 16, 50, 0)];
    // (the first a medium of Wwise's Vorbis, by its `fmt ` chunk: `.wem`; the others of no RIFF:
    // `.bin`, as a bank's media)
    let mut vorbis = e(100, 2048, 70, 0);
    vorbis.data[..22].copy_from_slice(b"RIFF\0\0\0\0WAVEfmt \x18\0\0\0\xff\xff");
    let sounds = [vorbis, e(100, 2048, 90, 1), e(101, 1, 3, 9)];
    let ext = [e(1 << 40, 4, 12, 0)];
    for externals in [None, Some(&ext[..])] {
        let d = package(&banks, &sounds, externals);
        assert!(starts(&d));
        let f = files(&d).unwrap();
        assert_eq!(f.len(), 5 + externals.is_some() as usize);
        let names: Vec<_> = f.iter().map(|x| String::from_utf8(x.name.clone()).unwrap()).collect();
        assert_eq!(names[..5], ["1.bnk", "2.bnk", "100.wem", "english(us)/100.bin", "lang9/101.bin"]);
        if externals.is_some() {
            assert_eq!(names[5], format!("{}.bin", 1u64 << 40));
        }
        for (x, en) in f.iter().zip(banks.iter().chain(&sounds).chain(externals.unwrap_or(&[]))) {
            assert_eq!(&d[x.extents[0].clone()], &en.data[..]);
        }
    }
    assert!(files(&package(&[], &[], None)).unwrap().is_empty());
}

#[test]
fn truncated_and_foreign() {
    let banks = [e(1, 1, 30, 0), e(2, 16, 50, 0)];
    let sounds = [e(100, 2048, 70, 0), e(101, 1, 3, 1)];
    let ext = [e(7, 4, 12, 0)];
    for d in [package(&banks, &sounds, None), package(&banks, &sounds, Some(&ext))] {
        for n in 0..d.len() {
            if let Some(f) = files(&d[..n]) {
                for x in f {
                    assert!(x.extents.iter().all(|r| r.end <= n));
                }
            }
        }
        for at in (4..120).step_by(4) {
            let mut h = d.clone();
            h[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let _ = files(&h);
        }
    }
    assert!(files(b"AKPK").is_none());
    assert!(files(&[0; 200]).is_none());
}
