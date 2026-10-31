//! Chunks made by a small LZX encoder of this test (greedy matches, Huffman codes of the
//! block's frequencies, runs of the pretree taken or not at random), read and written back,
//! and split into data and record and rebuilt.

use aqr_common::rng::Rng;
use aqr_lzx::lzx::{self, Block, Lens, Parse, PreSym, Tok, TreeCode, Variant};
use aqr_lzx::recons::{self, CHUNK, huffman};

/// How the pretree writes `target` over `before`: runs taken where `runs` says.
fn tree_code(before: &[u8], target: &[u8], rng: &mut Rng, runs: bool) -> TreeCode {
    let mut t = TreeCode::default();
    let mut i = 0;
    while i < target.len() {
        let zeros = target[i..].iter().take_while(|&&l| l == 0).count();
        let same = target[i..].iter().take_while(|&&l| l == target[i]).count();
        let take = runs && rng.below(4) != 0;
        if take && zeros >= 20 {
            let n = zeros.min(51);
            t.syms.push(PreSym { sym: 18, extra: (n - 20) as u8, then: 0 });
            i += n;
        } else if take && zeros >= 4 && rng.below(2) == 0 {
            let n = zeros.min(19);
            t.syms.push(PreSym { sym: 17, extra: (n - 4) as u8, then: 0 });
            i += n;
        } else if take && same >= 4 {
            let n = same.min(4 + rng.below(2) as usize);
            let then = (before[i] + 17 - target[i]) % 17;
            t.syms.push(PreSym { sym: 19, extra: (n - 4) as u8, then });
            i += n;
        } else {
            t.syms.push(PreSym { sym: (before[i] + 17 - target[i]) % 17, extra: 0, then: 0 });
            i += 1;
        }
    }
    let mut freqs = [0u32; lzx::PRE];
    for s in &t.syms {
        freqs[s.sym as usize] += 1;
        if s.sym == 19 {
            freqs[s.then as usize] += 1;
        }
    }
    huffman(&freqs, 15, &mut t.pre);
    t
}

/// A chunk of `d` (as e8 translates it): blocks at the sizes given (an uncompressed one where
/// the size is negative), greedy matches from a hash chain, repeat offsets where they match.
fn encode(d: &[u8], sizes: &[isize], rng: &mut Rng) -> Vec<u8> {
    encode_as(d, sizes, rng, lzx::WIM, None)
}

/// [`encode`] of a stream of `v`, with header `e8` (CAB's).
fn encode_as(d: &[u8], sizes: &[isize], rng: &mut Rng, v: Variant, e8: Option<u32>) -> Vec<u8> {
    let mut parse = Parse { e8, ..Parse::default() };
    let mut lens = Lens::new(v);
    let main = v.main();
    let mut reps = lzx::REPS;
    let mut at = 0;
    for (bi, &sz) in sizes.iter().enumerate() {
        let last = bi + 1 == sizes.len();
        let size = if last { d.len() - at } else { sz.unsigned_abs() };
        let end = at + size;
        let mut b = Block { size, default_size: !v.cab && size == CHUNK, ..Block::default() };
        if sz < 0 && !last {
            b.kind = lzx::UNCOMPRESSED;
            b.reps = reps;
            b.pad_byte = (size % 2 == 1).then_some(0);
            parse.blocks.push(b);
            at = end;
            continue;
        }
        b.kind = if rng.below(2) == 0 { lzx::VERBATIM } else { lzx::ALIGNED_BLOCK };
        let first = parse.toks.len();
        let mut pos = at;
        while pos < end {
            // (CAB's: no match over a frame's end, as decoders take frames one at a time)
            let stop = if v.cab { end.min((pos / lzx::FRAME + 1) * lzx::FRAME) } else { end };
            let cap = (stop - pos).min(lzx::MAX_MATCH);
            let mut best = (0, 0, None);
            for (k, &r) in reps.iter().enumerate() {
                let r = r as usize;
                if r <= pos {
                    let n = (0..cap).take_while(|&i| d[pos - r + i] == d[pos + i]).count();
                    if n >= 2 && n > best.0 {
                        best = (n, r, Some(k as u8));
                    }
                }
            }
            for o in 1..=pos.min(4096) {
                let n = (0..cap).take_while(|&i| d[pos - o + i] == d[pos + i]).count();
                if n >= 3 && n > best.0 + 1 {
                    best = (n, o, None);
                }
            }
            match best {
                (0, ..) => {
                    parse.toks.push(Tok::Lit);
                    pos += 1;
                }
                (n, o, rep) => {
                    // (now and then shorter than it could be)
                    let n = if n > 4 && rng.below(5) == 0 { n - 1 } else { n };
                    match rep {
                        Some(k) => reps.swap(0, k as usize),
                        None => reps = [o as u32, reps[0], reps[1]],
                    }
                    parse.toks.push(Tok::Match { len: n as u16, off: o as u32, rep });
                    pos += n;
                }
            }
        }
        b.toks = parse.toks.len() - first;
        // the trees of the block's frequencies
        let (mut fm, mut fl, mut fa) = ([0u32; lzx::MAIN_MAX], [0u32; lzx::LEN], [0u32; lzx::ALIGNED]);
        let mut p = at;
        for tok in &parse.toks[first..] {
            match *tok {
                Tok::Lit => {
                    fm[d[p] as usize] += 1;
                    p += 1;
                }
                Tok::Match { len, off, rep } => {
                    let h = (len as usize - 2).min(7);
                    let slot = rep.map_or_else(|| lzx::slot(off + 2), |k| k as usize);
                    fm[256 + slot * 8 + h] += 1;
                    if h == 7 {
                        fl[len as usize - 9] += 1;
                    }
                    if rep.is_none() && lzx::EXTRA[slot] >= 3 {
                        fa[((off + 2 - lzx::BASE[slot]) & 7) as usize] += 1;
                    }
                    p += len as usize;
                }
            }
        }
        if b.kind == lzx::ALIGNED_BLOCK {
            huffman(&fa, 7, &mut b.aligned);
        }
        let (mut tm, mut tl) = (vec![0u8; main], [0u8; lzx::LEN]);
        huffman(&fm[..main], 16, &mut tm);
        huffman(&fl, 16, &mut tl);
        // (a length one off where it does not matter: a symbol of no use)
        if rng.below(3) == 0 && tl.iter().all(|&l| l == 0) {
            tl[rng.below(lzx::LEN as u64) as usize] = 1 + rng.below(16) as u8;
        }
        let runs = rng.below(2) == 0;
        b.trees[0] = tree_code(&lens.main[..256], &tm[..256], rng, runs);
        b.trees[1] = tree_code(&lens.main[256..], &tm[256..], rng, runs);
        b.trees[2] = tree_code(&lens.len, &tl, rng, runs);
        lens.main = tm;
        lens.len = tl;
        parse.blocks.push(b);
        at = end;
    }
    // (CAB's: a frame's end in a block of tokens pads to a word, with zeros)
    if v.cab {
        let (mut pos, mut t, mut frame_end) = (0, 0, lzx::FRAME.min(d.len()));
        for b in &parse.blocks {
            if b.kind == lzx::UNCOMPRESSED {
                pos += b.size;
                while frame_end <= pos && frame_end < d.len() {
                    frame_end = (frame_end + lzx::FRAME).min(d.len());
                }
                continue;
            }
            for tok in &parse.toks[t..t + b.toks] {
                pos += match tok {
                    Tok::Lit => 1,
                    Tok::Match { len, .. } => *len as usize,
                };
                if pos >= frame_end && frame_end < d.len() {
                    parse.frame_pad.push(0);
                    frame_end = (frame_end + lzx::FRAME).min(d.len());
                }
            }
            t += b.toks;
        }
    }
    lzx::write_as(d, &parse, v).expect("a parse that holds")
}

/// Data of several kinds: text, runs, noise, structured records, x86-like calls (e8).
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        match rng.below(5) {
            0 => d.extend_from_slice(b"the quick brown fox jumps over the lazy dog; "),
            1 => d.extend(std::iter::repeat_n(rng.below(256) as u8, 1 + rng.below(300) as usize)),
            2 => d.extend((0..rng.below(200)).map(|_| rng.below(256) as u8)),
            3 => {
                let v = rng.below(1 << 20) as u32;
                for i in 0..8 {
                    d.extend_from_slice(&(v + i * 16).to_le_bytes());
                }
            }
            _ => {
                d.push(0xe8);
                d.extend_from_slice(&(rng.below(1 << 16) as i32 - 30000).to_le_bytes());
            }
        }
    }
    d.truncate(n);
    d
}

/// A stream: the file's data cut into chunks, each LZX (at random block sizes) or stored.
fn stream(rng: &mut Rng, n: usize) -> (Vec<u8>, Vec<(Vec<u8>, usize)>) {
    let file = data(rng, n);
    let chunks = file
        .chunks(CHUNK)
        .map(|raw| {
            if rng.below(8) == 0 {
                return (raw.to_vec(), raw.len());
            }
            let mut d = raw.to_vec();
            lzx::e8_encode(&mut d);
            let mut sizes = Vec::new();
            let mut left = d.len() as isize;
            while left > 0 && rng.below(3) == 0 {
                let s = 1 + rng.below(left as u64 / 2 + 1) as isize;
                sizes.push(if rng.below(4) == 0 { -s } else { s });
                left -= s;
            }
            sizes.push(left.max(1));
            (encode(&d, &sizes, rng), raw.len())
        })
        .collect();
    (file, chunks)
}

#[test]
fn e8_inverts() {
    let mut rng = Rng::new(1);
    for _ in 0..50 {
        let n = 1 + rng.below(5000) as usize;
        let d = data(&mut rng, n);
        let mut e = d.clone();
        lzx::e8_encode(&mut e);
        lzx::e8_decode(&mut e);
        assert_eq!(e, d);
    }
}

#[test]
fn chunks_read_and_written_back() {
    let mut rng = Rng::new(2);
    for _ in 0..20 {
        let n = 1 + rng.below(3 * CHUNK as u64) as usize;
        let (file, chunks) = stream(&mut rng, n);
        for ((c, n), raw) in chunks.iter().zip(file.chunks(CHUNK)) {
            if c.len() == *n {
                continue;
            }
            let (mut d, parse) = lzx::read(c, *n).expect("read");
            assert_eq!(lzx::write(&d, &parse).as_deref(), Some(&c[..]));
            lzx::e8_decode(&mut d);
            assert_eq!(d, raw);
        }
    }
}

#[test]
fn split_and_joined() {
    let mut rng = Rng::new(3);
    for _ in 0..20 {
        let n = 1 + rng.below(3 * CHUNK as u64) as usize;
        let (file, chunks) = stream(&mut rng, n);
        let cs: Vec<(&[u8], usize)> = chunks.iter().map(|(c, n)| (&c[..], *n)).collect();
        let sp = recons::split(&cs).expect("split");
        assert_eq!(sp.data, file);
        let all: Vec<u8> = chunks.iter().flat_map(|c| c.0.iter().copied()).collect();
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), all);
    }
}

/// Records that are noise, or cut short, fail or give something: never a panic.
#[test]
fn damaged_records() {
    let mut rng = Rng::new(4);
    let (_, chunks) = stream(&mut rng, 2 * CHUNK + 100);
    let cs: Vec<(&[u8], usize)> = chunks.iter().map(|(c, n)| (&c[..], *n)).collect();
    let sp = recons::split(&cs).unwrap();
    for i in 0..200 {
        let mut r = sp.record.clone();
        match i % 3 {
            0 => r.truncate(rng.below(r.len() as u64) as usize),
            1 => {
                let at = rng.below(r.len() as u64) as usize;
                r[at] ^= 1 << rng.below(8);
            }
            _ => r = (0..r.len()).map(|_| rng.below(256) as u8).collect(),
        }
        let _ = recons::join(&sp.data, &r);
    }
}

#[test]
fn empty_stream() {
    let sp = recons::split(&[]).unwrap();
    assert!(sp.data.is_empty());
    assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), Vec::<u8>::new());
}

/// Writes `tests/data/greedy.chunks`, a stream of this test's chunks for other crates' tests
/// (aqr-decompress runs the module on it as WebAssembly): each chunk its data's length and its
/// bytes' length (u32 little-endian), then its bytes.
#[test]
#[ignore]
fn write_fixture() {
    let mut rng = Rng::new(5);
    let (_, chunks) = stream(&mut rng, 3 * CHUNK + 1234);
    let mut out = Vec::new();
    for (c, n) in &chunks {
        out.extend_from_slice(&(*n as u32).to_le_bytes());
        out.extend_from_slice(&(c.len() as u32).to_le_bytes());
        out.extend_from_slice(c);
    }
    std::fs::create_dir_all(format!("{}/tests/data", env!("CARGO_MANIFEST_DIR"))).unwrap();
    std::fs::write(format!("{}/tests/data/greedy.chunks", env!("CARGO_MANIFEST_DIR")), out).unwrap();
}

/// A CAB stream of `n` bytes of data of a window of `2^bits` bytes: the file's data, (with e8
/// at random) its stream, its blocks at random sizes over the frames.
fn cab_stream(rng: &mut Rng, n: usize, bits: u8) -> (Vec<u8>, Vec<u8>) {
    let v = Variant::cab(bits).unwrap();
    let file = data(rng, n);
    let e8 = (rng.below(2) == 0).then(|| [12_000_000, 1 << 20, 3_000][rng.below(3) as usize]);
    let mut d = file.clone();
    if let Some(size) = e8 {
        lzx::e8_encode_frames(&mut d, size);
    }
    let mut sizes = Vec::new();
    let mut left = d.len() as isize;
    while left > 0 && rng.below(4) != 0 {
        let s = 1 + rng.below(left as u64 * 2 / 3 + 1) as isize;
        sizes.push(if rng.below(5) == 0 { -s } else { s });
        left -= s;
    }
    sizes.push(left.max(1));
    (file, encode_as(&d, &sizes, rng, v, e8))
}

/// CAB streams (windows of 32 KiB to 2 MiB, blocks over frames, uncompressed blocks, e8): read
/// and written back, their frames' ends where the parse aligns, split and joined.
#[test]
fn cab_streams() {
    let mut rng = Rng::new(6);
    for k in 0..24 {
        let bits = [15, 16, 17, 21][k % 4];
        let v = Variant::cab(bits).unwrap();
        let n = 1 + rng.below(4 * lzx::FRAME as u64 + 5000) as usize;
        let (file, src) = cab_stream(&mut rng, n, bits);
        let (mut d, parse) = lzx::read_as(&src, n, v).expect("read");
        assert_eq!(lzx::write_as(&d, &parse, v).as_deref(), Some(&src[..]));
        if let Some(size) = parse.e8 {
            lzx::e8_decode_frames(&mut d, size);
        }
        assert_eq!(d, file);
        let ends = lzx::frame_ends(&src, n, v).unwrap();
        assert_eq!(ends.len(), n.div_ceil(lzx::FRAME) - 1);
        assert!(ends.windows(2).all(|w| w[0] <= w[1]) && ends.last().is_none_or(|&e| e <= src.len()));
        let sp = recons::split_stream(&src, n, v).expect("split");
        assert_eq!(sp.data, file);
        let mut out = Vec::new();
        recons::join_stream_onto(&sp.data, &sp.record, v, &mut out).unwrap();
        assert_eq!(out, src);
        // (the record of another variant gives nothing, or something else: no panic)
        let mut out = Vec::new();
        let _ = recons::join_stream_onto(&sp.data, &sp.record, Variant::cab(bits.max(16) - 1).unwrap(), &mut out);
    }
}

/// CAB streams' records damaged: fail or give something, never a panic.
#[test]
fn cab_damaged_records() {
    let mut rng = Rng::new(7);
    let v = Variant::cab(16).unwrap();
    let n = 2 * lzx::FRAME + 777;
    let (_, src) = cab_stream(&mut rng, n, 16);
    let sp = recons::split_stream(&src, n, v).unwrap();
    for i in 0..200 {
        let mut r = sp.record.clone();
        match i % 3 {
            0 => r.truncate(rng.below(r.len() as u64) as usize),
            1 => {
                let at = rng.below(r.len() as u64) as usize;
                r[at] ^= 1 << rng.below(8);
            }
            _ => r = (0..r.len()).map(|_| rng.below(256) as u8).collect(),
        }
        let _ = recons::join_stream_onto(&sp.data, &r, v, &mut Vec::new());
    }
}

/// Writes `tests/data/greedy.cab-lzx`, a CAB stream of this test (a window of 64 KiB, e8) for
/// other crates' tests: the window's bits (u8), the data's length (u32 little-endian), then each
/// frame's bytes' length (u32) and its bytes.
#[test]
#[ignore]
fn write_cab_fixture() {
    let mut rng = Rng::new(8);
    let n = 3 * lzx::FRAME + 4321;
    let (_, src) = loop {
        let s = cab_stream(&mut rng, n, 16);
        if lzx::read_as(&s.1, n, Variant::cab(16).unwrap()).is_some_and(|(_, p)| p.e8.is_some()) {
            break s;
        }
    };
    let ends = lzx::frame_ends(&src, n, Variant::cab(16).unwrap()).unwrap();
    let mut out = vec![16];
    out.extend_from_slice(&(n as u32).to_le_bytes());
    let mut at = 0;
    for e in ends.into_iter().chain([src.len()]) {
        out.extend_from_slice(&((e - at) as u32).to_le_bytes());
        out.extend_from_slice(&src[at..e]);
        at = e;
    }
    std::fs::write(format!("{}/tests/data/greedy.cab-lzx", env!("CARGO_MANIFEST_DIR")), out).unwrap();
}

/// Writes `tests/data/greedy.chm-lzx`, CHM's LZX of this test for other crates' tests: a reset
/// every 2 frames, each interval a CAB stream of its own (a window of 64 KiB, no e8), the last of
/// whole frames past the data (zeros). The data's length (u32 little-endian), then each
/// interval's frames: their bytes' lengths (u32) and bytes.
#[test]
#[ignore]
fn write_chm_fixture() {
    let mut rng = Rng::new(10);
    let v = Variant::cab(16).unwrap();
    let n = 4 * lzx::FRAME + 9000;
    let mut file = data(&mut rng, n);
    file.resize(n.div_ceil(lzx::FRAME) * lzx::FRAME, 0);
    let mut out = (n as u32).to_le_bytes().to_vec();
    for d in file.chunks(2 * lzx::FRAME) {
        let src = encode_as(d, &[d.len() as isize], &mut rng, v, None);
        let ends = lzx::frame_ends(&src, d.len(), v).unwrap();
        let mut at = 0;
        for e in ends.into_iter().chain([src.len()]) {
            out.extend_from_slice(&((e - at) as u32).to_le_bytes());
            out.extend_from_slice(&src[at..e]);
            at = e;
        }
    }
    std::fs::write(format!("{}/tests/data/greedy.chm-lzx", env!("CARGO_MANIFEST_DIR")), out).unwrap();
}
