//! LHA archives: their entries by method; the streams of `-lh4-` to `-lh7-` decoded, checked
//! against the headers' CRC-16, encoded again from their parses. Totals are printed (`-v` adds a
//! line per stream).
//!
//! ```sh
//! cargo run --release -p aqr-lha --example aqr-lha-check -- [-v] FILE_OR_DIR...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aqr_lha::{huff, lha, lzh, recons};
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

/// The archive in `d`: at its start, or after a self-extracting program (the first header found).
fn find(d: &[u8]) -> Option<(usize, lha::Lha)> {
    if let Some(a) = lha::parse(d) {
        return Some((0, a));
    }
    let mut at = 0;
    while let Some(k) = d.get(at + 2..)?.windows(3).position(|w| w == b"-lh" || w == b"-lz" || w == b"-pm") {
        let h = at + k;
        if let Some(a) = lha::parse(&d[h..]) {
            return Some((h, a));
        }
        at = h + 1;
    }
    None
}

fn main() {
    let (mut paths, mut verbose) = (Vec::new(), false);
    for a in std::env::args().skip(1) {
        match &a[..] {
            "-v" => verbose = true,
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let mut methods: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // by the directory: blocks, heads predicted, c lengths, p lengths, t lengths alike
    let mut by_dir: BTreeMap<String, [usize; 5]> = BTreeMap::new();
    let (mut archives, mut streams, mut decoded, mut crc_ok, mut same) = (0, 0, 0, 0, 0);
    let (mut bytes, mut data) = (0, 0);
    // streams split and joined again; their bytes and records; the times
    let (mut joined, mut sp_bytes, mut sp_record, mut t_split, mut t_join) = (0, 0, 0, 0.0, 0.0);
    let mut cost = [0.0; recons::CATS];
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some((off, a)) = find(&d) else {
            if verbose {
                println!("{}: no archive", p.display());
            }
            continue;
        };
        archives += 1;
        let d = &d[off..];
        for e in &a.entries {
            let m = String::from_utf8_lossy(&e.method).into_owned();
            let c = methods.entry(m.clone()).or_default();
            c.0 += 1;
            c.1 += e.data.len();
            let Some(method) = lzh::Method::of(&e.method) else { continue };
            if e.size > 1 << 28 {
                println!("{}: a stream of {} B skipped", p.display(), e.size);
                continue;
            }
            streams += 1;
            bytes += e.data.len();
            let s = &d[e.data.clone()];
            let name = String::from_utf8_lossy(&d[e.name.clone()]).into_owned();
            let st = match lzh::decode(s, e.size as usize, method) {
                Ok(st) => st,
                Err(err) => {
                    println!("{} {name} {m} L{}: not decoded: {err}", p.display(), e.level);
                    continue;
                }
            };
            decoded += 1;
            let dir =
                p.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let bd = by_dir.entry(dir).or_default();
            let mut at = 0;
            let mut sizes = Vec::new();
            for h in &st.heads {
                let toks = &st.tokens[at..at + h.size as usize];
                at += h.size as usize;
                sizes.push(h.size);
                let g = huff::head(toks, method);
                bd[0] += 1;
                bd[1] += (g == *h) as usize;
                bd[2] += (lzh::c_lens(&g.c).ok() == lzh::c_lens(&h.c).ok() && g.c.single == h.c.single) as usize;
                bd[3] += (lzh::pt_lens(&g.p, method.np()).ok() == lzh::pt_lens(&h.p, method.np()).ok()
                    && g.p.single == h.p.single) as usize;
                bd[4] +=
                    (lzh::pt_lens(&g.t, 19).ok() == lzh::pt_lens(&h.t, 19).ok() && g.t.single == h.t.single) as usize;
                if verbose && g != *h {
                    println!("   head {:?}\n   pred {:?}", h, g);
                }
            }
            if verbose {
                println!("  sizes {sizes:?}");
            }
            data += st.data.len();
            let good = lha::crc16(&st.data) == e.crc;
            crc_ok += good as usize;
            let enc = lzh::encode(&st, method);
            let eq = enc.as_deref() == Ok(s);
            same += eq as usize;
            let t0 = Instant::now();
            let sp = recons::split(s, e.size as usize, method);
            t_split += t0.elapsed().as_secs_f64();
            let rlen = match &sp {
                Ok(sp) => {
                    let t0 = Instant::now();
                    let j = recons::join(&sp.data, &sp.record);
                    t_join += t0.elapsed().as_secs_f64();
                    if j.as_deref() == Ok(s) {
                        joined += 1;
                        sp_bytes += s.len();
                        sp_record += sp.record.len();
                        cost.iter_mut().zip(sp.cost).for_each(|(a, b)| *a += b / 8.0);
                        format!("record {} B", sp.record.len())
                    } else {
                        format!("NOT JOINED: {:?}", j.err())
                    }
                }
                Err(e) => format!("not split: {e}"),
            };
            if verbose || !eq || !good || !rlen.starts_with("record") {
                println!("  {rlen}");
                println!(
                    "{} {name} {m} L{}: {} -> {} B, {} blocks, crc {}, {}, pad {}/{} tail {}",
                    p.display(),
                    e.level,
                    s.len(),
                    st.data.len(),
                    st.heads.len(),
                    if good { "ok" } else { "BAD" },
                    match &enc {
                        Ok(x) if x == s => "same".to_string(),
                        Ok(_) => "OTHER".to_string(),
                        Err(e) => format!("not encoded: {e}"),
                    },
                    st.pad,
                    st.pad_bits,
                    st.tail.len()
                );
            }
        }
    }
    println!("by directory: blocks, heads predicted, c, p, t lengths predicted");
    for (k, v) in &by_dir {
        println!("  {k} {v:?}");
    }
    println!("{archives} archives; methods (entries, bytes):");
    for (m, (n, b)) in &methods {
        println!("  {m} {n} {b}");
    }
    println!(
        "{streams} streams ({bytes} B): {decoded} decoded ({data} B), {crc_ok} CRC-16 right, {same} encoded again"
    );
    println!(
        "{joined} split and joined ({sp_bytes} B): records {sp_record} B ({:.3}%); split {t_split:.2} s, join {t_join:.2} s",
        100.0 * sp_record as f64 / sp_bytes.max(1) as f64
    );
    println!(
        "  bytes: start {:.0}, pair {:.0}, len {:.0}, escape {:.0}, blocks {:.0}",
        cost[0], cost[1], cost[2], cost[3], cost[4]
    );
}
