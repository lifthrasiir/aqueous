//! Malformed DEFLATE through every entry point that reads a stream: all fail, alike, and no
//! entry point panics or reads a distance before the data (`inflate::copy_match`'s guard).

use aqr_flate::bits::BitWriter;
use std::time::Duration;

/// A Huffman code written most significant bit first, as DEFLATE does.
fn code(w: &mut BitWriter, c: u32, len: u32) {
    for i in (0..len).rev() {
        w.bits(1, (c >> i) & 1);
    }
}

/// The bytes of `w`, the last one padded.
fn finish(mut w: BitWriter) -> Vec<u8> {
    w.bits(w.pad_len(), 0);
    w.finish()
}

/// A symbol of the fixed literal/length code.
fn fixed(w: &mut BitWriter, s: u32) {
    match s {
        0..=143 => code(w, 0x30 + s, 8),
        144..=255 => code(w, 0x190 + s - 144, 9),
        256..=279 => code(w, s - 256, 7),
        _ => code(w, 0xc0 + s - 280, 8),
    }
}

fn fixed_block(last: bool, f: impl FnOnce(&mut BitWriter)) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.bits(1, last as u32);
    w.bits(2, 1);
    f(&mut w);
    finish(w)
}

const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// A dynamic block with `hlit`/`hdist` counts and the lengths `lens` (of both codes, as many
/// as the counts say) written by a code length code of 4 bits for each of 0..=15; `body`
/// writes what follows the header.
fn dynamic(hlit: usize, hdist: usize, lens: &[u8], body: impl FnOnce(&mut BitWriter)) -> Vec<u8> {
    assert_eq!(lens.len(), hlit + hdist);
    let mut w = BitWriter::new();
    w.bits(1, 1);
    w.bits(2, 2);
    w.bits(5, (hlit - 257) as u32);
    w.bits(5, (hdist - 1) as u32);
    w.bits(4, 15);
    for &s in &CL_ORDER {
        w.bits(3, if s < 16 { 4 } else { 0 });
    }
    for &l in lens {
        code(&mut w, l as u32, 4);
    }
    body(&mut w);
    finish(w)
}

/// Lengths for 257 literal/length symbols and 1 distance symbol: `'a'` and the end of block of
/// one bit, the distance symbol 0 of one bit.
fn simple_lens() -> Vec<u8> {
    let mut l = vec![0u8; 258];
    l[97] = 1;
    l[256] = 1;
    l[257] = 1;
    l
}

/// Every way of reading a stream, each giving `expect` (None: an error).
fn check(name: &str, raw: &[u8], expect: Option<&[u8]>) {
    let raw = raw.to_vec();
    let name = name.to_string();
    let expect = expect.map(<[u8]>::to_vec);
    aqr_testkit::within(Duration::from_secs(20), move || {
        let n = expect.as_ref().map_or(10, |e| e.len());
        let mut results: Vec<(&str, bool)> = Vec::new();
        let ok = |r: Option<Vec<u8>>| match (&expect, r) {
            (Some(e), Some(d)) => *e == d,
            (None, None) => true,
            _ => false,
        };
        results.push((
            "inflate::parse",
            ok(aqr_flate::inflate::parse(&raw)
                .ok()
                .map(|f| f.data.to_vec())),
        ));
        results.push((
            "inflate::parse_segments",
            ok(aqr_flate::inflate::parse_segments(&raw, 5)
                .ok()
                .map(|f| f.data.to_vec())),
        ));
        results.push((
            "inflate::parse_segments_head",
            ok(
                aqr_flate::inflate::parse_segments_head(&raw, 5, 3, usize::MAX, 0, false)
                    .ok()
                    .map(|f| f.data.to_vec()),
            ),
        ));
        results.push(("inflate::data", ok(aqr_flate::inflate::data(&raw, n).ok())));
        let streamed = {
            let mut at = 0;
            let mut out = Vec::new();
            let r = aqr_flate::inflate::data_streamed(
                raw.len() as u64,
                &mut |v, max| {
                    let k = max.min(raw.len() - at);
                    if k == 0 {
                        return Err(aqr_flate::Error::Truncated);
                    }
                    v.extend_from_slice(&raw[at..at + k]);
                    at += k;
                    Ok(())
                },
                &mut |d| {
                    out.extend_from_slice(d);
                    Ok(())
                },
                n as u64,
            );
            r.ok().map(|()| out)
        };
        results.push(("inflate::data_streamed", ok(streamed)));
        results.push((
            "deflate::parse",
            ok(aqr_flate::deflate::parse(&raw).ok().map(|(_, d)| d)),
        ));
        let d = aqr_flate::deconstruct(&raw);
        if let Ok((data, meta)) = &d {
            assert_eq!(
                aqr_flate::reconstruct(data, meta).as_deref().ok(),
                Some(&raw[..]),
                "{name}"
            );
        }
        results.push(("deconstruct", ok(d.ok().map(|(d, _)| d))));
        let bad: Vec<_> = results.iter().filter(|r| !r.1).map(|r| r.0).collect();
        assert!(
            bad.is_empty(),
            "{name}: not as expected ({expect:?}) in {bad:?}"
        );
    })
    .unwrap();
}

#[test]
fn valid_streams_are_read_by_all() {
    let raw = fixed_block(true, |w| {
        fixed(w, b'a' as u32);
        fixed(w, 257); // length 3
        code(w, 0, 5); // distance 1
        fixed(w, 256);
    });
    check("fixed", &raw, Some(b"aaaa"));
    let raw = dynamic(257, 1, &simple_lens(), |w| {
        code(w, 0, 1); // 'a'
        code(w, 1, 1); // the end of block
    });
    check("dynamic", &raw, Some(b"a"));
}

#[test]
fn malformed_streams_fail_alike() {
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    cases.push(("btype 3", vec![0x07, 0, 0, 0]));
    cases.push((
        "stored with a bad NLEN",
        vec![0x01, 3, 0, 0, 0, b'a', b'b', b'c'],
    ));
    cases.push(("stored cut short", vec![0x01, 3, 0, 0xfc, 0xff, b'a']));
    cases.push((
        "distance before the start (none written)",
        fixed_block(true, |w| {
            fixed(w, 257);
            code(w, 0, 5);
            fixed(w, 256);
        }),
    ));
    cases.push((
        "distance before the start (past one byte)",
        fixed_block(true, |w| {
            fixed(w, b'a' as u32);
            fixed(w, 257);
            code(w, 1, 5); // distance 2
            fixed(w, 256);
        }),
    ));
    for (name, s) in [("length symbol 286", 286), ("length symbol 287", 287)] {
        cases.push((
            name,
            fixed_block(true, |w| {
                fixed(w, b'a' as u32);
                fixed(w, s);
                code(w, 0, 5);
                fixed(w, 256);
            }),
        ));
    }
    for (name, s) in [("distance symbol 30", 30), ("distance symbol 31", 31)] {
        cases.push((
            name,
            fixed_block(true, |w| {
                fixed(w, b'a' as u32);
                fixed(w, 257);
                code(w, s, 5);
                fixed(w, 256);
            }),
        ));
    }
    cases.push((
        "fixed block with no end of block",
        fixed_block(true, |w| {
            fixed(w, b'a' as u32);
        }),
    ));
    cases.push(("empty stream", vec![]));
    // HLIT 288: the field's 31 (286 is the most)
    {
        let mut w = BitWriter::new();
        w.bits(3, 0b101);
        w.bits(5, 31);
        w.bits(5, 0);
        w.bits(4, 0);
        for _ in 0..4 {
            w.bits(3, 1);
        }
        w.bits(32, 0);
        w.bits(32, 0);
        cases.push(("HLIT 288", finish(w)));
    }
    // repeat 16 first, runs past HLIT+HDIST, oversubscribed code length code
    {
        let header = |f: &dyn Fn(&mut BitWriter)| {
            let mut w = BitWriter::new();
            w.bits(3, 0b101);
            w.bits(5, 0);
            w.bits(5, 0);
            w.bits(4, 0);
            f(&mut w);
            w.bits(32, 0);
            finish(w)
        };
        // lengths of 16 and 0 are 1 bit each: 0 is the code 0, 16 the code 1
        cases.push((
            "repeat 16 first",
            header(&|w| {
                for l in [1, 0, 0, 1] {
                    w.bits(3, l);
                }
                code(w, 1, 1);
                w.bits(2, 0);
            }),
        ));
        // 18 (code 1) and 0 (code 0) one bit each; 18 twice, of 138 zeros: past 258
        cases.push((
            "run past HLIT+HDIST",
            header(&|w| {
                for l in [0, 0, 1, 1] {
                    w.bits(3, l);
                }
                for _ in 0..2 {
                    code(w, 1, 1);
                    w.bits(7, 127);
                }
            }),
        ));
        cases.push((
            "oversubscribed code length code",
            header(&|w| {
                for l in [1, 1, 1, 0] {
                    w.bits(3, l);
                }
            }),
        ));
        cases.push((
            "repeat 17 first",
            header(&|w| {
                for l in [0, 1, 0, 1] {
                    w.bits(3, l);
                }
                code(w, 1, 1);
                w.bits(3, 0);
            }),
        ));
    }
    // oversubscribed literal/length code and distance code
    {
        let mut l = simple_lens();
        l[98] = 1; // three symbols of one bit
        cases.push((
            "oversubscribed literal code",
            dynamic(257, 1, &l, |w| code(w, 0, 1)),
        ));
        let mut l = simple_lens();
        l.extend([1, 1]);
        cases.push((
            "oversubscribed distance code",
            dynamic(257, 3, &l, |w| code(w, 1, 1)),
        ));
        // no end of block symbol in the code, then the stream ends
        let mut l = simple_lens();
        l[256] = 0;
        l[98] = 1;
        cases.push((
            "no end of block symbol",
            dynamic(257, 1, &l, |w| code(w, 0, 1)),
        ));
        // a code used where it has no symbol (incomplete)
        let mut l = vec![0u8; 258];
        l[256] = 2;
        l[97] = 2;
        l[257] = 1;
        cases.push((
            "undefined code",
            dynamic(257, 1, &l, |w| {
                code(w, 3, 2);
                code(w, 3, 2);
            }),
        ));
    }
    // a length with a distance before the start in a dynamic block
    {
        let mut l = vec![0u8; 258];
        l[b'a' as usize] = 2;
        l[256] = 2;
        l[257] = 2;
        l.extend([1, 1]);
        // lit: 'a'=00 256=01 257=10; dist 0 and 1 one bit each: the distance 2 of one byte
        let raw = dynamic(258, 2, &l, |w| {
            code(w, 0, 2);
            code(w, 2, 2);
            code(w, 1, 1);
            code(w, 1, 2);
        });
        cases.push(("dynamic distance before the start", raw));
    }
    let mut bad = Vec::new();
    for (name, raw) in &cases {
        let r = std::panic::catch_unwind(|| check(name, raw, None));
        if r.is_err() {
            bad.push(*name);
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}

/// A stream's bits that no code reads (the padding of its last byte, of a stored block's
/// header, the bytes after it) are kept in the record: the stream is made again byte for byte.
#[test]
fn padding_and_trailing_bytes_are_kept() {
    let text: Vec<u8> = (0..3000)
        .flat_map(|i| format!("{} {}\n", i % 97, i % 13).into_bytes())
        .collect();
    // a fixed block: the last byte's padding set (31 bits: one pad bit)
    let fixed_raw = fixed_block(true, |w| {
        fixed(w, b'a' as u32);
        fixed(w, 257);
        code(w, 0, 5);
        fixed(w, 256);
    });
    assert_eq!(fixed_raw.len(), 4);
    let mut padded = fixed_raw.clone();
    padded[3] |= 0x80;
    // a stored block with its header's padding set, then a fixed one
    let stored = |pad: u32, body: &[u8], last: bool| {
        let mut w = BitWriter::new();
        w.bits(1, last as u32);
        w.bits(2, 0);
        w.bits(5, pad);
        w.bits(16, body.len() as u32);
        w.bits(16, !(body.len() as u32) & 0xffff);
        let mut v = finish(w);
        v.extend_from_slice(body);
        v
    };
    let mut mixed = stored(0b10101, b"hello", false);
    mixed.extend(fixed_block(true, |w| {
        fixed(w, b'x' as u32);
        fixed(w, 256);
    }));
    let alone = stored(0b11111, &text[..500], true);
    let with_tail = |raw: &[u8], tail: &[u8]| [raw, tail].concat();
    let z = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&text)
        .unwrap();
    let mut z_padded = z.clone();
    // (the real encoder's last byte, its bits above the stream's end set when there are any)
    let (d, m) = aqr_flate::deconstruct(&z).unwrap();
    assert_eq!(aqr_flate::reconstruct(&d, &m).unwrap(), z);
    *z_padded.last_mut().unwrap() |= 0x80;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("fixed", fixed_raw.clone()),
        ("fixed, padding set", padded.clone()),
        (
            "fixed, padding set, trailing bytes",
            with_tail(&padded, b"\xff\0trail"),
        ),
        ("fixed, trailing zeros", with_tail(&fixed_raw, &[0; 7])),
        ("stored padding", mixed),
        ("stored padding, only", alone.clone()),
        ("stored padding, trailing", with_tail(&alone, b"\x01\x02")),
        ("zlib, last bits set", z_padded),
        ("zlib, trailing bytes", with_tail(&z, b"\x78\x9c")),
    ];
    for profile in [0, aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE] {
        let opts = aqr_flate::Options::for_profile(profile).unwrap();
        for (name, raw) in &cases {
            let r = aqr_flate::deconstruct_with(raw, &opts);
            let Ok((data, meta)) = r else {
                panic!("{name} (profile {profile}): {:?}", r.err())
            };
            let back = aqr_flate::reconstruct(&data, &meta).unwrap();
            assert!(&back == raw, "{name} (profile {profile}) is not made again");
        }
    }
}

/// Damaged streams (an encoder's, with every kind of block) are read alike by every entry
/// point: all fail, or all give the same data; the record of one that is read makes it again.
#[test]
fn damaged_streams_are_read_alike() {
    let text: Vec<u8> = (0..400)
        .flat_map(|i| format!("{} {}\n", i % 97, i % 13).into_bytes())
        .collect();
    let mut raws = Vec::new();
    for spec in ["zlib:level=6", "zlib:level=1", "zlib:level=0"] {
        let enc = spec.parse::<flate_zoo::Encoder>().unwrap();
        let z = enc.compress(&text).unwrap();
        raws.push(z);
    }
    for raw in raws {
        aqr_testkit::damaged(&raw, 40, 24, Duration::from_secs(20), |v| {
            let a = aqr_flate::inflate::parse(v).ok();
            let b = aqr_flate::deflate::parse(v).ok();
            assert_eq!(
                a.as_ref().map(|f| f.data.to_vec()),
                b.as_ref().map(|x| x.1.clone())
            );
            if let Some(f) = &a {
                let n = f.data.len();
                // (`data` wants the stream to end where it does: trailing bytes are an error)
                if f.stream.trailing.is_empty() {
                    assert_eq!(
                        aqr_flate::inflate::data(v, n).ok().as_deref(),
                        Some(&f.data[..])
                    );
                }
            }
            if let Ok((data, meta)) = aqr_flate::deconstruct(v) {
                assert_eq!(aqr_flate::reconstruct(&data, &meta).unwrap(), v);
                assert_eq!(Some(&data[..]), a.as_ref().map(|f| &f.data[..]));
            }
        });
    }
}
