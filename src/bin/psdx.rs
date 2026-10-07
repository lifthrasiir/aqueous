//! Pixel data tools.
//!
//! `psdx check FILE...` decodes every layer channel and the composite image, reporting
//! failures.
//!
//! `psdx dump OUTDIR FILE...` writes the 8-bit RGB layers (RGBA when they have transparency),
//! user masks and composite of each file as PAM images under `OUTDIR/<file stem>/`, and
//! prints one TSV line per image: source file, image path, bytes the image takes in the PSD
//! (compression fields and RLE counts included), decoded bytes.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use psd_recons::pixels;
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
            eprintln!("usage: psdx check FILE... | psdx dump OUTDIR FILE...");
            return ExitCode::FAILURE;
        }
    };
    let (outdir, files) = match cmd {
        "check" => (None, rest),
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
