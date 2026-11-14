//! Checks the emulation of libzstd against pairs of (content, frame): `zcheck LEVEL FILE...`,
//! each file of `u32` lengths (little endian) before a content, then its frame.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let level: i32 = args[0].parse().unwrap();
    let verbose = std::env::var_os("ZCHECK_V").is_some();
    for path in &args[1..] {
        let d = std::fs::read(path).unwrap();
        let (mut n, mut same, mut unsupported, mut bytes, mut shown) = (0, 0, 0, 0usize, 0);
        let mut first_diff = std::collections::BTreeMap::<&str, usize>::new();
        let mut t = 0f64;
        let mut p = 0;
        while p < d.len() {
            let len = u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            let content = &d[p + 4..p + 4 + len];
            p += 4 + len;
            let flen = u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            let frame = &d[p + 4..p + 4 + flen];
            p += 4 + flen;
            n += 1;
            let t0 = Instant::now();
            let made = aqr_zstd::compress::compress(content, level);
            t += t0.elapsed().as_secs_f64();
            bytes += len;
            match made {
                None => unsupported += 1,
                Some(m) if m == frame => same += 1,
                Some(m) => {
                    let at = m.iter().zip(frame).position(|(a, b)| a != b).unwrap_or(m.len().min(frame.len()));
                    let what = describe(frame, at);
                    *first_diff.entry(what).or_default() += 1;
                    if verbose && shown < 5 {
                        shown += 1;
                        eprintln!(
                            "#{n}: {len} B, frame {} made {}, first difference at {at} ({what})",
                            frame.len(),
                            m.len()
                        );
                        let dump = format!("{}/zc-{n}.bin", std::env::var("ZCHECK_DIR").unwrap_or(".".into()));
                        std::fs::write(&dump, content).ok();
                    }
                }
            }
        }
        println!(
            "{path}: {n} frames, {same} the same, {unsupported} not emulated, {} differ; {:.1} MB/s; {first_diff:?}",
            n - same - unsupported,
            bytes as f64 / t / 1e6
        );
    }
}

/// What part of the frame the byte at `at` is.
fn describe(f: &[u8], at: usize) -> &'static str {
    let fhd = f[4];
    let mut p =
        5 + if fhd & 0x20 == 0 { 1 } else { 0 } + [if fhd & 0x20 != 0 { 1 } else { 0 }, 2, 4, 8][(fhd >> 6) as usize];
    if at < p {
        return "header";
    }
    loop {
        let h = f[p] as u32 | (f[p + 1] as u32) << 8 | (f[p + 2] as u32) << 16;
        if at < p + 3 {
            return "block header";
        }
        let t = h >> 1 & 3;
        let size = (h >> 3) as usize;
        let body = if t == 1 { 1 } else { size };
        if at < p + 3 + body {
            if t != 2 {
                return "raw or rle block";
            }
            let b = &f[p + 3..];
            let lt = b[0] & 3;
            let sf = b[0] >> 2 & 3;
            let (lh, csize) = match (lt, sf) {
                (0 | 1, 0 | 2) => (1, if lt == 1 { 1 } else { (b[0] >> 3) as usize }),
                (0 | 1, 1) => (2, if lt == 1 { 1 } else { (u16::from_le_bytes([b[0], b[1]]) >> 4) as usize }),
                (0 | 1, _) => {
                    (3, if lt == 1 { 1 } else { (b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16) >> 4 })
                }
                (_, 0 | 1) => (3, ((b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16) >> 14) & 0x3ff),
                (_, 2) => (4, (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) >> 18) as usize & 0x3fff),
                _ => (
                    5,
                    ((u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64 | (b[4] as u64) << 32) >> 22) as usize
                        & 0x3ffff,
                ),
            };
            let off = at - (p + 3);
            return if off < lh {
                "literals header"
            } else if off < lh + csize {
                if lt >= 2 { "huffman literals" } else { "raw literals" }
            } else if off < lh + csize + 1 {
                "sequences count"
            } else {
                "sequences"
            };
        }
        p += 3 + body;
        if h & 1 != 0 {
            return "after the last block";
        }
    }
}
