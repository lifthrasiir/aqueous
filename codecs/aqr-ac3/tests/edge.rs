//! Edge cases: AC-3 syncframes written by the test (silent blocks, valid checksums) of every channel
//! layout and frame size, round trips of them, the stream's records around damaged and false frames,
//! dense false syncwords, and damaged or lying coded streams.

use std::time::{Duration, Instant};

use aqr_ac3::{formula, module, native, stream};
use aqr_testkit::{Refusing, case, damaged, within};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(60);

struct Bits(Vec<u8>, usize);

impl Bits {
    fn new() -> Self {
        Bits(Vec::new(), 0)
    }
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.1.is_multiple_of(8) {
                self.0.push(0);
            }
            let b = (v >> i & 1) as u8;
            *self.0.last_mut().unwrap() |= b << (7 - self.1 % 8);
            self.1 += 1;
        }
    }
}

fn crc16(d: &[u8], mut crc: u16) -> u16 {
    for &b in d {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { crc << 1 ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

/// Sets crc1 (bytes 2..4) so that the CRC of the bytes up to the 5/8 point is zero, and crc2 (the last two bytes).
fn set_crcs(f: &mut [u8]) {
    let n = f.len();
    let words = n / 2;
    let n58 = ((words >> 1) + (words >> 3)) * 2;
    f[2] = 0;
    f[3] = 0;
    // the register after the bytes is linear in crc1's two: a basis of the images of the unit vectors
    let target = crc16(&f[2..n58], 0);
    let zeros = vec![0u8; n58 - 4];
    let mut basis = [(0u16, 0u16); 16];
    for i in 0..16 {
        let m = 1u16 << i;
        let mut v = crc16(&zeros, crc16(&[(m >> 8) as u8, m as u8], 0));
        let mut mm = m;
        for bit in (0..16).rev() {
            if v >> bit & 1 == 0 {
                continue;
            }
            if basis[bit].0 != 0 {
                v ^= basis[bit].0;
                mm ^= basis[bit].1;
            } else {
                basis[bit] = (v, mm);
                break;
            }
        }
    }
    let (mut s, mut c) = (target, 0u16);
    for bit in (0..16).rev() {
        if s >> bit & 1 != 0 {
            s ^= basis[bit].0;
            c ^= basis[bit].1;
        }
    }
    assert_eq!(s, 0);
    f[2] = (c >> 8) as u8;
    f[3] = c as u8;
    assert_eq!(crc16(&f[2..n58], 0), 0);
    let c2 = crc16(&f[2..n - 2], 0);
    f[n - 2] = (c2 >> 8) as u8;
    f[n - 1] = c2 as u8;
    assert_eq!(crc16(&f[2..], 0), 0);
}

/// A silent syncframe: the exponents of the first block (all of one value), none of the others' (reused), no
/// mantissas, the frame filled by the blocks' skip fields. `None` where the frame is too small for the layout.
fn silent(fscod: u32, frm: u32, acmod: u32, lfe: bool, tag: u32) -> Option<Vec<u8>> {
    let total = aqr_ac3::frame_len(&[0x0B, 0x77, 0, 0, (fscod << 6 | frm) as u8])? * 8;
    let nf = [2, 1, 2, 3, 3, 4, 4, 5][acmod as usize];
    let build = |skips: &[u32; 6], pad: u32| -> Bits {
        let mut b = Bits::new();
        b.put(0x0B77, 16);
        b.put(0, 16);
        b.put(fscod, 2);
        b.put(frm, 6);
        b.put(8, 5);
        b.put(0, 3);
        b.put(acmod, 3);
        if acmod & 1 != 0 && acmod != 1 {
            b.put(1, 2);
        }
        if acmod & 4 != 0 {
            b.put(1, 2);
        }
        if acmod == 2 {
            b.put(0, 2);
        }
        b.put(lfe as u32, 1);
        b.put(31, 5);
        b.put(0, 3); // compre, langcode, audprodie
        if acmod == 0 {
            b.put(31, 5);
            b.put(0, 3);
        }
        b.put(0, 2); // copyrightb, origbs
        b.put(0, 3); // timecod1e, timecod2e, addbsie
        #[allow(clippy::needless_range_loop)]
        for blk in 0..6 {
            for _ in 0..2 * nf {
                b.put(0, 1); // blksw, dithflag
            }
            b.put(0, 1); // dynrnge
            if acmod == 0 {
                b.put(0, 1);
            }
            if blk == 0 {
                b.put(1, 1); // cplstre
                b.put(0, 1); // cplinu
            } else {
                b.put(0, 1);
            }
            if acmod == 2 {
                if blk == 0 {
                    b.put(1, 1);
                    b.put(0, 4);
                } else {
                    b.put(0, 1);
                }
            }
            for _ in 0..nf {
                b.put(if blk == 0 { 3 } else { 0 }, 2); // chexpstr: D45, then reuse
            }
            if lfe {
                b.put((blk == 0) as u32, 1);
            }
            if blk == 0 {
                for _ in 0..nf {
                    b.put(0, 6); // chbwcod
                }
                for ch in 0..nf {
                    b.put(5 + (tag + ch as u32) % 12, 4);
                    for _ in 0..6 {
                        b.put(62, 7); // no change
                    }
                    b.put(0, 2);
                }
                if lfe {
                    b.put(4, 4);
                    b.put(62, 7);
                    b.put(62, 7);
                }
                b.put(1, 1); // baie
                b.put(0, 11);
                b.put(1, 1); // snroffste
                b.put(0, 6);
                for _ in 0..nf {
                    b.put(0, 7);
                }
                if lfe {
                    b.put(0, 7);
                }
            } else {
                b.put(0, 2); // baie, snroffste
            }
            b.put(0, 1); // deltbaie
            let s = skips[blk];
            if s > 0 {
                b.put(1, 1);
                b.put(s, 9);
                for i in 0..s {
                    b.put((i * 7 + tag) & 0xff, 8);
                }
            } else {
                b.put(0, 1);
            }
        }
        b.put(0, pad);
        b.put(0, 2); // auxdatae, crcrsv
        b.put(0, 16);
        b
    };
    let used = build(&[0; 6], 0).1;
    let rest = total.checked_sub(used)? as u32;
    // a skip of `s` bytes costs 9 + 8 s bits (s up to 511); what is left is padding (32 bits at most)
    let mut skips = [0u32; 6];
    let mut pad = rest;
    if rest > 32 {
        let k = (1..=6u32).find(|&k| rest >= 9 * k && (rest - 9 * k) / 8 <= 511 * k)?;
        pad = (rest - 9 * k) % 8;
        let mut left = (rest - 9 * k) / 8;
        for (i, s) in skips.iter_mut().enumerate().take(k as usize) {
            let share = left / (k - i as u32);
            *s = share;
            left -= share;
        }
        // a skip of 0 bytes is still a skip field: 9 bits (the writer puts one for s > 0 only)
        if skips.iter().take(k as usize).any(|&s| s == 0) {
            return None;
        }
    }
    let b = build(&skips, pad);
    assert_eq!(b.1, total);
    let mut f = b.0;
    set_crcs(&mut f);
    Some(f)
}

#[test]
fn writer_makes_frames_that_check() {
    let f = silent(0, 20, 2, false, 0).unwrap();
    assert_eq!(f.len(), aqr_ac3::frame_len(&f).unwrap());
    assert!(aqr_ac3::check(&f));
}

fn roundtrip(data: &[u8], expect_frames: usize) {
    let recs = stream::records(data);
    assert_eq!(recs.iter().filter(|r| r.frame).count(), expect_frames);
    let coded = formula::encode_always(data, 2);
    let a = formula::decode(&[], &[&coded], data.len() as u64, 2).unwrap();
    assert!(a == data, "native");
    let b = module::decode_serial(&[], &[&coded], data.len() as u64, 1).unwrap();
    assert!(b == data, "serial");
}

#[test]
fn layouts_and_sizes() {
    let mut n = 0;
    for acmod in 0..8 {
        for lfe in [false, true] {
            for (fscod, frm) in [(0, 14), (0, 36), (1, 21), (2, 10), (2, 37), (1, 34), (0, 20)] {
                let Some(f) = silent(fscod, frm, acmod, lfe, n) else { continue };
                assert!(aqr_ac3::check(&f), "acmod {acmod} lfe {lfe} {fscod}/{frm}");
                roundtrip(&f, 1);
                n += 1;
            }
        }
    }
    assert!(n > 40, "{n} layouts made");
}

#[test]
fn mixed_layouts_in_one_stream() {
    let mut data = Vec::new();
    let mut frames = 0;
    for i in 0..30u32 {
        let (acmod, lfe) = [(1, false), (2, false), (7, true), (0, false)][i as usize % 4];
        data.extend(silent(0, 24 + i % 4, acmod, lfe, i).expect("frame"));
        frames += 1;
        if i % 5 == 0 {
            data.extend([1, 2, 3, 0x0B, 0x77, 9]);
        }
    }
    roundtrip(&data, frames);
    let coded = formula::encode_always(&data, 1);
    assert!(coded.len() < data.len(), "{} -> {}", data.len(), coded.len());
}

#[test]
fn smallest_and_largest_frames() {
    // 64 words: the smallest; and the largest the writer fills (its skip fields hold at most 6 x 511 bytes)
    let small = silent(0, 0, 1, false, 3).unwrap();
    assert_eq!(small.len(), 128);
    roundtrip(&small, 1);
    let big = silent(0, 37, 1, false, 3).unwrap();
    assert_eq!(big.len(), 2560);
    roundtrip(&big, 1);
}

#[test]
fn frames_with_bad_checksums_or_cut_are_kept_as_bytes() {
    let f = silent(0, 18, 2, false, 1).unwrap();
    for at in [2usize, 3, f.len() - 1, f.len() / 2] {
        let mut bad = f.clone();
        bad[at] ^= 0x10;
        let mut data = f.clone();
        data.extend(&bad);
        data.extend(&f);
        roundtrip(&data, 2);
    }
    // cut: the next frame's start taken for its end
    let mut data = f[..f.len() - 1].to_vec();
    data.extend(&f);
    roundtrip(&data, 1);
    // a frame at the very end, cut
    let mut data = f.clone();
    data.extend(&f[..f.len() - 2]);
    roundtrip(&data, 1);
    // nothing at all
    let coded = formula::encode_always(&[], 1);
    assert_eq!(formula::decode(&[], &[&coded], 0, 1).unwrap(), Vec::<u8>::new());
}

#[test]
fn syncwords_before_frames() {
    let f = silent(0, 14, 2, false, 1).unwrap();
    let g = silent(0, 10, 1, false, 2).unwrap();
    let mut data = vec![0x0B, 0x77, 0x0B, 0x77];
    data.extend(&f);
    data.extend([0x0B, 0x77]);
    data.extend(&g);
    roundtrip(&data, 2);
}

#[test]
fn dense_false_syncwords_are_not_slow() {
    // every 5 bytes a syncword and a header of a large frame: the checksums are tried at each
    for tail in [0x25u8, 0x65, 0xA5, 0x00] {
        let mut data = Vec::new();
        while data.len() < 1 << 20 {
            data.extend([0x0B, 0x77, 0, 0, tail]);
        }
        let t = Instant::now();
        let r = within(Duration::from_secs(60), || stream::records(&data).len());
        let dt = t.elapsed();
        assert!(r.is_ok(), "tail {tail:#x}: {r:?}");
        assert!(dt < Duration::from_secs(5), "tail {tail:#x}: {dt:?} for {} bytes", data.len());
    }
    let data: Vec<u8> = (0..1 << 20).map(|i| if i % 2 == 0 { 0x0B } else { 0x77 }).collect();
    let t = Instant::now();
    assert_eq!(stream::records(&data).iter().filter(|r| r.frame).count(), 0);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn damaged_coded_streams_fail_cleanly() {
    let mut data = Vec::new();
    for i in 0..6 {
        data.extend(silent(0, 16, [1, 2, 7][i % 3], i % 2 == 0, i as u32).unwrap());
        data.extend([0x0B, 0x77, 0, 1, 2]);
    }
    let coded = formula::encode_always(&data, 1);
    let len = data.len() as u64;
    damaged(&coded, 60, 40, LIMIT, |d| {
        // an error or some output: never a panic, a hang, an allocation of gigabytes
        let _ = native::decode(&[], &[d], len, 2);
        let _ = module::decode_serial(&[], &[d], len, 1);
    });
}

#[test]
fn damaged_streams_encode_and_decode() {
    let mut data = Vec::new();
    for i in 0..4 {
        data.extend(silent(1, 12, [2, 7][i % 2], i % 2 == 1, i as u32).unwrap());
    }
    damaged(&data, 80, 40, LIMIT, |d| {
        let coded = formula::encode_always(d, 1);
        let back = formula::decode(&[], &[&coded], d.len() as u64, 1).unwrap();
        assert!(back == d, "round trip");
        assert!(module::decode_serial(&[], &[&coded], d.len() as u64, 1).unwrap() == d);
    });
}

#[test]
fn lying_chunk_tables() {
    case("lying chunk tables");
    let table = |ents: &[(u64, u64)]| {
        let mut v = Vec::new();
        aqr_common::varint::put(&mut v, ents.len() as u64);
        for &(a, b) in ents {
            aqr_common::varint::put(&mut v, a);
            aqr_common::varint::put(&mut v, b);
        }
        v
    };
    for ents in [
        vec![(1u64 << 50, 3u64)],
        vec![(1 << 63, 3)],
        vec![(u64::MAX, 1), (u64::MAX, 1)],
        vec![(10, u64::MAX)],
        vec![(10, 1 << 40)],
        vec![(0, 0); 1000],
        vec![(5, 0)],
    ] {
        let mut coded = table(&ents);
        coded.extend([0u8; 3]);
        let total = ents.iter().fold(0u64, |a, e| a.wrapping_add(e.0));
        for len in [total, 10, 0, 1 << 50] {
            let t = Instant::now();
            let r = within(Duration::from_secs(20), || {
                let _ = native::decode(&[], &[&coded], len, 2);
                let _ = module::decode_serial(&[], &[&coded], len, 1);
            });
            assert!(t.elapsed() < Duration::from_secs(2), "{} chunks, len {len}: {:?}", ents.len(), t.elapsed());
            assert!(r.is_ok(), "{ents:?} len {len}: {r:?}");
        }
    }
}

/// Each chunk starts a model of its own: a table of many chunks that decode to nothing or to very little must not
/// cost a model's memory apiece.
#[test]
fn many_empty_chunks_are_cheap() {
    for bytes in [0u64, 1] {
        let mut coded = Vec::new();
        aqr_common::varint::put(&mut coded, 20000);
        for _ in 0..20000 {
            aqr_common::varint::put(&mut coded, bytes);
            aqr_common::varint::put(&mut coded, 0);
        }
        let total = bytes * 20000;
        let t = Instant::now();
        let r = within(Duration::from_secs(60), || {
            let _ = native::decode(&[], &[&coded], total, 1);
            let _ = module::decode_serial(&[], &[&coded], total, 1);
        });
        assert!(r.is_ok());
        assert!(t.elapsed() < Duration::from_secs(1), "{bytes} bytes a chunk: {:?}", t.elapsed());
    }
}
