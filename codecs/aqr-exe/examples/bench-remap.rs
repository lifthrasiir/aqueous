//! ARM64 code against another version of it (`arm64::remap`): of the ARM64 image of Mach-O
//! file B, its BL and ADRP targets written in the address space of that of file A. Each section
//! is verified to round-trip; B's CODE stream is compressed against A's (`zstd -7 --long=28
//! --patch-from`, the zstd CLI), as it is and remapped, and the flags and maps on their own
//! (zstd -19).
//!
//! With `-x`, the same of the x86-64 images (`x86::remap`), each of B's streams against A's.
//!
//! usage: aqr-exe-bench-remap [-x] A B

use std::process::Command;
use std::time::Instant;

use aqr_exe::arch::arm64::{self, remap};
use aqr_exe::arch::x86;
use aqr_exe::format::macho::{self, CPU_TYPE_ARM64, CPU_TYPE_X86_64};

fn sections(d: &[u8], cputype: u32) -> Vec<(u64, &[u8])> {
    let images = macho::images(d).expect("a Mach-O file");
    let i = images.iter().find(|i| i.cputype == cputype).expect("an image of the architecture");
    i.sections.iter().map(|&(va, off, size)| (va, &d[off..off + size])).collect()
}

fn zstd(d: &[u8], level: i32) -> usize {
    zstd::bulk::compress(d, level).unwrap().len()
}

fn patch(reference: &[u8], d: &[u8]) -> usize {
    if d.is_empty() || reference.is_empty() {
        return if d.is_empty() { 0 } else { zstd(d, 7) };
    }
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
    if let ["-x", a, b] = &args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        let (da, db) = (std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
        return x86_main(&sections(&da, CPU_TYPE_X86_64), &sections(&db, CPU_TYPE_X86_64));
    }
    let [a, b] = &args[..] else { panic!("usage: aqr-exe-bench-remap [-x] A B") };
    let (da, db) = (std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
    let (sa, sb) = (sections(&da, CPU_TYPE_ARM64), sections(&db, CPU_TYPE_ARM64));

    let alike = remap::signature(&sa).alike(&remap::signature(&sb));
    let t = Instant::now();
    let r = remap::Remap::new(&sa, &sb);
    let t_map = t.elapsed().as_secs_f64();
    let s = r.stats;
    println!(
        "alike {alike:.3}; functions A {} B {}, matched {} ({:.0}% of A's words); map BL {} ADRP {} ({t_map:.2} s)",
        s.functions[0],
        s.functions[1],
        s.matched,
        100.0 * s.matched_units as f64 / s.units[0] as f64,
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

fn x86_main(sa: &[(u64, &[u8])], sb: &[(u64, &[u8])]) {
    let alike = x86::remap::signature(sa, true).alike(&x86::remap::signature(sb, true));
    let t = Instant::now();
    let r = x86::remap::Remap::new(sa, sb, true);
    let t_map = t.elapsed().as_secs_f64();
    let s = r.stats;
    println!(
        "alike {alike:.3}; functions A {} B {}, matched {} ({:.0}% of A's bytes); map {} ({t_map:.2} s)",
        s.functions[0],
        s.functions[1],
        s.matched,
        100.0 * s.matched_units as f64 / s.units[0] as f64,
        s.mapped[0],
    );
    let n = x86::NUM_STREAMS;
    let cat = |secs: &[(u64, &[u8])]| -> Vec<Vec<u8>> {
        let mut out = vec![Vec::new(); n];
        for &(va, c) in secs {
            let e = x86::encode(c, va, true).unwrap();
            for (o, s) in out.iter_mut().zip(e.streams()) {
                o.extend_from_slice(s);
            }
        }
        out
    };
    let (a, plain) = (cat(sa), cat(sb));
    let (mut remapped, mut extra) = (vec![Vec::new(); n], vec![Vec::new(); 4]);
    let mut flagged = [(0, 0); 3];
    let (mut t_enc, mut t_dec) = (0.0, 0.0);
    for &(va, c) in sb {
        let t = Instant::now();
        let e = x86::remap::encode(c, va, true, &r).unwrap();
        t_enc += t.elapsed().as_secs_f64();
        let st: [&[u8]; x86::remap::NUM_STREAMS] =
            std::array::from_fn(|i| if i < n { &e.base.streams()[i][..] } else { &e.extra()[i - n][..] });
        let t = Instant::now();
        let back =
            x86::remap::decode(e.base.origin(), e.base.sentinel_size(), st, true, &x86::Opcodes::BUILTIN, c.len());
        t_dec += t.elapsed().as_secs_f64();
        assert_eq!(back.as_deref(), Some(c), "section at {va:#x}");
        for (o, s) in remapped.iter_mut().zip(e.base.streams()) {
            o.extend_from_slice(s);
        }
        for (o, s) in extra.iter_mut().zip(e.extra()) {
            o.extend_from_slice(s);
        }
        for (k, f) in e.flagged().into_iter().enumerate() {
            flagged[k].0 += f.0;
            flagged[k].1 += f.1;
        }
    }
    let bytes = sb.iter().map(|s| s.1.len()).sum::<usize>() as f64;
    let share = |(n, k): (usize, usize)| 100.0 * k as f64 / n.max(1) as f64;
    println!(
        "CALL {} flagged {:.1}%, other jumps {} flagged {:.1}%, addresses {} flagged {:.1}%; encode {:.0} MB/s, decode {:.0} MB/s",
        flagged[0].0,
        share(flagged[0]),
        flagged[1].0,
        share(flagged[1]),
        flagged[2].0,
        share(flagged[2]),
        bytes / t_enc / 1e6,
        bytes / t_dec / 1e6,
    );
    let (mut alone, mut before, mut after) = (0, 0, 0);
    for i in 0..n {
        let (x, y) = (patch(&a[i], &plain[i]), patch(&a[i], &remapped[i]));
        if plain[i].len() > 1 << 16 {
            println!("  {:12} {:7.2} -> {:7.2} MB", x86::STREAM_NAMES[i], mb(x), mb(y));
        }
        alone += zstd(&plain[i], 7);
        before += x;
        after += y;
    }
    let side: Vec<usize> = extra.iter().map(|e| zstd(e, 19)).collect();
    let total = after + side.iter().sum::<usize>();
    println!(
        "streams of B: alone {:.2} MB, against A {:.2} MB, remapped {:.2} + flags {:.2} + {:.2} + {:.2} + map {:.2} = {:.2} MB ({:+.1}%)",
        mb(alone),
        mb(before),
        mb(after),
        mb(side[0]),
        mb(side[1]),
        mb(side[2]),
        mb(side[3]),
        mb(total),
        100.0 * (total as f64 / before as f64 - 1.0),
    );
}
