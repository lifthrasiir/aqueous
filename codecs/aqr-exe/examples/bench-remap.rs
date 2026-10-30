//! ARM64 code against another version of it (`arm64::remap`): of the ARM64 image of Mach-O
//! file B, its BL and ADRP targets written in the address space of that of file A. Each section
//! is verified to round-trip; B's CODE stream is compressed against A's (`zstd -7 --long=28
//! --patch-from`, the zstd CLI), as it is and remapped, and the flags and maps on their own
//! (zstd -19).
//!
//! usage: aqr-exe-bench-remap A B

use std::process::Command;
use std::time::Instant;

use aqr_exe::arch::arm64::{self, remap};
use aqr_exe::format::macho::{self, CPU_TYPE_ARM64};

fn sections(d: &[u8]) -> Vec<(u64, &[u8])> {
    let images = macho::images(d).expect("a Mach-O file");
    let i = images.iter().find(|i| i.cputype == CPU_TYPE_ARM64).expect("an ARM64 image");
    i.sections.iter().map(|&(va, off, size)| (va, &d[off..off + size])).collect()
}

fn zstd(d: &[u8], level: i32) -> usize {
    zstd::bulk::compress(d, level).unwrap().len()
}

fn patch(reference: &[u8], d: &[u8]) -> usize {
    let dir = std::env::temp_dir().join(format!("aqr-exe-bench-remap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (r, p) = (dir.join("ref"), dir.join("data"));
    std::fs::write(&r, reference).unwrap();
    std::fs::write(&p, d).unwrap();
    let out = Command::new("zstd")
        .args(["-7", "--long=28", "-T0", "-q", "-c"])
        .arg(format!("--patch-from={}", r.display()))
        .arg(&p)
        .output()
        .unwrap();
    assert!(out.status.success());
    std::fs::remove_dir_all(&dir).unwrap();
    out.stdout.len()
}

fn mb(n: usize) -> f64 {
    n as f64 / 1e6
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [a, b] = &args[..] else { panic!("usage: aqr-exe-bench-remap A B") };
    let (da, db) = (std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
    let (sa, sb) = (sections(&da), sections(&db));

    let alike = remap::Signature::of(&sa).alike(&remap::Signature::of(&sb));
    let t = Instant::now();
    let r = remap::Remap::new(&sa, &sb);
    let t_map = t.elapsed().as_secs_f64();
    let s = r.stats;
    println!(
        "alike {alike:.3}; functions A {} B {}, matched {} ({:.0}% of A's words); map BL {} ADRP {} ({t_map:.2} s)",
        s.functions[0],
        s.functions[1],
        s.matched,
        100.0 * s.matched_words as f64 / s.words[0] as f64,
        s.mapped[0],
        s.mapped[1],
    );

    let code_a: Vec<u8> = sa.iter().flat_map(|&(va, c)| arm64::encode(c, va).streams()[0].clone()).collect();
    let (mut plain, mut remapped, mut side) = (Vec::new(), Vec::new(), Vec::new());
    let mut flagged = [(0, 0); 2];
    let (mut t_enc, mut t_dec) = (0.0, 0.0);
    for &(va, c) in &sb {
        plain.extend_from_slice(&arm64::encode(c, va).streams()[0]);
        let t = Instant::now();
        let e = remap::encode(c, va, &r);
        t_enc += t.elapsed().as_secs_f64();
        let st: [&[u8]; remap::NUM_STREAMS] = std::array::from_fn(|i| &e.streams()[i][..]);
        let t = Instant::now();
        assert_eq!(remap::decode(st, va, c.len()).as_deref(), Some(c), "section at {va:#x}");
        t_dec += t.elapsed().as_secs_f64();
        remapped.extend_from_slice(&e.streams()[0]);
        for (k, f) in e.flagged().into_iter().enumerate() {
            flagged[k].0 += f.0;
            flagged[k].1 += f.1;
        }
        side.push([remap::BLF, remap::ADRPF, remap::BLMAP, remap::ADRPMAP].map(|i| e.streams()[i].clone()));
    }
    let side_z: [usize; 4] =
        std::array::from_fn(|i| zstd(&side.iter().flat_map(|s| s[i].clone()).collect::<Vec<_>>(), 19));
    let n = sb.iter().map(|s| s.1.len()).sum::<usize>() as f64;
    println!(
        "BL {} flagged {:.1}%, ADRP {} flagged {:.1}%; encode {:.0} MB/s, decode {:.0} MB/s",
        flagged[0].0,
        100.0 * flagged[0].1 as f64 / flagged[0].0 as f64,
        flagged[1].0,
        100.0 * flagged[1].1 as f64 / flagged[1].0 as f64,
        n / t_enc / 1e6,
        n / t_dec / 1e6,
    );
    let (alone, before, after) = (zstd(&plain, 7), patch(&code_a, &plain), patch(&code_a, &remapped));
    let extra: usize = side_z.iter().sum();
    println!(
        "CODE of B: alone {:.2} MB, against A {:.2} MB, remapped {:.2} + flags {:.2} + {:.2} + maps {:.2} + {:.2} = {:.2} MB ({:+.1}%)",
        mb(alone),
        mb(before),
        mb(after),
        mb(side_z[0]),
        mb(side_z[1]),
        mb(side_z[2]),
        mb(side_z[3]),
        mb(after + extra),
        100.0 * ((after + extra) as f64 / before as f64 - 1.0),
    );
}
