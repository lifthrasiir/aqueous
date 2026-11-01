//! RAR archives: their streams decoded, checked against the headers' CRC-32, encoded again from
//! their tokens. Totals are printed (`-v` adds a line per stream).
//!
//! ```sh
//! cargo run --release -p aqr-rar --example aqr-rar-check -- [-v] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_common::crc32;
use aqr_rar::{huff, lz5, rar5, rec, recons5};

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
        let Some(a) = rar5::parse(&d) else {
            if verbose {
                println!("{}: no RAR5 archive", p.display());
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
    if std::env::var_os("CUTS").is_some() {
        // each block: its tokens, whether it has tables, where in the file it ends
        for (fi, st) in sts.iter().enumerate() {
            let (mut at, mut pos, mut last) = (0, 0usize, 0u32);
            for (bi, b) in st.blocks.iter().enumerate() {
                for tk in &st.tokens[at..at + b.tokens] {
                    match *tk {
                        lz5::Token::Lit(_) => pos += 1,
                        lz5::Token::Copy { len, .. } | lz5::Token::Rep { len, .. } => {
                            pos += len as usize;
                            last = len;
                        }
                        lz5::Token::Last => pos += last as usize,
                        _ => {}
                    }
                }
                let nf = st.tokens[at..at + b.tokens].iter().filter(|t| matches!(t, lz5::Token::Filter(_))).count();
                if !b.last {
                    let def = 16384 - (bi == 0) as usize - b.tokens;
                    let fs: Vec<String> = st.tokens[at..at + b.tokens]
                        .iter()
                        .filter_map(|t| match t {
                            lz5::Token::Filter(f) => Some(format!(
                                "k{}c{}l{}s{}b{}{}",
                                f.kind, f.channels, f.len, f.start, f.start_bytes, f.len_bytes
                            )),
                            _ => None,
                        })
                        .collect();
                    println!("  deficit {def} filters {nf}: {}", fs.join(" "));
                }
                at += b.tokens;
                println!(
                    "  cut f{fi} b{bi} tokens {} filters {nf} tables {} last {} end {} end%128K {}",
                    b.tokens,
                    b.tables.is_some() as u8,
                    b.last as u8,
                    pos,
                    pos % 131072
                );
            }
        }
    }
    t.same += 1;
    None
}
