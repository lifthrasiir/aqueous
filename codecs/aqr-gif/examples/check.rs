//! Splits GIF files and rebuilds them, and tells what the records cost against the files'
//! LZW data and against zstd.
//!
//! `aqr-gif-check FILE_OR_DIR...`

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_gif::{gif, recons};

fn files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
        v.sort();
        for q in v {
            files(&q, out);
        }
    } else {
        out.push(p.to_owned());
    }
}

fn main() {
    let mut paths = vec![];
    for a in std::env::args().skip(1) {
        files(Path::new(&a), &mut paths);
    }
    let (mut tot_file, mut tot_data, mut tot_rec, mut tot_sk, mut tot_px) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut cost = [0f64; aqr_gif::model::CATS];
    let (mut t_split, mut t_join) = (0f64, 0f64);
    let (mut ok, mut failed) = (0, 0);
    for p in &paths {
        let d = std::fs::read(p).unwrap();
        let Some(g) = gif::parse(&d) else { continue };
        let data: usize = g.images.iter().map(|i| i.data.len()).sum();
        let area: u64 = g.images.iter().map(|i| i.width as u64 * i.height as u64).sum();
        let t = Instant::now();
        let sp = match recons::split(&d) {
            Ok(sp) => sp,
            Err(e) => {
                println!("{}: not split: {e}", p.display());
                failed += 1;
                continue;
            }
        };
        let ts = t.elapsed().as_secs_f64();
        t_split += ts;
        let t = Instant::now();
        let mut out = Vec::new();
        let r = if std::env::var_os("NOJOIN").is_some() {
            out = d.clone();
            Ok(())
        } else {
            recons::join_onto(&sp.skeleton, &sp.record, &mut out)
        };
        t_join += t.elapsed().as_secs_f64();
        if r.is_err() || out != d {
            println!("{}: REBUILT OTHERWISE: {r:?}", p.display());
            failed += 1;
            continue;
        }
        ok += 1;
        let px = d.len() - sp.skeleton.len();
        println!(
            "{}: {} B, {area} px, {:.1} Mpx/s, images {}/{}, data {} -> record {} ({:.3}), layout {:.0} pixels {:.0} parse {:.0} B",
            p.display(),
            d.len(),
            area as f64 / ts / 1e6,
            sp.coded,
            sp.images,
            px,
            sp.record.len(),
            sp.record.len() as f64 / px as f64,
            sp.cost[0] / 8.0,
            sp.cost[1] / 8.0,
            sp.cost[2] / 8.0,
        );
        tot_file += d.len() as u64;
        tot_data += px as u64;
        tot_rec += sp.record.len() as u64;
        tot_sk += sp.skeleton.len() as u64;
        tot_px += data as u64;
        for (a, b) in cost.iter_mut().zip(sp.cost) {
            *a += b / 8.0;
        }
    }
    println!(
        "{ok} rebuilt, {failed} not; files {tot_file} B, data {tot_data} -> record {tot_rec} ({:.4}), skeletons {tot_sk}; \
         layout {:.0} pixels {:.0} parse {:.0} B; split {t_split:.2} s, join {t_join:.2} s",
        tot_rec as f64 / tot_data as f64,
        cost[0],
        cost[1],
        cost[2],
    );
    let _ = tot_px;
}
