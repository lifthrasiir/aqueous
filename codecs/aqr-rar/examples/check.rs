//! RAR archives: their streams decoded, checked against the headers' CRC-32, encoded again from
//! their tokens. Totals are printed (`-v` adds a line per stream).
//!
//! ```sh
//! cargo run --release -p aqr-rar --example aqr-rar-check -- [-v] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_common::crc32;
use aqr_rar::{huff, lz3, lz5, rar4, rar5, rec, recons3, recons5};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

#[derive(Default)]
struct Totals {
    archives: usize,
    groups: usize,
    files: usize,
    failed: usize,
    crc_bad: usize,
    same: usize,
    packed: usize,
    data: usize,
    /// Tables, and those Huffman's lengths give, by code (the four, the length code).
    tables: usize,
    alike: [usize; 5],
    /// Groups split and joined again; their packed bytes and records; the times; the costs.
    joined: usize,
    sp_packed: usize,
    sp_record: usize,
    t_split: f64,
    t_join: f64,
    cost: [f64; rec::CATS],
}

/// The tables of a group's streams against Huffman's lengths of the counts of the blocks they
/// serve (up to the next tables, across files).
fn tables(sts: &[lz5::Stream], v1: bool, t: &mut Totals) {
    // (tables, the tokens they serve)
    let mut spans: Vec<(&lz5::Tables, Vec<lz5::Token>)> = Vec::new();
    for st in sts {
        let mut at = 0;
        for b in &st.blocks {
            let toks = &st.tokens[at..at + b.tokens];
            at += b.tokens;
            match &b.tables {
                Some(tb) => spans.push((tb, toks.to_vec())),
                None => spans.last_mut().unwrap().1.extend_from_slice(toks),
            }
        }
    }
    for (tb, toks) in spans {
        t.tables += 1;
        let c = lz5::counts(&toks, v1);
        let mut pre = vec![0u32; lz5::PRE];
        lz5::rle(&tb.lens).iter().for_each(|r| pre[r.0 as usize] += 1);
        let mut parts = Vec::new();
        let mut k = 0;
        for f in &c {
            parts.push((&tb.lens[k..k + f.len()], f.clone()));
            k += f.len();
        }
        parts.push((&tb.pre[..], pre));
        for (i, (lens, f)) in parts.iter().enumerate() {
            t.alike[i] += (huff::lengths(f, huff::MAX) == *lens) as usize;
        }
    }
}

fn main() {
    let (mut paths, mut verbose) = (Vec::new(), false);
    for a in std::env::args().skip(1) {
        match &a[..] {
            "-v" => verbose = true,
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let mut t = Totals::default();
    let start = Instant::now();
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        if let Some(a) = rar4::parse(&d) {
            t.archives += 1;
            let mut groups: Vec<Vec<&rar4::Entry>> = Vec::new();
            for e in a.entries.iter().filter(|e| !e.dir && e.method != 0x30 && !e.data.is_empty()) {
                match groups.last_mut() {
                    Some(g) if e.solid && !e.service => g.push(e),
                    _ => groups.push(vec![e]),
                }
            }
            for g in groups {
                t.groups += 1;
                let name = String::from_utf8_lossy(&d[g[0].name.clone()]).into_owned();
                let why = check4(&d, &g, &mut t);
                if verbose || why.is_some() {
                    println!("{}: {} ({} files): {}", p.display(), name, g.len(), why.unwrap_or_else(|| "ok".into()));
                }
            }
            continue;
        }
        let Some(a) = rar5::parse(&d) else {
            if verbose {
                println!("{}: no RAR archive", p.display());
            }
            continue;
        };
        t.archives += 1;
        // the solid groups: a compressed file and those continuing it
        let mut groups: Vec<Vec<&rar5::Entry>> = Vec::new();
        for e in a.entries.iter().filter(|e| !e.service && !e.dir && e.method != 0 && !e.data.is_empty()) {
            match groups.last_mut() {
                Some(g) if e.solid => g.push(e),
                _ => groups.push(vec![e]),
            }
        }
        for g in groups {
            t.groups += 1;
            let name = String::from_utf8_lossy(&d[g[0].name.clone()]).into_owned();
            let why = check(&d, &g, &mut t);
            if verbose || why.is_some() {
                println!("{}: {} ({} files): {}", p.display(), name, g.len(), why.unwrap_or_else(|| "ok".into()));
            }
        }
    }
    println!(
        "{} archives, {} streams of {} files: {} rebuilt, {} failed, {} CRC mismatches; {} B packed, {} B data; {:.2} s",
        t.archives,
        t.groups,
        t.files,
        t.same,
        t.failed,
        t.crc_bad,
        t.packed,
        t.data,
        start.elapsed().as_secs_f64()
    );
    println!(
        "split and joined {} groups: records {} B of {} B packed ({:.3}%), split {:.2} s, join {:.2} s",
        t.joined,
        t.sp_record,
        t.sp_packed,
        100.0 * t.sp_record as f64 / t.sp_packed.max(1) as f64,
        t.t_split,
        t.t_join
    );
    let names = ["head", "start", "pair", "len", "escape", "form", "blocks"];
    let total: f64 = t.cost.iter().sum();
    println!(
        "  bits: {}",
        names
            .iter()
            .zip(t.cost)
            .map(|(n, c)| format!("{n} {:.1}%", 100.0 * c / total.max(1.0)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("of {} tables, Huffman's lengths (main, dist, low, rep, pre): {:?}", t.tables, t.alike);
}

fn check(d: &[u8], g: &[&rar5::Entry], t: &mut Totals) -> Option<String> {
    t.files += g.len();
    let e0 = g[0];
    if g.iter().any(|e| e.split || e.encrypted || e.size.is_none() || e.version > 1) {
        t.failed += 1;
        return Some("split, encrypted or of unknown size".into());
    }
    let mut dec = lz5::Decoder5::new(e0.version == 1, e0.dict);
    let mut enc = lz5::Encoder5::new(e0.version == 1);
    let mut again = Vec::new();
    let mut sts = Vec::new();
    for e in g {
        let packed = &d[e.data.clone()];
        let at = dec.out.len();
        let last = dec.last();
        let st = match dec.file(packed, e.size.unwrap() as usize) {
            Ok(st) => st,
            Err(err) => {
                t.failed += 1;
                return Some(format!("decoding: {err}"));
            }
        };
        let mut file = dec.out[at..].to_vec();
        if let Err(err) = lz5::unfilter(&mut file, &st.placed(last)) {
            t.failed += 1;
            return Some(format!("filters: {err}"));
        }
        if e.crc.is_some_and(|c| c != crc32::hash(&file)) {
            t.crc_bad += 1;
            return Some("CRC mismatch".into());
        }
        again.clear();
        if let Err(err) = enc.file(&st, &mut again) {
            t.failed += 1;
            return Some(format!("encoding: {err}"));
        }
        if again != packed {
            t.failed += 1;
            let at = again.iter().zip(packed).position(|(a, b)| a != b).unwrap_or(again.len().min(packed.len()));
            return Some(format!("encoded otherwise at {at} of {} ({} B again)", packed.len(), again.len()));
        }
        t.packed += packed.len();
        t.data += file.len();
        sts.push(st);
    }
    t.same += 1;
    tables(&sts, e0.version == 1, t);
    let parts: Vec<&[u8]> = g.iter().map(|e| &d[e.data.clone()]).collect();
    let sizes: Vec<usize> = g.iter().map(|e| e.size.unwrap() as usize).collect();
    let t0 = Instant::now();
    let sp = match recons5::split(&parts, &sizes, e0.version == 1, e0.dict) {
        Ok(sp) => sp,
        Err(err) => return Some(format!("split: {err}")),
    };
    let t1 = Instant::now();
    let joined = recons5::join(&sp.data, &sp.record);
    let t2 = Instant::now();
    measure(t, &parts, sp, joined, t0, t1, t2)
}

/// A split and join measured: the joined stream compared with the packed data.
fn measure(
    t: &mut Totals,
    parts: &[&[u8]],
    sp: recons5::Split,
    joined: Result<Vec<u8>, aqr_rar::Error>,
    t0: Instant,
    t1: Instant,
    t2: Instant,
) -> Option<String> {
    let whole: Vec<u8> = parts.concat();
    match joined {
        Ok(j) if j == whole => {}
        Ok(_) => return Some("joined otherwise".into()),
        Err(err) => return Some(format!("join: {err}")),
    }
    t.joined += 1;
    t.sp_packed += whole.len();
    t.sp_record += sp.record.len();
    t.t_split += (t1 - t0).as_secs_f64();
    t.t_join += (t2 - t1).as_secs_f64();
    for (a, b) in t.cost.iter_mut().zip(sp.cost) {
        *a += b;
    }
    if std::env::var_os("REC").is_some() {
        println!(
            "  record {} B of {} B packed ({:.3}%)",
            sp.record.len(),
            whole.len(),
            100.0 * sp.record.len() as f64 / whole.len() as f64
        );
    }
    None
}

fn check4(d: &[u8], g: &[&rar4::Entry], t: &mut Totals) -> Option<String> {
    t.files += g.len();
    if g.iter().any(|e| e.split || e.encrypted || e.version != 29) {
        t.failed += 1;
        return Some(format!("split, encrypted or of version {}", g[0].version));
    }
    let mut dec = lz3::Decoder3::new(g[0].dict);
    let mut enc = lz3::Encoder3::new();
    let mut header = true;
    let mut again = Vec::new();
    for e in g {
        let packed = &d[e.data.clone()];
        let at = dec.out.len();
        let st = match dec.file(packed, e.size as usize) {
            Ok(st) => st,
            Err(err) => {
                t.failed += 1;
                return Some(format!("decoding: {err}"));
            }
        };
        let mut file = dec.out[at..].to_vec();
        if let Err(err) = lz3::unfilter(&mut file, &st.filters) {
            t.failed += 1;
            return Some(format!("filters: {err}"));
        }
        if e.crc != crc32::hash(&file) {
            t.crc_bad += 1;
            return Some("CRC mismatch".into());
        }
        again.clear();
        if let Err(err) = enc.file(&st, header, &mut again) {
            t.failed += 1;
            return Some(format!("encoding: {err}"));
        }
        header = st.next_header;
        tables3(&st, t);
        if again != packed {
            t.failed += 1;
            let at = again.iter().zip(packed).position(|(a, b)| a != b).unwrap_or(again.len().min(packed.len()));
            return Some(format!("encoded otherwise at {at} of {} ({} B again)", packed.len(), again.len()));
        }
        t.packed += packed.len();
        t.data += file.len();
    }
    t.same += 1;
    let parts: Vec<&[u8]> = g.iter().map(|e| &d[e.data.clone()]).collect();
    let sizes: Vec<usize> = g.iter().map(|e| e.size as usize).collect();
    let t0 = Instant::now();
    let sp = match recons3::split(&parts, &sizes, g[0].dict) {
        Ok(sp) => sp,
        Err(err) => return Some(format!("split: {err}")),
    };
    let t1 = Instant::now();
    let joined = recons3::join(&sp.data, &sp.record);
    let t2 = Instant::now();
    measure(t, &parts, sp, joined, t0, t1, t2)
}

/// The tables of an RAR 3.x stream (a file) against Huffman's lengths of the symbols up to the
/// next table (the 256 ending each block counted); the blocks' sizes against 16384 symbols.
fn tables3(st: &lz3::Stream, t: &mut Totals) {
    let bases = lz3::bases();
    let mut low = (0u32, 0u32);
    let mut cur: Option<(&lz3::Table, Vec<Vec<u32>>)> = None;
    let done = |c: Option<(&lz3::Table, Vec<Vec<u32>>)>, t: &mut Totals| {
        if let Some((tb, c)) = c {
            t.tables += 1;
            let mut k = 0;
            for (i, f) in c.iter().enumerate() {
                t.alike[i] += (huff::lengths(f, huff::MAX) == tb.lens[k..k + f.len()]) as usize;
                k += f.len();
            }
        }
    };
    let n = st.blocks.len();
    for (bi, b) in st.blocks.iter().enumerate() {
        match b {
            lz3::Block::Lz { table, tokens } => {
                if let Some(tb) = table {
                    done(cur.take(), t);
                    low = (0, 0);
                    cur =
                        Some((tb, vec![vec![0; lz3::MAIN], vec![0; lz3::DIST], vec![0; lz3::LOW], vec![0; lz3::LEN]]));
                }
                if let Some((_, c)) = cur.as_mut() {
                    for tk in tokens {
                        lz3::symbols(tk, &bases, &mut low, |k, s| c[k][s] += 1);
                    }
                    c[0][256] += std::env::var_os("NO256").is_none() as u32;
                }
                if std::env::var_os("CUTS").is_some() && bi + 1 < n {
                    println!("  cut3 tokens {} table {}", tokens.len(), table.is_some() as u8);
                }
            }
            lz3::Block::Ppmd { .. } => done(cur.take(), t),
        }
    }
    done(cur, t);
}
