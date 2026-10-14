//! Per zlib stream (at level 6) of the given packs: bytes of data, of
//! the zlib stream, tokens, matches, matched bytes, and the time zlib emulation takes to
//! write it (best of three), as CSV.
use std::time::Instant;

fn main() {
    println!("pack,level,size,zsize,tokens,matches,matched,chain,chain32,ns");
    for path in std::env::args().skip(1) {
        // A directory: its loose objects (objects/xx/*).
        if std::path::Path::new(&path).is_dir() {
            let name = path.split('/').nth(2).unwrap_or("?");
            for d in std::fs::read_dir(&path).unwrap() {
                let d = d.unwrap().path();
                if d.file_name().unwrap().len() != 2 {
                    continue;
                }
                for f in std::fs::read_dir(&d).unwrap() {
                    let z = std::fs::read(f.unwrap().path()).unwrap();
                    let (data, _) = git_recons::obj::inflate(&z, 1 << 16).unwrap();
                    measure(name, &data, &z);
                }
            }
            continue;
        }
        let pack = std::fs::read(&path).unwrap();
        let mut p = 12;
        while p < pack.len() - 20 {
            let (t, size, mut q) = git_recons::obj::entry_header_any(&pack[p..]).unwrap();
            q += p;
            if t == 6 {
                q += git_recons::obj::parse_ofs(&pack[q..]).unwrap().1;
            } else if t == 7 {
                q += 20;
            }
            let (data, used) = git_recons::obj::inflate(&pack[q..], size as usize).unwrap();
            let z = &pack[q..q + used];
            p = q + used;
            let name = path.split('/').nth(2).unwrap_or("?");
            measure(name, &data, z);
        }
    }
}

/// Times the emulation of `z`, the zlib stream of `data`, at its level (if zlib's).
fn measure(name: &str, data: &[u8], z: &[u8]) {
    let used = z.len();
    let Some(level) = git_recons::zl::find_level(z, data, 6) else { return };
    let f = flate_recons::inflate::parse(&z[2..used - 4]).unwrap();
    let (mut matches, mut matched) = (0, 0);
    for t in &f.tokens {
        if let flate_recons::deflate::Token::Match { len, .. } = t {
            matches += 1;
            matched += *len as usize;
        }
    }
    let (chain, chain32) = occupancy(data);
    let mut best = f64::MAX;
    let mut same = true;
    for _ in 0..3 {
        let t = Instant::now();
        let mut out = vec![];
        git_recons::zl::zlib(data, level, &mut out);
        best = best.min(t.elapsed().as_secs_f64());
        same &= out == z;
    }
    assert!(same);
    println!(
        "{name},{level},{},{},{},{matches},{matched},{chain},{chain32},{:.0}",
        data.len(),
        used,
        f.tokens.len(),
        best * 1e9
    );
}

/// Over every position, how many earlier positions within zlib's window share its hash
/// (zlib's, memLevel 8), capped at 128 and at 32: about how far longest_match walks.
fn occupancy(d: &[u8]) -> (u64, u64) {
    const W: usize = 32768 - 262;
    let mut count = vec![0u32; 1 << 15];
    let h = |p: usize| ((d[p] as usize) << 10 ^ (d[p + 1] as usize) << 5 ^ d[p + 2] as usize) & 0x7fff;
    let (mut s, mut s32) = (0u64, 0u64);
    for p in 0..d.len().saturating_sub(2) {
        if p >= W {
            count[h(p - W)] -= 1;
        }
        let c = &mut count[h(p)];
        s += (*c).min(128) as u64;
        s32 += (*c).min(32) as u64;
        *c += 1;
    }
    (s, s32)
}
