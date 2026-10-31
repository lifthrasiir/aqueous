//! The AZO streams of EGG archives (their blocks of method 3, found by their headers): decoded,
//! checked against the blocks' CRC-32, and encoded again from their parses. Only totals are
//! printed (`-v` adds a line per stream, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-azo --example aqr-azo-check -- [-v] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};

use aqr_azo::{azo, recons};
use std::time::Instant;

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

fn main() {
    let (mut paths, mut verbose) = (Vec::new(), false);
    for a in std::env::args().skip(1) {
        match &a[..] {
            "-v" => verbose = true,
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let (mut n, mut decoded, mut crc_ok, mut rebuilt) = (0, 0, 0, 0);
    let (mut blocks, mut blocks_same, mut bytes) = (0, 0, 0);
    // streams split and joined again; their bytes, data and records; the times
    let (mut joined, mut sp_bytes, mut sp_data, mut sp_record) = (0, 0, 0, 0);
    let (mut t_split, mut t_join) = (0.0, 0.0);
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        for (s, crc, size) in streams(&d) {
            let i = n;
            n += 1;
            bytes += s.len();
            let dec = match azo::decode(s, usize::MAX) {
                Ok(x) => x,
                Err(e) => {
                    println!("{i}: {} B, not decoded: {e}", s.len());
                    continue;
                }
            };
            decoded += 1;
            let good = dec.data.len() == size && aqr_common::crc32::hash(&dec.data) == crc;
            crc_ok += good as usize;
            // each compressed block encoded again from its data before the filter and tokens
            let mut same = true;
            let mut at = 0;
            for (b, t) in dec.stream.blocks.iter().zip(&dec.tokens) {
                let mut x = dec.data[at..at + b.size].to_vec();
                at += b.size;
                let Some(t) = t else { continue };
                if dec.stream.x86 {
                    azo::filter(&mut x);
                }
                blocks += 1;
                let code = azo::encode_block(&x, t, &mut azo::Model::default());
                let orig = &s[b.code.clone()];
                match code {
                    Ok(c) if c == orig => blocks_same += 1,
                    Ok(c) => {
                        same = false;
                        let k = c.iter().zip(orig).position(|(a, b)| a != b).unwrap_or(c.len().min(orig.len()));
                        if verbose {
                            println!(
                                "{i}: block of {} B: code {} B, again {} B, differ from {k} ({:02x?} vs {:02x?})",
                                b.size,
                                orig.len(),
                                c.len(),
                                &orig[k.saturating_sub(2)..(k + 3).min(orig.len())],
                                &c[k.saturating_sub(2)..(k + 3).min(c.len())]
                            );
                        }
                    }
                    Err(e) => {
                        same = false;
                        println!("{i}: not encoded: {e}");
                    }
                }
            }
            rebuilt += (good && same) as usize;
            let t = Instant::now();
            let sp = recons::split(s, usize::MAX, 1);
            t_split += t.elapsed().as_secs_f64();
            match sp {
                Ok(sp) => {
                    let t = Instant::now();
                    let j = recons::join(&sp.data, &sp.record, 1);
                    t_join += t.elapsed().as_secs_f64();
                    if j.as_deref().ok() == Some(&s[..sp.used]) {
                        joined += 1;
                        (sp_bytes, sp_data, sp_record) =
                            (sp_bytes + sp.used, sp_data + sp.data.len(), sp_record + sp.record.len());
                        if verbose {
                            println!("{i}: split, record {} B of {} B", sp.record.len(), sp.used);
                        }
                    } else {
                        println!("{i}: JOINED OTHERWISE {:?}", j.err());
                    }
                }
                Err(e) => println!("{i}: not split: {e}"),
            }
            if verbose {
                println!(
                    "{i}: {} B -> {} B, crc {}, x86 {}, blocks {}",
                    s.len(),
                    size,
                    good,
                    dec.stream.x86,
                    dec.stream.blocks.len()
                );
            }
        }
    }
    println!("{n} streams ({bytes} B), {decoded} decoded, {crc_ok} of the right CRC-32, {rebuilt} rebuilt");
    println!("{blocks} compressed blocks, {blocks_same} encoded again the same");
    println!(
        "{joined} split and joined: {sp_bytes} B, data {sp_data} B, records {sp_record} B ({:.2}%); split {t_split:.2} s, join {t_join:.2} s",
        100.0 * sp_record as f64 / sp_bytes.max(1) as f64
    );
}
