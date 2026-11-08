//! `png-recons` as a part module ([`part::run`] through [`part::decode_streamed`]: the five inputs
//! pulled in pieces, the file put out in pieces) against the whole decoding, on files of every
//! format and shape; and on inputs cut short, extended and damaged.

mod common;

use aqr_api::stream::{Pull, Put};
use aqr_png::{formula, part};
use common::*;

/// The five inputs of the formula of `file`.
fn inputs(file: &[u8]) -> Option<Vec<Vec<u8>>> {
    let f = formula::encode(file, 1)?;
    assert_eq!(f.streams.len(), 5);
    Some(f.streams.iter().map(|s| s.data.to_vec()).collect())
}

struct Few<'a>(&'a [u8], usize);

impl Pull for Few<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.len().min(buf.len()).min(self.1);
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

struct Vecs(Vec<u8>);

impl Put for Vecs {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        self.0.extend_from_slice(d);
        Some(())
    }
}

/// The part module's output on `i`, pulled `k` bytes at a time.
fn part_few(i: &[Vec<u8>], k: usize) -> aqr_png::Result<Vec<u8>> {
    let mut srcs: Vec<Few> = i.iter().map(|v| Few(v, k)).collect();
    let [s0, s1, s2, s3, s4] = &mut srcs[..] else { unreachable!() };
    let lens: Vec<u64> = i.iter().map(|v| v.len() as u64).collect();
    let mut out = Vecs(Vec::new());
    part::run([(s0, lens[0]), (s1, lens[1]), (s2, lens[2]), (s3, lens[3]), (s4, lens[4])], &mut out)?;
    Ok(out.0)
}

fn both(i: &[Vec<u8>], len: u64) -> (aqr_png::Result<Vec<u8>>, aqr_png::Result<Vec<u8>>) {
    let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
    (formula::decode(&[], &r, len, 1), part::decode_streamed(&[], &r, len))
}

#[track_caller]
fn same(what: &str, file: &[u8]) {
    let Some(i) = inputs(file) else { return };
    let (whole, part) = both(&i, file.len() as u64);
    assert!(whole.as_deref().ok() == Some(file), "{what}: whole");
    assert!(part.as_deref().ok() == Some(file), "{what}: part");
    for k in [1, 13, 4093, 1 << 20] {
        assert!(part_few(&i, k).as_deref().ok() == Some(file), "{what}: {k} at a time");
    }
    assert!(both(&i, file.len() as u64 + 1).1.is_err(), "{what}: longer");
}

#[test]
fn every_format() {
    for (depth, color) in [(1, 0), (4, 0), (8, 0), (16, 0), (8, 2), (16, 2), (4, 3), (8, 3), (8, 4), (8, 6), (16, 6)] {
        for interlace in [0, 1] {
            let data = image_data(137, 99, depth, color, interlace == 1, depth as u32 * 7 + color as u32);
            let mut cs = vec![(b"IHDR", ihdr(137, 99, depth, color, interlace))];
            if color == 3 {
                cs.push((b"PLTE", (0..768).map(|i| (i * 7) as u8).collect()));
            }
            cs.extend(idats(&zlib(&zlib6(), &data), 8192));
            cs.push((b"IEND", vec![]));
            same(&format!("{depth} bits, colour {color}, interlace {interlace}"), &png(&cs));
        }
    }
}

#[test]
fn large_images_and_apng() {
    // (past the pieces and the windows: tens of IDAT chunks, hundreds of blocks)
    let data = image_data(1500, 900, 8, 6, false, 5);
    let cs = [(b"IHDR", ihdr(1500, 900, 8, 6, 0)), (b"IDAT", zlib(&zlib6(), &data)), (b"IEND", vec![])];
    same("one IDAT", &png(&cs));
    let mut cs = vec![(b"IHDR", ihdr(1500, 900, 8, 6, 0))];
    cs.extend(idats(&zlib(&zlib6(), &data), 3001));
    cs.push((b"IEND", vec![]));
    let mut file = png(&cs);
    file.extend_from_slice(b"trailing bytes");
    same("many IDATs", &file);
    same("frames", &apng(&[(64, 48), (32, 32), (64, 48), (300, 200)], 1));
    same("frames 2", &apng(&[(10, 10); 9], 2));
}

#[test]
fn kept_and_coded_images_among_each_other() {
    // a frame of noise that a model does not make smaller stays as it is, among coded ones
    let file = apng(&[(40, 40), (64, 48), (50, 30)], 3);
    same("apng", &file);
    let noise: Vec<u8> = {
        let mut x = 1u32;
        (0..40)
            .flat_map(|_| {
                std::iter::once(0)
                    .chain((0..60).map(|_| {
                        x = x.wrapping_mul(1103515245).wrapping_add(12345);
                        (x >> 16) as u8
                    }))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    same("noise", &png(&[(b"IHDR", ihdr(60, 40, 8, 0, 0)), (b"IDAT", zlib(&zlib6(), &noise)), (b"IEND", vec![])]));
}

#[test]
fn damaged_inputs_fail_or_agree() {
    let data = image_data(200, 120, 8, 2, false, 9);
    let file = png(&[(b"IHDR", ihdr(200, 120, 8, 2, 0)), (b"IDAT", zlib(&zlib6(), &data)), (b"IEND", vec![])]);
    let i = inputs(&file).expect("coded");
    let len = file.len() as u64;
    let check = |j: &[Vec<u8>], s: usize, what: &str| {
        let r: Vec<&[u8]> = j.iter().map(|v| &v[..]).collect();
        let (a, b) = (formula::decode(&[], &r, len, 1), part::decode_streamed(&[], &r, len));
        assert!(
            a.is_err() == b.is_err() && (a.is_err() || a.as_ref().ok() == b.as_ref().ok()),
            "input {s} {what}: whole {:?}, part {:?}",
            a.as_ref().map(|v| v.len()),
            b.as_ref().map(|v| v.len())
        );
    };
    for s in 0..i.len() {
        for cut in [0, i[s].len() / 2, i[s].len().saturating_sub(1)] {
            let mut j = i.clone();
            j[s].truncate(cut);
            check(&j, s, &format!("cut at {cut}"));
        }
        let mut j = i.clone();
        j[s].push(7);
        check(&j, s, "extended");
        for at in (0..i[s].len()).step_by(i[s].len() / 40 + 1) {
            let mut j = i.clone();
            j[s][at] ^= 0x5a;
            check(&j, s, &format!("damaged at {at}"));
        }
    }
}

#[test]
fn frames_as_tasks_and_one_at_a_time() {
    // many small frames: batches of them are decoded as tasks, the order of the file kept
    same("many frames", &apng(&[(48, 32); 37], 4));
    same(
        "frames of sizes",
        &apng(&[(10, 10), (200, 150), (7, 300), (64, 64), (1, 1), (333, 20), (50, 50), (20, 20), (90, 90), (40, 5)], 8),
    );
    // a frame past the size of a task's among small ones: all one at a time in the instance
    same("a large frame", &apng(&[(40, 40), (2400, 1500), (40, 40)], 5));
}
