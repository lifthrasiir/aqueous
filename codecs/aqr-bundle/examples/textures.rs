//! Unity's textures in files of serialized objects, by format: for each of the paths (files, or
//! directories walked), the textures of the serialized files there (their blocks in the object,
//! or in a resource file beside), tallied by aqr-tex's format; with `-o DIR`, those of the formats
//! `-f` names (all but BC1 to BC5 by default) written to DIR as `.tex` files for aqr-tex-bench
//! (`AQRTEX` and 2 zero bytes, the layout's params' length as a u32 little-endian, the params,
//! the blocks).
//!
//! ```sh
//! cargo run --release -p aqr-bundle --example aqr-bundle-textures -- [-o DIR] [-f bc7,astc,...] PATH...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aqr_bundle::packs::unity_assets::{self, Data, Pixels};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    match std::fs::read_dir(p) {
        Ok(rd) => {
            let mut es: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            es.sort();
            for e in es {
                walk(&e, out);
            }
        }
        Err(_) => out.push(p.to_path_buf()),
    }
}

fn main() {
    let (mut dir, mut only, mut paths) = (None, None::<Vec<String>>, vec![]);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => dir = Some(PathBuf::from(args.next().unwrap())),
            "-f" => only = Some(args.next().unwrap().split(',').map(str::to_owned).collect()),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let mut tally: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut written = 0;
    for p in &paths {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if matches!(ext, "resS" | "resource" | "dll" | "exe" | "png" | "ogg" | "wav" | "mp4" | "bank") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(p) else { continue };
        if meta.len() < 64 || meta.len() > 4 << 30 {
            continue;
        }
        let Ok(d) = std::fs::read(p) else { continue };
        let Some(o) = unity_assets::objects(&d) else { continue };
        for (k, ob) in o.objects.iter().enumerate().filter(|(_, ob)| ob.class == 28) {
            let Some(t) = unity_assets::texture(&d[ob.extent.clone()], o.engine, o.big) else { continue };
            let Pixels::Blocks(l) = t.pixels else { continue };
            let name = match l.format {
                aqr_tex::layout::Format::Astc { w, h } => format!("astc{w}x{h}"),
                f => f.name().to_owned(),
            };
            let blocks: Option<Vec<u8>> = match &t.data {
                Data::Inline(r) => Some(d[ob.extent.start + r.start..ob.extent.start + r.end].to_vec()),
                Data::Stream { path, offset, size } => {
                    // (the resource file by its name, beside the serialized file)
                    let name = String::from_utf8_lossy(path);
                    let name = name.rsplit(['/', ':']).next().unwrap_or("");
                    let res = p.with_file_name(name);
                    std::fs::read(&res).ok().and_then(|r| {
                        let (a, b) = (*offset as usize, (*offset + *size) as usize);
                        r.get(a..b).map(<[u8]>::to_vec)
                    })
                }
            };
            let Some(blocks) = blocks else { continue };
            let e = tally.entry(name.clone()).or_default();
            e.0 += 1;
            e.1 += blocks.len() as u64;
            let want = match &only {
                Some(o) => o.iter().any(|f| *f == name || name.starts_with(f.as_str())),
                None => l.format.group() != aqr_tex::layout::Group::Bcn,
            };
            if let Some(dir) = &dir
                && want
            {
                let game = p.ancestors().find(|a| a.extension().is_none() && a.to_string_lossy().ends_with("_Data"));
                let game = game.and_then(|g| g.file_name()).map(|g| g.to_string_lossy().replace(' ', "_"));
                let stem = p.file_name().unwrap().to_string_lossy();
                let out = dir.join(format!("{}-{stem}-{k}.{name}.tex", game.unwrap_or_default()));
                let params = l.to_bytes();
                let mut f = b"AQRTEX\0\0".to_vec();
                f.extend((params.len() as u32).to_le_bytes());
                f.extend(params);
                f.extend(&blocks);
                std::fs::write(out, f).unwrap();
                written += 1;
            }
        }
    }
    for (k, (n, b)) in &tally {
        println!("{k}: {n} textures, {:.1} MB", *b as f64 / 1e6);
    }
    if dir.is_some() {
        println!("{written} written");
    }
}
