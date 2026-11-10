use aqr_azo::azo::{self, History, Token};
use aqr_azo::recons;
use aqr_common::rng::Rng;

/// The AZO streams of an EGG archive's blocks of method 3 (found by their headers).
fn streams(d: &[u8]) -> Vec<Vec<u8>> {
    let sig = 0x02B5_0C13u32.to_le_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(k) = d[at..].windows(4).position(|w| w == sig) {
        let h = at + k + 4;
        at = h;
        let Some(hd) = d.get(h..h + 18) else { break };
        if hd[0] != 3 {
            continue;
        }
        let csize = u32::from_le_bytes(hd[6..10].try_into().unwrap()) as usize;
        out.push(d[h + 18..h + 18 + csize].to_vec());
        at = h + 18 + csize;
    }
    out
}

/// The AZO streams of aqr-egg's fixtures (EggDotNet's, ALZip's output).
fn fixtures() -> Vec<Vec<u8>> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../aqr-egg/tests/data");
    let v: Vec<_> = ["azoshort_txt", "azonumbers"]
        .iter()
        .flat_map(|n| streams(&std::fs::read(format!("{dir}/{n}.egg")).unwrap()))
        .collect();
    assert_eq!(v.len(), 2);
    v
}

#[test]
fn fixtures_split_and_joined() {
    for s in fixtures() {
        let sp = recons::split(&s, usize::MAX, 2).unwrap();
        assert_eq!(sp.used, s.len());
        assert!(recons::join(&sp.data, &sp.record, 2).unwrap() == s);
        eprintln!("{} B of {} B of data: record {} B", s.len(), sp.data.len(), sp.record.len());
    }
}

/// A parse of `d` drawn at random among the tokens that fit at each position: every kind, and
/// matches neither the nearest nor as long as they could be.
fn random_parse(d: &[u8], rng: &mut Rng) -> Vec<Token> {
    let lcp = |a: usize, b: usize| (0..d.len() - b).take_while(|&i| d[a + i] == d[b + i]).count();
    let (mut h, mut tokens, mut pos) = (History::default(), Vec::new(), 1);
    let cap = |pos: usize| (d.len() - pos).min(azo::MAX_LENGTH as usize);
    while pos < d.len() {
        let mut options = vec![Token::Literal];
        let fits = |dist: u32, len: u32| {
            dist >= 1
                && dist as usize <= pos
                && len >= 2
                && len as usize <= cap(pos)
                && lcp(pos - dist as usize, pos) >= len as usize
        };
        for k in 0..2 {
            let i = h.recent_index[k] as usize;
            if fits((pos as u32).wrapping_sub(h.places[i]), h.lengths[i]) {
                options.push(Token::ReuseRecent(k as u8));
            }
            let dist = h.recent_dist[k];
            if dist as usize <= pos && lcp(pos - dist as usize, pos).min(cap(pos)) >= 2 {
                let full = lcp(pos - dist as usize, pos).min(cap(pos)) as u32;
                options.push(Token::Near(k as u8, 2 + rng.below(full as u64 - 1) as u32));
            }
        }
        for i in 0..128 {
            if fits((pos as u32).wrapping_sub(h.places[i]), h.lengths[i]) {
                options.push(Token::Reuse(i as u8));
            }
        }
        // a few earlier positions that match
        for _ in 0..4 {
            let c = rng.below(pos as u64) as usize;
            let l = lcp(c, pos).min(cap(pos));
            if l >= 2 {
                let len = if rng.below(2) == 0 { l } else { 2 + rng.below(l as u64 - 1) as usize };
                options.push(Token::Far((pos - c) as u32, len as u32));
            }
        }
        let t = options[rng.below(options.len() as u64) as usize];
        pos += h.apply(pos as u32, t).map_or(1, |(_, n)| n as usize);
        tokens.push(t);
    }
    tokens
}

/// Data of repeats and noise.
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        if d.len() > 16 && rng.below(3) > 0 {
            let at = rng.below(d.len() as u64 - 8) as usize;
            let k = (2 + rng.below(40) as usize).min(d.len() - at);
            for i in 0..k {
                d.push(d[at + i]);
            }
        } else {
            d.push(b"abcdefgh\xe8\x00\xff"[rng.below(11) as usize]);
        }
    }
    d.truncate(n);
    d
}

/// A stream of `blocks` (data and a parse, or None for stored).
fn stream(x86: bool, blocks: &[(Vec<u8>, Option<Vec<Token>>)]) -> Vec<u8> {
    let mut s = vec![azo::VERSION, x86 as u8];
    let put = |s: &mut Vec<u8>, n: usize, code: &[u8]| {
        for v in [n, code.len(), n ^ code.len()] {
            s.extend_from_slice(&(v as u32).to_be_bytes());
        }
        s.extend_from_slice(code);
    };
    for (d, t) in blocks {
        match t {
            Some(t) => put(&mut s, d.len(), &azo::encode_block(d, t, &mut azo::Model::default()).unwrap()),
            None => put(&mut s, d.len(), d),
        }
    }
    put(&mut s, 0, &[]);
    s
}

/// Parses of every kind encoded and decoded again, and their streams split and joined: blocks
/// of full and short sizes, stored blocks, the x86 flag.
#[test]
fn random_parses() {
    let mut rng = Rng::new(7);
    let mut kinds = [0; 5];
    for round in 0..6 {
        let size = [300, 4000, 20000][round % 3];
        let mut blocks = Vec::new();
        for b in 0..3 {
            let d = data(&mut rng, if b == 2 { size / 3 } else { size });
            let t = random_parse(&d, &mut rng);
            for x in &t {
                kinds[match x {
                    Token::Literal => 0,
                    Token::ReuseRecent(_) => 1,
                    Token::Reuse(_) => 2,
                    Token::Near(..) => 3,
                    Token::Far(..) => 4,
                }] += 1;
            }
            let code = azo::encode_block(&d, &t, &mut azo::Model::default()).unwrap();
            let back = azo::decode_block(&code, d.len(), &mut azo::Model::default()).unwrap();
            assert!(back.data == d && back.tokens == t, "{round}/{b}");
            // (a coded block as long as its data would be read as stored)
            let stored = round == 1 && b == 1 || code.len() == d.len();
            blocks.push((d, (!stored).then_some(t)));
        }
        // (blocks of one size but the last)
        let total: usize = blocks.iter().map(|b| b.0.len()).sum();
        let s = stream(round % 2 == 1, &blocks);
        let sp = recons::split(&s, usize::MAX, 2);
        let sp = match sp {
            Ok(sp) => sp,
            Err(e) => panic!("{round}: {e}"),
        };
        assert_eq!((sp.used, sp.data.len()), (s.len(), total));
        // (with the x86 filter the data is the file itself: what the stream decodes to)
        assert!(sp.data == azo::decode(&s, usize::MAX).unwrap().data, "{round}: data");
        assert!(recons::join(&sp.data, &sp.record, 2).unwrap() == s, "{round}");
        // a record cut or damaged: fails, or gives another stream of the data
        for k in 0..8 {
            let mut r = sp.record.clone();
            let at = r.len() * k / 8;
            r[at] ^= 1 << k;
            if let Ok(j) = recons::join(&sp.data, &r, 2) {
                let back = azo::decode(&j, usize::MAX).unwrap();
                assert!(back.data.len() == total);
            }
            assert!(recons::join(&sp.data, &sp.record[..at], 2).is_err() || at == sp.record.len());
        }
    }
    assert!(kinds.iter().all(|&n| n > 100), "{kinds:?}");
}

/// Streams not taken: another version, blocks of another size than the first but for the last, a
/// check that does not hold, a block that does not decode.
#[test]
fn refusals() {
    let mut rng = Rng::new(9);
    let d = data(&mut rng, 1000);
    let t = random_parse(&d, &mut rng);
    let s = stream(false, &[(d.clone(), Some(t.clone()))]);
    assert!(recons::split(&s, usize::MAX, 2).is_ok());
    assert!(recons::split(&s, 999, 2).is_err());
    let mut v = s.clone();
    v[0] = b'2';
    assert!(recons::split(&v, usize::MAX, 2).is_err());
    let mut v = s.clone();
    v[2 + 11] ^= 1;
    assert!(recons::split(&v, usize::MAX, 2).is_err());
    let small = stream(false, &[(d[..500].to_vec(), None), (d.clone(), Some(t))]);
    assert!(recons::split(&small, usize::MAX, 2).is_err());
    let mut v = s.clone();
    v[2 + 12] ^= 0x80;
    assert!(recons::split(&v, usize::MAX, 2).is_err());
    // the empty stream
    let e = stream(false, &[]);
    let sp = recons::split(&e, usize::MAX, 2).unwrap();
    assert!(sp.data.is_empty() && recons::join(&[], &sp.record, 2).unwrap() == e);
}

/// The x86 filter (found by the CRC-32 of ALZip's archives alone, FORMAT.md §2.6): E8 and E9
/// whose address ends in 00 or FF are made relative to their own position, the address
/// extended from bit 24; and the five bytes of an E8 or E9 are skipped whether it was converted
/// or not, so an E8 in the window of one that was not is left alone.
#[test]
fn x86_filter_skips_a_window_whether_converted_or_not() {
    // converted: at 3, the address 0x00000010 + 3; at 12 (E9), 0xFFFFFF00 + 12 (bit 24 set)
    let mut d = vec![0x90, 0x90, 0x90, 0xE8, 0x13, 0, 0, 0, 0x90, 0x90, 0x90, 0x90];
    d.extend_from_slice(&[0xE9, 0x0C, 0xFF, 0xFF, 0xFF, 0x90]);
    let mut want = d.clone();
    want[4..8].copy_from_slice(&0x10u32.to_le_bytes());
    want[13..17].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    let mut got = d.clone();
    azo::unfilter(&mut got);
    assert_eq!(got, want);
    azo::filter(&mut got);
    assert_eq!(got, d);

    // not converted (its address ends in E8): the E8 four bytes on, with a convertible address,
    // is inside its window and stays as it is
    let d = [0xE8, 0x12, 0x34, 0xD8, 0xE8, 0x31, 0xB5, 0x15, 0xFF, 0x78, 0x00, 0x00];
    let mut got = d;
    azo::unfilter(&mut got);
    assert_eq!(got, d);
}
