//! Which bvx streams in files split into their data and a record and are rebuilt bit for bit,
//! and what the records cost. Only totals are printed, never names or contents.
//!
//! ```sh
//! cargo run --release -p aqr-bvx --example aqr-bvx-check -- FILE_OR_DIR...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_bvx::recons;

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

fn main() {
    let mut paths = Vec::new();
    std::env::args().skip(1).for_each(|a| walk(Path::new(&a), &mut paths));
    let mut outcome: BTreeMap<String, usize> = BTreeMap::new();
    // per kind (LZFSE or LZVN): streams, stream bytes, data bytes, record bytes, costs
    let mut tot: BTreeMap<&str, (usize, usize, usize, usize, [f64; recons::CATS])> = BTreeMap::new();
    let (mut t_split, mut t_join) = (0.0, 0.0);
    for p in paths {
        if p.extension().is_none_or(|e| e != "lzfse") {
            continue;
        }
        let s = std::fs::read(&p).unwrap();
        let t = Instant::now();
        let k = match recons::split(&s, usize::MAX) {
            Err(e) => {
                if std::env::var_os("AQR_BVX_NAMES").is_some() {
                    eprintln!("{}: {e}", p.display());
                }
                format!("split: {e}")
            }
            Ok(sp) => {
                t_split += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let mut out = Vec::new();
                let r = recons::join_onto(&sp.data, &sp.record, &mut out);
                t_join += t.elapsed().as_secs_f64();
                match r {
                    Err(e) => format!("join: {e}"),
                    Ok(()) if out != s[..sp.used] => "rebuilt otherwise".into(),
                    Ok(()) => {
                        let kind = if s.starts_with(b"bvxn") { "lzvn" } else { "lzfse" };
                        let e = tot.entry(kind).or_default();
                        e.0 += 1;
                        e.1 += sp.used;
                        e.2 += sp.data.len();
                        e.3 += sp.record.len();
                        for i in 0..recons::CATS {
                            e.4[i] += sp.cost[i] / 8.0;
                        }
                        "rebuilt".into()
                    }
                }
            }
        };
        *outcome.entry(k).or_default() += 1;
    }
    for (k, n) in outcome {
        println!("{n:6} {k}");
    }
    for (k, (n, s, d, r, c)) in tot {
        println!(
            "{k}: {n} streams, {s} B of streams, {d} B of data, records {r} B ({:.2}% of the streams); layout {:.0} start {:.0} len {:.0} dist {:.0} B",
            100.0 * r as f64 / s as f64,
            c[0],
            c[1],
            c[2],
            c[3]
        );
    }
    println!("split {t_split:.2} s, join {t_join:.2} s");
}
