use aqr_rar::{lz3, lz5, rar4, rar5, recons3, recons5};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.rar", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A RAR5 stream: its files' parts, their sizes, version 1's or not, the dictionary.
type Group5<'a> = (Vec<&'a [u8]>, Vec<usize>, bool, u64);
type Group3<'a> = (Vec<&'a [u8]>, Vec<usize>, u64);

/// A RAR5 archive's streams, a solid group each.
fn groups5(d: &[u8]) -> Vec<Group5<'_>> {
    let a = rar5::parse(d).unwrap();
    let mut out: Vec<Group5> = Vec::new();
    for e in a.entries.iter().filter(|e| !e.service && e.method != 0 && !e.data.is_empty()) {
        let p = (&d[e.data.clone()], e.size.unwrap() as usize);
        match out.last_mut() {
            Some(g) if e.solid => {
                g.0.push(p.0);
                g.1.push(p.1);
            }
            _ => out.push((vec![p.0], vec![p.1], e.version == 1, e.dict)),
        }
    }
    out
}

fn groups3(d: &[u8]) -> Vec<Group3<'_>> {
    let a = rar4::parse(d).unwrap();
    let mut out: Vec<Group3> = Vec::new();
    for e in a.entries.iter().filter(|e| !e.service && e.method != 0x30 && !e.data.is_empty()) {
        let p = (&d[e.data.clone()], e.size as usize);
        match out.last_mut() {
            Some(g) if e.solid => {
                g.0.push(p.0);
                g.1.push(p.1);
            }
            _ => out.push((vec![p.0], vec![p.1], e.dict)),
        }
    }
    out
}

/// A record damaged: an error or some stream, never a panic.
fn damaged(data: &[u8], record: &[u8], join: fn(&[u8], &[u8]) -> aqr_rar::Result<Vec<u8>>) {
    let mut seed = 1u32;
    for _ in 0..40 {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let mut r = record.to_vec();
        let i = 1 + seed as usize % (r.len() - 1);
        r[i] ^= 1 << ((seed >> 16) % 8);
        let _ = join(data, &r);
        let _ = join(data, &r[..i]);
    }
}

#[test]
fn rar5_split_joined() {
    let mut filters = 0;
    for name in ["r5_m3", "r5_solid"] {
        let d = fixture(name);
        for (parts, sizes, v1, dict) in groups5(&d) {
            let sp = recons5::split(&parts, &sizes, v1, dict).unwrap();
            assert_eq!(sp.data.len(), sizes.iter().sum::<usize>());
            assert!(recons5::join(&sp.data, &sp.record).unwrap() == parts.concat(), "{name}");
            damaged(&sp.data, &sp.record, recons5::join);
            // (the filters the fixtures were made with)
            let mut dec = lz5::Decoder5::new(v1, dict);
            for (p, s) in parts.iter().zip(&sizes) {
                filters +=
                    dec.file(p, *s).unwrap().tokens.iter().filter(|t| matches!(t, lz5::Token::Filter(_))).count();
            }
        }
    }
    assert!(filters > 0, "no filters in the fixtures");
}

#[test]
fn rar3_split_joined() {
    let (mut filters, mut ppmd) = (Vec::new(), 0);
    for name in ["r4_m3", "r4_solid", "r4_ppmd", "r4_audio"] {
        let d = fixture(name);
        for (parts, sizes, dict) in groups3(&d) {
            let sp = recons3::split(&parts, &sizes, dict).unwrap();
            assert!(recons3::join(&sp.data, &sp.record).unwrap() == parts.concat(), "{name}");
            damaged(&sp.data, &sp.record, recons3::join);
            let mut dec = lz3::Decoder3::new(dict);
            for (p, s) in parts.iter().zip(&sizes) {
                let st = dec.file(p, *s).unwrap();
                filters.extend(st.filters.iter().map(|f| f.kind));
                ppmd += st.blocks.iter().filter(|b| matches!(b, lz3::Block::Ppmd { .. })).count();
            }
        }
    }
    for k in [lz3::DELTA, lz3::E8, lz3::AUDIO] {
        assert!(filters.contains(&k), "no filter {k} in the fixtures: {filters:?}");
    }
    assert!(ppmd > 0, "no PPMd blocks in the fixtures");
}

/// Streams refused: not RAR's, cut short.
#[test]
fn refused() {
    let d = fixture("r4_m3");
    for (parts, sizes, dict) in groups3(&d) {
        let p = parts[0];
        for n in [0, 1, p.len() / 2, p.len() - 1] {
            assert!(recons3::split(&[&p[..n]], &sizes, dict).is_err());
        }
    }
    let d = fixture("r5_m3");
    for (parts, sizes, v1, dict) in groups5(&d) {
        let p = parts[0];
        for n in [0, 1, p.len() / 2, p.len() - 1] {
            assert!(recons5::split(&[&p[..n]], &sizes, v1, dict).is_err());
        }
    }
}
