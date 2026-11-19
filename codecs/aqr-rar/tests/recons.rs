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
    for name in ["r4_m3", "r4_solid", "r4_ppmd", "r4_audio", "r4_rgb", "r4_audio_forced", "r4_ppmd_e8", "r4_e8"] {
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
    for k in [lz3::DELTA, lz3::E8, lz3::AUDIO, lz3::RGB] {
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

/// PPMd blocks of what no archiver in the fixtures writes: a filter record among the symbols
/// (the escape 3: the record of an LZ stream's fixture put in), and matches (the escapes 4
/// and 5), besides the escaped escape (1): decoded to the data, and rebuilt from the split.
#[test]
fn rar3_ppmd_escapes() {
    // A filter record of the fixtures, to be declared at the start of the data.
    let d = fixture("r4_e8");
    let (parts, sizes, dict) = groups3(&d).remove(0);
    let mut dec = lz3::Decoder3::new(dict);
    let st = dec.file(parts[0], sizes[0]).unwrap();
    let rec = st.filters[0].clone();
    assert!(rec.raw[0] & 0x80 == 0 || rec.raw.len() > 40, "a record that makes its program");
    // (declared first in the PPMd block: its start is in the data from the record's own offset)
    let esc = 2u8;
    let mut data: Vec<u8> = Vec::new();
    let mut syms: Vec<u8> = vec![esc, 3];
    syms.extend_from_slice(&rec.raw);
    let mut x = 12345u32;
    let mut lit = |data: &mut Vec<u8>, syms: &mut Vec<u8>, n: usize| {
        for _ in 0..n {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let b = b"abcde fghij klmno 0123"[(x >> 16) as usize % 22];
            data.push(b);
            syms.push(b);
        }
    };
    lit(&mut data, &mut syms, rec.at + rec.len as usize + 300);
    // an escape byte as data
    data.push(esc);
    syms.extend([esc, 1]);
    lit(&mut data, &mut syms, 50);
    // the escape 5: a match at distance 1, of 4 + the symbol bytes
    let last = *data.last().unwrap();
    data.extend(std::iter::repeat_n(last, 9));
    syms.extend([esc, 5, 5]);
    lit(&mut data, &mut syms, 40);
    // the escape 4: a match at distance 2 + the three bytes, of 32 + the symbol bytes
    let (dist, len) = (130usize, 35usize);
    for _ in 0..len {
        data.push(data[data.len() - dist]);
    }
    let v = dist - 2;
    syms.extend([esc, 4, (v >> 16) as u8, (v >> 8) as u8, v as u8, (len - 32) as u8]);
    lit(&mut data, &mut syms, 30);
    syms.extend([esc, 2]); // the end of the file
    // the filter changes the data as it is put out: only the unfiltered stream is compared
    let flags = 0x20 | 0x40 | 4;
    let st = lz3::Stream {
        blocks: vec![lz3::Block::Ppmd { flags, mem: 0, esc, syms: syms.clone() }],
        next_header: true,
        ..Default::default()
    };
    let mut packed = Vec::new();
    lz3::Encoder3::with_model(1).unwrap().file(&st, true, &mut packed).unwrap();
    let mut dec = lz3::Decoder3::new(1 << 20);
    let got = dec.file(&packed, data.len()).unwrap();
    assert!(dec.out == data, "the unfiltered data");
    assert_eq!(got.filters.len(), 1);
    assert_eq!((got.filters[0].kind, got.filters[0].len, got.filters[0].raw.clone()), (rec.kind, rec.len, rec.raw));
    assert_eq!(got.blocks, st.blocks);
    assert!(got.next_header);
    // the stream split and joined again
    let sp = recons3::split(&[&packed[..]], &[data.len()], 1 << 20).unwrap();
    assert!(recons3::join(&sp.data, &sp.record).unwrap() == packed);
    damaged(&sp.data, &sp.record, recons3::join);
}

/// Code lengths past the bound (Fibonacci counts make a chain of depth 39) are brought down to
/// it: a prefix code (not oversubscribed), the least frequent symbols the longest.
#[test]
fn huffman_lengths_are_limited() {
    let mut freq = vec![0u32; 40];
    let (mut a, mut b) = (1u32, 1u32);
    for f in freq.iter_mut().take(38) {
        *f = a;
        (a, b) = (b, a.saturating_add(b));
    }
    freq[39] = 1; // (a symbol after unused ones)
    for max in [15, 12, 9] {
        let lens = aqr_rar::huff::lengths(&freq, max);
        for (s, &l) in lens.iter().enumerate() {
            assert_eq!(l == 0, freq[s] == 0, "max {max}: symbol {s}");
            assert!(l as usize <= max, "max {max}: symbol {s} has {l}");
        }
        let kraft: u64 = lens.iter().filter(|&&l| l > 0).map(|&l| 1u64 << (max - l as usize)).sum();
        assert!(kraft <= 1 << max && kraft > 1 << (max - 1), "max {max}: Kraft sum {kraft}");
        // (the more frequent, the shorter or as long)
        for s in 1..38 {
            assert!(lens[s - 1] >= lens[s], "max {max}: symbols {} and {s}", s - 1);
        }
        assert!(aqr_rar::huff::codes(&lens).is_ok());
    }
}
