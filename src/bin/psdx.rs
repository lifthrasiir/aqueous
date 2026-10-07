//! Pixel data tools.
//!
//! `psdx check FILE...` decodes every layer channel and the composite image, reporting
//! failures.
//!
//! `psdx predict FILE...` renders the layers of 8-bit RGB files and compares the result with
//! the stored composite: share of exact and near pixels, and order-0 entropy of the residual,
//! of a left-neighbor prediction of the composite alone, and of a mix of the two.
//!
//! `psdx rle FILE...` identifies the PackBits encoder of every RLE channel and of the
//! composite, and prints per file how many rows it reproduces, by encoder.
//!
//! `psdx dump OUTDIR FILE...` writes the 8-bit RGB layers (RGBA when they have transparency),
//! user masks and composite of each file as PAM images under `OUTDIR/<file stem>/`, and
//! prints one TSV line per image: source file, image path, bytes the image takes in the PSD
//! (compression fields and RLE counts included), decoded bytes.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use psd_recons::packbits::{self, Encoder};
use psd_recons::psd::Compression;
use psd_recons::{composite, pixels};
use psd_recons::psd::Psd;

fn check(psd: &Psd, d: &[u8]) -> Vec<String> {
    let mut errs = Vec::new();
    for (i, l) in psd.layer_info.layers.iter().enumerate() {
        for (j, c) in l.channels.iter().enumerate() {
            if let Err(e) = pixels::layer_channel(psd, d, i, j) {
                errs.push(format!("layer {i} channel {}: {e}", c.id));
            }
        }
    }
    if let Err(e) = pixels::composite(psd, d) {
        errs.push(format!("composite: {e}"));
    }
    errs
}

/// Order-0 entropy in bytes of each plane of `planes`, summed.
fn entropy(planes: &[Vec<u8>]) -> f64 {
    let mut bits = 0.0;
    for p in planes {
        let mut h = [0u64; 256];
        for &b in p {
            h[b as usize] += 1;
        }
        let n = p.len() as f64;
        bits += h.iter().filter(|&&c| c > 0).map(|&c| -(c as f64) * (c as f64 / n).log2()).sum::<f64>();
    }
    bits / 8.0
}

fn predict(psd: &Psd, d: &[u8], f: &str) -> Result<(), String> {
    if psd.depth != 8 || psd.mode != 3 || psd.layer_info.layers.is_empty() {
        return Err(format!("skipped: depth {} mode {} layers {}", psd.depth, psd.mode, psd.layer_info.layers.len()));
    }
    let comp = pixels::composite(psd, d).map_err(|e| e.to_string())?;
    let n = (psd.width * psd.height) as usize;
    let with_alpha = psd.channels >= 4 && psd.layer_info.merged_alpha;
    let nc = if with_alpha { 4 } else { 3 };
    let mut rep = composite::Report::default();
    let r = composite::render(psd, d, with_alpha, &mut rep).map_err(|e| e.to_string())?;
    let (mut exact, mut near) = (0usize, 0usize);
    let mut resid = vec![Vec::with_capacity(n); nc];
    let mut left = vec![Vec::with_capacity(n); nc];
    // Hybrid: the residual where the left neighbor was predicted exactly, else the left
    // difference; the two cases coded in separate contexts.
    let mut hyb = vec![Vec::new(); 2 * nc];
    let mut prev_exact = false;
    let w = psd.width as usize;
    for i in 0..n {
        let mut maxd = 0;
        for k in 0..nc {
            let c = comp[k * n + i];
            let p = r[i * nc + k];
            maxd = maxd.max((c as i16 - p as i16).unsigned_abs());
            resid[k].push(c.wrapping_sub(p));
            left[k].push(c.wrapping_sub(if i % w > 0 { comp[k * n + i - 1] } else { 0 }));
        }
        for k in 0..nc {
            let sym = if prev_exact { resid[k][i] } else { left[k][i] };
            hyb[2 * k + prev_exact as usize].push(sym);
        }
        prev_exact = maxd == 0 && (i + 1) % w != 0;
        exact += (maxd == 0) as usize;
        near += (maxd <= 2) as usize;
    }
    let packed = psd.composite.len();
    println!(
        "{f}\t{}x{}\tlayers {}\texact {:.3}\tnear {:.3}\tpacked {packed}\tleft-H0 {:.0}\tresid-H0 {:.0}\thybrid-H0 {:.0}\tskipped {}\teffects {}\tfallback {:?}",
        psd.width,
        psd.height,
        psd.layer_info.layers.len(),
        exact as f64 / n as f64,
        near as f64 / n as f64,
        entropy(&left),
        entropy(&resid),
        entropy(&hyb),
        rep.skipped,
        rep.effects,
        rep.blend_fallback,
    );
    Ok(())
}

fn rle(psd: &Psd, d: &[u8], f: &str) {
    let cb = if psd.psb { 4 } else { 2 };
    let mut planes: Vec<(&[u8], usize)> = Vec::new();
    for l in &psd.layer_info.layers {
        for c in &l.channels {
            if c.compression(d) == Some(Compression::Rle) {
                planes.push((&d[c.span.start + 2..c.span.end], c.height as usize));
            }
        }
    }
    let comp_rows = (psd.height * psd.channels as u64) as usize;
    if psd.composite_compression(d) == Compression::Rle {
        // Each composite plane is encoded on its own.
        let data = &d[psd.composite.start + 2..];
        if let Some(rows) = packbits::rows(data, comp_rows, cb) {
            let h = psd.height as usize;
            for k in 0..psd.channels as usize {
                let r = &rows[k * h..(k + 1) * h];
                let (enc, n) = packbits::identify(r);
                print_kind(enc, n, h, true);
            }
        }
    }
    let (mut rows_all, mut ok) = (0, 0);
    let mut kinds: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for (data, h) in planes {
        let Some(rows) = packbits::rows(data, h, cb) else { continue };
        let (enc, n) = packbits::identify(&rows);
        let k = kinds.entry(kind(enc)).or_default();
        k.0 += h;
        k.1 += n;
        rows_all += h;
        ok += n;
    }
    println!("{f}\trows {rows_all}\treproduced {:.5}\t{kinds:?}", ok as f64 / rows_all.max(1) as f64);
}

fn kind(enc: Encoder) -> String {
    match enc {
        Encoder::Photoshop { period: usize::MAX, .. } => "ps".into(),
        Encoder::Photoshop { period, .. } if period >= 4096 => "ps-1tile".into(),
        Encoder::Photoshop { period, .. } => format!("ps-{period}"),
        Encoder::Greedy { min_run, split_rest } => format!("greedy{min_run}{}", if split_rest { "s" } else { "" }),
    }
}

fn print_kind(enc: Encoder, n: usize, h: usize, composite: bool) {
    if n < h {
        println!("  {}: {} {n}/{h}", if composite { "composite plane" } else { "plane" }, kind(enc));
    }
}

fn write_pam(path: &Path, w: u64, h: u64, planes: &[&[u8]]) -> std::io::Result<()> {
    let tuple = match planes.len() {
        1 => "GRAYSCALE",
        2 => "GRAYSCALE_ALPHA",
        3 => "RGB",
        _ => "RGB_ALPHA",
    };
    let mut out = Vec::with_capacity(64 + (w * h) as usize * planes.len());
    write!(out, "P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH {}\nMAXVAL 255\nTUPLTYPE {tuple}\nENDHDR\n", planes.len())?;
    for i in 0..(w * h) as usize {
        for p in planes {
            out.push(p[i]);
        }
    }
    std::fs::write(path, out)
}

fn dump(psd: &Psd, d: &[u8], src: &str, dir: &Path) -> Result<(), String> {
    if psd.depth != 8 || psd.mode != 3 {
        return Err(format!("skipped: depth {} mode {}", psd.depth, psd.mode));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let line = |name: &str, packed: usize, raw: usize| {
        println!("{src}\t{}\t{packed}\t{raw}", dir.join(name).display());
    };
    for (i, l) in psd.layer_info.layers.iter().enumerate() {
        let find = |id: i16| l.channels.iter().position(|c| c.id == id);
        let (w, h) = (l.rect.width(), l.rect.height());
        if w == 0 || h == 0 {
            continue;
        }
        let ids: Vec<i16> = [0, 1, 2, -1].into_iter().filter(|&id| find(id).is_some()).collect();
        if ids.len() < 3 {
            continue;
        }
        let planes: Vec<Vec<u8>> = ids
            .iter()
            .map(|&id| pixels::layer_channel(psd, d, i, find(id).unwrap()))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        // An all-opaque alpha carries nothing; leave it out so the coders see RGB.
        let keep = if planes.len() == 4 && planes[3].iter().all(|&b| b == 255) { 3 } else { planes.len() };
        let refs: Vec<&[u8]> = planes[..keep].iter().map(|p| p.as_slice()).collect();
        let name = format!("L{i:04}.pam");
        write_pam(&dir.join(&name), w, h, &refs).map_err(|e| e.to_string())?;
        let packed: usize = ids.iter().map(|&id| l.channels[find(id).unwrap()].span.len()).sum();
        line(&name, packed, planes.iter().map(|p| p.len()).sum());
        for (j, c) in l.channels.iter().enumerate() {
            if c.id >= -1 || c.width == 0 || c.height == 0 {
                continue;
            }
            let m = pixels::layer_channel(psd, d, i, j).map_err(|e| e.to_string())?;
            let name = format!("L{i:04}m{}.pam", -c.id);
            write_pam(&dir.join(&name), c.width, c.height, &[&m]).map_err(|e| e.to_string())?;
            line(&name, c.span.len(), m.len());
        }
    }
    let comp = pixels::composite(psd, d).map_err(|e| e.to_string())?;
    let n = (psd.width * psd.height) as usize;
    let planes: Vec<&[u8]> = comp.chunks(n).take(4).collect();
    if planes.len() >= 3 {
        write_pam(&dir.join("C.pam"), psd.width, psd.height, &planes).map_err(|e| e.to_string())?;
        line("C.pam", psd.composite.len() * planes.len() / psd.channels as usize, n * planes.len());
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => {
            eprintln!("usage: psdx check|predict|rle FILE... | psdx dump OUTDIR FILE...");
            return ExitCode::FAILURE;
        }
    };
    let (outdir, files) = match cmd {
        "check" | "predict" | "rle" => (None, rest),
        "dump" if !rest.is_empty() => (Some(Path::new(&rest[0])), &rest[1..]),
        _ => {
            eprintln!("unknown command {cmd}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = 0;
    for f in files {
        let d = match std::fs::read(f) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{f}: {e}");
                failed += 1;
                continue;
            }
        };
        let psd = match psd_recons::parse(&d) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{f}: {e}");
                failed += 1;
                continue;
            }
        };
        match outdir {
            None if cmd == "rle" => rle(&psd, &d, f),
            None if cmd == "predict" => {
                if let Err(e) = predict(&psd, &d, f) {
                    eprintln!("{f}: {e}");
                    failed += 1;
                }
            }
            None => {
                let errs = check(&psd, &d);
                for e in &errs {
                    eprintln!("{f}: {e}");
                }
                failed += !errs.is_empty() as usize;
            }
            Some(o) => {
                let stem = Path::new(f).file_stem().unwrap_or_default();
                if let Err(e) = dump(&psd, &d, f, &o.join(stem)) {
                    eprintln!("{f}: {e}");
                    failed += 1;
                }
            }
        }
    }
    if failed > 0 {
        eprintln!("{failed} of {} files failed", files.len());
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
