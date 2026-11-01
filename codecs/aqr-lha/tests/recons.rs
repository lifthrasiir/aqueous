use aqr_common::rng::Rng;
use aqr_lha::lzh::{self, Head, Method, Stream, Token};
use aqr_lha::{huff, lha, recons};

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzh"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert_eq!(v.len(), 9);
    v
}

/// The streams of the fixtures: each with its method, its size and the CRC-16 of its file.
fn streams() -> Vec<(String, Method, Vec<u8>, usize, u16)> {
    let mut out = Vec::new();
    for (name, d) in fixtures() {
        for e in lha::parse(&d).unwrap().entries {
            if let Some(m) = Method::of(&e.method) {
                out.push((name.clone(), m, d[e.data.clone()].to_vec(), e.size as usize, e.crc));
            }
        }
    }
    out
}

#[test]
fn fixtures_split_and_joined() {
    let all = streams();
    assert_eq!(all.len(), 7);
    for (name, m, s, size, crc) in all {
        let st = lzh::decode(&s, size, m).unwrap();
        assert_eq!(lha::crc16(&st.data), crc, "{name}");
        assert!(lzh::encode(&st, m).unwrap() == s, "{name}");
        // (every compressor's headers as predicted)
        let mut at = 0;
        for h in &st.heads {
            assert_eq!(huff::head(&st.tokens[at..at + h.size as usize], m), *h, "{name}");
            at += h.size as usize;
        }
        let sp = recons::split(&s, size, m).unwrap();
        assert!(sp.data == st.data, "{name}");
        assert!(recons::join(&sp.data, &sp.record).unwrap() == s, "{name}");
        eprintln!("{name}: {} B, {} blocks, record {} B", s.len(), st.heads.len(), sp.record.len());
        assert!(sp.record.len() * 20 < s.len(), "{name}");
    }
}

/// Data of runs copied from before, so that matches abound, some far.
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        if d.len() > 16 && rng.below(3) != 0 {
            let back = 1 + rng.below(d.len().min(70000) as u64) as usize;
            let len = 3 + rng.below(40) as usize;
            for _ in 0..len {
                d.push(d[d.len() - back]);
            }
        } else {
            d.push(b'a' + rng.below(6) as u8);
        }
    }
    d.truncate(n);
    d
}

/// A random parse of `d` within the window of `m`: literals, and matches of any lengths and
/// distances that fit, not only the nearest nor the longest.
fn parse(rng: &mut Rng, d: &[u8], m: Method) -> Vec<Token> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < d.len() {
        let mut took = false;
        if rng.below(3) != 0 && pos > 0 {
            for _ in 0..8 {
                let dist = 1 + rng.below(pos.min(m.window()) as u64) as usize;
                let l = recons::lcp(d, pos - dist, pos).min(256);
                if l >= 3 {
                    let len = 3 + rng.below((l - 2) as u64) as usize;
                    out.push(Token::Match { len: len as u16, dist: dist as u32 });
                    pos += len;
                    took = true;
                    break;
                }
            }
        }
        if !took {
            out.push(Token::Literal(d[pos]));
            pos += 1;
        }
    }
    out
}

/// A stream of `tokens` in blocks of random sizes, some headers written otherwise than predicted
/// (runs of zeros as single zeros, the zeros at 3-5 not skipped), random padding, bytes after it.
fn stream(rng: &mut Rng, d: &[u8], tokens: Vec<Token>, m: Method) -> Stream {
    let mut heads = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let size = (1 + rng.below(3000) as usize).min(tokens.len() - at);
        let mut h = huff::head(&tokens[at..at + size], m);
        if rng.below(3) == 0 && h.c.n != 0 {
            let lens = lzh::c_lens(&h.c).unwrap();
            h.c.items = lens[..h.c.n as usize]
                .iter()
                .map(|&l| lzh::Item { sym: if l == 0 { 0 } else { l + 2 }, extra: 0 })
                .collect();
            let tl = huff::lengths(&huff::t_freqs(&h.c.items));
            h.t = huff::pt_table(tl, true);
            if h.t.n > 3 && h.t.skip > 0 && rng.below(2) == 0 {
                h.t.skip = 0;
            }
        }
        heads.push(h);
        at += size;
    }
    let mut st = Stream { data: d.to_vec(), heads, tokens, ..Stream::default() };
    // (the padding found by writing it: whatever bits there are to the byte's end)
    let probe = lzh::encode(&Stream { pad_bits: 0, ..st.clone() }, m);
    let bits = match probe {
        Ok(_) => 0,
        Err(_) => (1..8).find(|&b| lzh::encode(&Stream { pad_bits: b, ..st.clone() }, m).is_ok()).unwrap(),
    };
    st.pad_bits = bits;
    st.pad = rng.below(1 << bits) as u8;
    if rng.below(2) == 0 {
        st.tail = (0..rng.below(5)).map(|_| rng.below(256) as u8).collect();
    }
    st
}

#[test]
fn random_parses_split_and_joined() {
    let mut rng = Rng::new(7);
    // (headers not as predicted, blocks of more than one, streams with bytes after their padding)
    let (mut other, mut blocks, mut tails) = (0, 0, 0);
    for k in 0..24 {
        let m = Method::of_number(4 + (k % 4) as u8).unwrap();
        let n = [1, 2, 100, 5000, 40000, 150000][k % 6];
        let d = data(&mut rng, n);
        let tokens = parse(&mut rng, &d, m);
        let st = stream(&mut rng, &d, tokens, m);
        let s = lzh::encode(&st, m).unwrap();
        let back = lzh::decode(&s, d.len(), m).unwrap();
        assert!(back == st, "{k}");
        let mut at = 0;
        for h in &st.heads {
            other += (huff::head(&st.tokens[at..at + h.size as usize], m) != *h) as usize;
            at += h.size as usize;
        }
        blocks += st.heads.len().saturating_sub(1);
        tails += !st.tail.is_empty() as usize;
        let sp = recons::split(&s, d.len(), m).unwrap();
        assert!(sp.data == d);
        assert!(recons::join(&sp.data, &sp.record).unwrap() == s, "{k}");
        // damaged records fail, or give some stream: never a panic
        for i in 0..sp.record.len().min(64) {
            let mut r = sp.record.clone();
            r[i] ^= 1 << (i % 8);
            let _ = recons::join(&sp.data, &r);
        }
        let _ = recons::join(&sp.data, &sp.record[..sp.record.len() / 2]);
        let _ = recons::join(&d[..d.len() / 2], &sp.record);
    }
    assert!(other >= 5 && blocks >= 20 && tails >= 5, "{other} {blocks} {tails}");
}

#[test]
fn streams_refused() {
    let (_, m, s, size, _) = streams().into_iter().next().unwrap();
    // cut short, of another size, damaged: they fail or decode to a stream of themselves
    assert!(lzh::decode(&s[..s.len() / 2], size, m).is_err());
    assert!(recons::split(&s, size + 1, m).is_err());
    for i in (0..s.len()).step_by(97) {
        let mut x = s.clone();
        x[i] ^= 0x40;
        if let Ok(sp) = recons::split(&x, size, m) {
            assert!(recons::join(&sp.data, &sp.record).unwrap() == x);
        }
    }
    // a block of no tokens, a table past its symbols
    let empty = Head { size: 0, ..Head::default() };
    assert!(lzh::codes(&empty, m).is_err());
    assert!(lzh::decode(&[0, 0, 0, 0], 10, m).is_err());
    assert!(lzh::decode(&[0, 1, 0xff, 0xff, 0xff, 0xff], 10, m).is_err());
}
