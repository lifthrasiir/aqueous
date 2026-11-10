//! A search for ALZip's x86 filter by the CRC-32 of the files alone (see FORMAT.md §2.6 for how
//! the candidates were chosen). Each x86-flagged AZO stream of the EGG archives given is decoded
//! to what its blocks hold (the filter's output, as the encoder made it); every variant of the
//! decoder's filter in the family below is applied and the CRC-32 compared with the file's.
//!
//! ```sh
//! cargo run --profile release-test -p aqr-azo --example aqr-azo-x86-search -- FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};

use aqr_azo::azo;

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

/// The AZO streams of an EGG archive's blocks of method 3: each with its CRC-32 and size.
fn streams(d: &[u8]) -> Vec<(&[u8], u32, usize)> {
    let sig = 0x02B5_0C13u32.to_le_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(k) = d[at..].windows(4).position(|w| w == sig) {
        let h = at + k + 4;
        at = h;
        let Some(hd) = d.get(h..h + 18) else { break };
        let le = |i: usize| u32::from_le_bytes(hd[i..i + 4].try_into().unwrap());
        if hd[0] != 3 || hd[14..18] != 0x08E2_8222u32.to_le_bytes() {
            continue;
        }
        let (size, csize, crc) = (le(2) as usize, le(6) as usize, le(10));
        if let Some(s) = d.get(h + 18..h + 18 + csize) {
            out.push((s, crc, size));
            at = h + 18 + csize;
        }
    }
    out
}

/// One variant of the decoder's filter.
#[derive(Clone, Copy, Debug)]
struct V {
    /// 0: E8; 1: E8, E9; 2: E8, E9, 0F 80..8F (the address after the two bytes).
    ops: u8,
    /// Which values of the address's most significant byte are converted: 0: 00 or FF; 1: 00;
    /// 2: FF; 3: any.
    cond: u8,
    /// The address is relative to the position of the opcode plus this.
    off: u32,
    /// Subtracting (the decoder) or adding.
    sub: bool,
    be: bool,
    /// 0: bit 24 extended; 1: all 32 bits; 2: 24 bits, the top byte as it was.
    fix: u8,
    /// After a conversion: 1: go on with the next byte; 5: past the address.
    skip: usize,
    /// 0: not in LZX's way; else the file size of LZX's test (1: the block's, 2: the stream's,
    /// 3: 12000000, 4: 2^24): `-pos <= abs < size` is converted, ignoring `cond` and `fix`.
    lzx: u8,
    /// With `lzx`, skip past the address even if nothing was converted.
    skip_all: bool,
    /// Positions go on from block to block.
    running: bool,
}

fn apply(v: V, blocks: &[&[u8]]) -> Vec<u8> {
    let total: usize = blocks.iter().map(|b| b.len()).sum();
    let mut out = Vec::new();
    let mut base = 0u32;
    for b in blocks {
        let mut d = b.to_vec();
        let n = d.len();
        let mut i = 0;
        while i + 4 < n {
            let (at, ok) = match d[i] {
                0xE8 => (i + 1, true),
                0xE9 => (i + 1, v.ops >= 1),
                0x0F if v.ops >= 2 && i + 5 < n && d[i + 1] & 0xF0 == 0x80 => (i + 2, true),
                _ => (i, false),
            };
            if !ok || at + 4 > n {
                i += 1;
                continue;
            }
            let w: [u8; 4] = d[at..at + 4].try_into().unwrap();
            let x = if v.be { u32::from_be_bytes(w) } else { u32::from_le_bytes(w) };
            let hi = (x >> 24) as u8;
            if v.lzx != 0 {
                let fs = match v.lzx {
                    1 => n as i64,
                    2 => total as i64,
                    3 => 12_000_000,
                    _ => 1 << 24,
                };
                let p = (if v.running { base } else { 0 }) as i64 + i as i64 + v.off as i64;
                let abs = x as i32 as i64;
                // the decoder of LZX: absolute to relative
                let (r, conv) = if abs >= -p && abs < fs {
                    (if abs >= 0 { abs - p } else { abs + fs }, true)
                } else {
                    (abs, false)
                };
                if conv {
                    d[at..at + 4].copy_from_slice(&(r as u32).to_le_bytes());
                }
                i = if conv || v.skip_all { at + 4 } else { i + 1 };
                continue;
            }
            let go = match v.cond {
                0 => hi == 0 || hi == 0xFF,
                1 => hi == 0,
                2 => hi == 0xFF,
                _ => true,
            };
            if !go {
                i = if v.skip_all { at + 4 } else { i + 1 };
                continue;
            }
            let p = (if v.running { base } else { 0 }).wrapping_add(i as u32).wrapping_add(v.off);
            let a = if v.sub { x.wrapping_sub(p) } else { x.wrapping_add(p) };
            let a = match v.fix {
                0 if a >> 24 & 1 != 0 => a | 0xFF00_0000,
                0 => a & 0x00FF_FFFF,
                1 => a,
                _ => a & 0x00FF_FFFF | x & 0xFF00_0000,
            };
            let w = if v.be { a.to_be_bytes() } else { a.to_le_bytes() };
            d[at..at + 4].copy_from_slice(&w);
            i = if v.skip == 5 { at + 4 } else { i + 1 };
        }
        base = base.wrapping_add(n as u32);
        out.extend_from_slice(&d);
    }
    out
}

fn main() {
    let mut paths = Vec::new();
    std::env::args().skip(1).for_each(|a| walk(Path::new(&a), &mut paths));
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        for (s, crc, size) in streams(&d) {
            let Ok(st) = azo::parse(s) else { continue };
            if !st.x86 {
                continue;
            }
            let raw: Vec<Vec<u8>> = st
                .blocks
                .iter()
                .map(|b| {
                    let c = &s[b.code.clone()];
                    if b.stored() {
                        c.to_vec()
                    } else {
                        azo::decode_block(c, b.size, &mut azo::Model::default()).unwrap().data
                    }
                })
                .collect();
            let blocks: Vec<&[u8]> = raw.iter().map(|b| &b[..]).collect();
            println!("{}: {size} B, {} blocks, crc {crc:08x}", p.display(), blocks.len());
            let ident: Vec<u8> = raw.concat();
            println!("  unfiltered: crc {:08x}", aqr_common::crc32::hash(&ident));
            let mut all = Vec::new();
            for ops in 0..3 {
                for cond in 0..4 {
                    for off in [0, 1, 4, 5] {
                        for sub in [true, false] {
                            for be in [false, true] {
                                for fix in 0..3 {
                                    for skip in [1, 5] {
                                        for running in [false, true] {
                                            for lzx in 0..5 {
                                                for skip_all in [false, true] {
                                                    all.push(V {
                                                        ops,
                                                        cond,
                                                        off,
                                                        sub,
                                                        be,
                                                        fix,
                                                        skip,
                                                        lzx,
                                                        skip_all,
                                                        running,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let tried = all.len();
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
            std::thread::scope(|sc| {
                for chunk in all.chunks(tried.div_ceil(threads)) {
                    let blocks = &blocks;
                    sc.spawn(move || {
                        for &v in chunk {
                            if aqr_common::crc32::hash(&apply(v, blocks)) == crc {
                                println!("  MATCH {v:?}");
                            }
                        }
                    });
                }
            });
            println!("  {tried} variants tried");
        }
    }
}
