//! ARM64 filter benchmark over the code sections of Mach-O files: each section is filtered
//! and verified to round-trip; the code is compressed with zstd as it is, after xz's ARM64
//! BCJ filter (BL and ADRP made absolute), and after the filter (each stream on its own),
//! file by file and all together.
//!
//! usage: aqr-exe-bench-arm64 [-l LEVEL] FILE...

use std::time::Instant;

use aqr_exe::arch::arm64::{self, NUM_STREAMS, STREAM_NAMES};
use aqr_exe::format::macho::{self, CPU_TYPE_ARM64};

fn zstd(d: &[u8], level: i32) -> usize {
    if d.is_empty() {
        return 0;
    }
    let mut c = zstd::bulk::Compressor::new(level).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(8)).unwrap();
    c.compress(d).unwrap().len()
}

/// xz's ARM64 BCJ: BL and ADRP (within +-512 MiB in xz; all here) with absolute targets.
fn bcj(code: &[u8], origin: u64) -> Vec<u8> {
    let mut out = code.to_vec();
    for (k, w) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let pc = origin.wrapping_add(4 * k as u64);
        let mut x = u32::from_le_bytes(*w);
        if x >> 26 == 0x25 {
            x = 0x9400_0000 | (x.wrapping_add((pc >> 2) as u32) & 0x03ff_ffff);
        } else if x & 0x9f00_0000 == 0x9000_0000 {
            let imm = (x >> 29 & 3) | (x >> 3 & 0x001f_fffc);
            let a = imm.wrapping_add((pc >> 12) as u32) & 0x1f_ffff;
            x = (x & 0x9f00_001f) | (a & 3) << 29 | (a >> 2) << 5;
        }
        w.copy_from_slice(&x.to_le_bytes());
    }
    out
}

fn main() {
    let mut level = 9;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match &*a {
            "-l" => level = args.next().unwrap().parse().unwrap(),
            _ => paths.push(a),
        }
    }
    let (mut code_all, mut bcj_all) = (Vec::new(), Vec::new());
    let mut streams: [Vec<u8>; NUM_STREAMS] = std::array::from_fn(|_| Vec::new());
    let (mut enc, mut dec) = (0.0, 0.0);
    for p in &paths {
        let d = std::fs::read(p).unwrap();
        let Some(images) = macho::images(&d) else { continue };
        let (mut raw, mut z, mut zb, mut zf) = (0, 0, 0, 0);
        for i in images.iter().filter(|i| i.cputype == CPU_TYPE_ARM64) {
            for &(va, off, size) in &i.sections {
                let code = &d[off..off + size];
                let t = Instant::now();
                let e = arm64::encode(code, va);
                enc += t.elapsed().as_secs_f64();
                let s: [&[u8]; NUM_STREAMS] = std::array::from_fn(|k| &e.streams()[k][..]);
                let t = Instant::now();
                let back = arm64::decode(s, va, code.len());
                dec += t.elapsed().as_secs_f64();
                assert!(back.as_deref() == Some(code), "{p}: section at {va:#x} does not round-trip");
                let b = bcj(code, va);
                raw += size;
                z += zstd(code, level);
                zb += zstd(&b, level);
                zf += e.streams().iter().map(|s| zstd(s, level)).sum::<usize>();
                code_all.extend_from_slice(code);
                bcj_all.extend_from_slice(&b);
                for (a, s) in streams.iter_mut().zip(e.streams()) {
                    a.extend_from_slice(s);
                }
            }
        }
        if raw > 0 {
            let name = p.rsplit('/').next().unwrap();
            println!(
                "{name:30} {raw:>11}  zstd {z:>10}  bcj {zb:>10} ({:+.1}%)  filter {zf:>10} ({:+.1}%)",
                100.0 * (zb as f64 / z as f64 - 1.0),
                100.0 * (zf as f64 / z as f64 - 1.0)
            );
        }
    }
    let n = code_all.len();
    let (z, zb) = (zstd(&code_all, level), zstd(&bcj_all, level));
    let zs: Vec<usize> = streams.iter().map(|s| zstd(s, level)).collect();
    let zf: usize = zs.iter().sum();
    println!(
        "all {n} bytes of code: zstd -{level} {z}, bcj {zb} ({:+.1}%), filter {zf} ({:+.1}%); filter {:.0} MB/s, unfilter {:.0} MB/s",
        100.0 * (zb as f64 / z as f64 - 1.0),
        100.0 * (zf as f64 / z as f64 - 1.0),
        n as f64 / enc / 1e6,
        n as f64 / dec / 1e6
    );
    for ((name, s), z) in STREAM_NAMES.iter().zip(&streams).zip(&zs) {
        println!("  {name:8} {:>11} -> {z:>10}", s.len());
    }
}
