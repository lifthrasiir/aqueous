//! aqr-infer-pak-probe FILE...: the directories inferred in each file's family (the files of
//! its stem in its directory), with the time of each step. AQR_PAK_VERBOSE=1 tells what each
//! table gave.

use std::path::Path;
use std::time::Instant;

use aqr_infer_pak::tiling::Layout;
use aqr_infer_pak::{File, Options, infer};

fn family(path: &Path) -> Vec<std::path::PathBuf> {
    let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().to_lowercase());
    let mut out = vec![path.to_path_buf()];
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut v: Vec<_> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.file_name() != path.file_name() && stem(p) == stem(path))
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: aqr-infer-pak-probe FILE...");
        std::process::exit(2);
    }
    for a in &args {
        let paths = family(Path::new(a));
        let maps: Vec<memmap2::Mmap> = paths
            .iter()
            .map(|p| unsafe { memmap2::Mmap::map(&std::fs::File::open(p).expect("open")).expect("map") })
            .collect();
        let files: Vec<File> = paths
            .iter()
            .zip(&maps)
            .map(|(p, m)| File { name: p.file_name().unwrap().to_string_lossy().into_owned(), data: &m[..] })
            .collect();
        let sizes: Vec<String> = files.iter().map(|f| format!("{} {}", f.name, f.data.len())).collect();
        let t0 = Instant::now();
        let opt = Options { verbose: std::env::var_os("AQR_PAK_VERBOSE").is_some(), ..Options::default() };
        let (dirs, t) = infer(&files, &opt);
        let total = t0.elapsed();
        println!("{a}: {}", sizes.join(", "));
        for d in &dirs {
            let tb = &d.cand.table;
            let how = match &d.cand.layout {
                Layout::Explicit { off, size, alt, fit, ctrl } => {
                    let alt = alt.map(|a| format!(" or {a}")).unwrap_or_default();
                    format!("offset {off} size {size}{alt} (fit {fit:.2}, control {ctrl:.2})")
                }
                Layout::Implicit { size, align, base } => {
                    format!("sizes {size} end to end from {base:#x}, aligned {align}")
                }
            };
            println!(
                "  -> {} ({} B) by {} {:#x}..{:#x}/{}: {} members, {how}; covers {:.1}%, {:.0} bits",
                d.target,
                d.target_len,
                d.owner,
                tb.start,
                tb.end,
                tb.k,
                d.members.len(),
                100.0 * d.cover,
                d.ev
            );
            println!("     codecs {:?}; contents {:?}", d.codecs, &d.contents[..d.contents.len().min(6)]);
            if let Some(n) = d.decoded {
                println!("     decoded in place: {n} bytes");
            }
        }
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        println!(
            "  ({:.1} ms: tables {:.1}, profiles {:.1}, explicit {:.1}, implicit {:.1}, codecs {:.1}, decoded targets {:.1})",
            ms(total),
            ms(t.tables),
            ms(t.profiles),
            ms(t.explicit),
            ms(t.implicit),
            ms(t.codecs),
            ms(t.decode)
        );
    }
}
