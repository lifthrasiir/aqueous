//! RAR archives: their streams decoded, checked against the headers' CRC-32, encoded again from
//! their tokens. Totals are printed (`-v` adds a line per stream).
//!
//! ```sh
//! cargo run --release -p aqr-rar --example aqr-rar-check -- [-v] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_common::crc32;
use aqr_rar::{lz5, rar5};

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
    for e in g {
        let packed = &d[e.data.clone()];
        let at = dec.out.len();
        let st = match dec.file(packed, e.size.unwrap() as usize) {
            Ok(st) => st,
            Err(err) => {
                t.failed += 1;
                return Some(format!("decoding: {err}"));
            }
        };
        let mut file = dec.out[at..].to_vec();
        if let Err(err) = lz5::unfilter(&mut file, &st.placed()) {
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
    }
    t.same += 1;
    None
}
