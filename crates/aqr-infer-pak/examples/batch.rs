//! aqr-infer-pak-batch PATH...: the directories of a batch of files (directories given are
//! taken a level deep), as aqr would trigger the inference: the files of an unknown format
//! that start alike (the same 8 bytes in 3 files at least) are a group, each file with its
//! family (the files of its stem in its directory). The biggest family of a group is inferred
//! and its layout learned; the others verify the template, inferred where it does not hold
//! (and that layout learned too).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aqr_infer_pak::template::{self, Template};
use aqr_infer_pak::{Directory, File, Options, infer};

/// Whether 8 head bytes can tell a format: not zeros, not one byte, not text.
fn magic_like(h: &[u8]) -> bool {
    h.len() == 8 && h.iter().any(|&x| x != h[0]) && !h.iter().all(|&x| (0x20..0x7f).contains(&x) || x == b'\n')
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn main() {
    let mut paths: Vec<PathBuf> = Vec::new();
    for a in std::env::args().skip(1) {
        let p = PathBuf::from(&a);
        if p.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(&p).unwrap().filter_map(|e| e.ok().map(|e| e.path())).collect();
            v.retain(|q| q.is_file());
            v.sort();
            paths.extend(v);
        } else {
            paths.push(p);
        }
    }
    let maps: Vec<memmap2::Mmap> = paths
        .iter()
        .map(|p| unsafe { memmap2::Mmap::map(&std::fs::File::open(p).expect("open")).expect("map") })
        .collect();
    // families, and the groups of key files by their head
    let mut families: BTreeMap<(PathBuf, String), Vec<usize>> = BTreeMap::new();
    for (i, p) in paths.iter().enumerate() {
        families.entry((p.parent().unwrap_or(Path::new("")).to_path_buf(), stem(p))).or_default().push(i);
    }
    let mut groups: BTreeMap<Vec<u8>, Vec<(usize, Vec<usize>)>> = BTreeMap::new();
    for fam in families.values() {
        for &i in fam {
            let h = &maps[i][..maps[i].len().min(8)];
            if magic_like(h) {
                groups.entry(h.to_vec()).or_default().push((i, fam.clone()));
            }
        }
    }
    groups.retain(|_, v| v.len() >= 3);
    let grouped: usize = groups.values().map(|v| v.len()).sum();
    println!("{} files, {} families; {} groups of {} files", paths.len(), families.len(), groups.len(), grouped);
    let (mut t_full, mut t_tmpl, mut n_full, mut n_tmpl, mut n_none) = (Duration::ZERO, Duration::ZERO, 0, 0, 0);
    let mut dirs_found = 0usize;
    // the biggest groups first, each family once (data files may start alike too, by the
    // kind of their first member: their family is their index's group's)
    type Group = (Vec<u8>, Vec<(usize, Vec<usize>)>);
    let mut groups: Vec<Group> = groups.into_iter().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
    let mut done = std::collections::HashSet::new();
    for (magic, mut members) in groups {
        members.retain(|(_, fam)| done.insert(fam.clone()));
        if members.is_empty() {
            continue;
        }
        members.sort_by_key(|(_, fam)| std::cmp::Reverse(fam.iter().map(|&i| maps[i].len()).sum::<usize>()));
        println!("group {:02x?} ({} families)", magic, members.len());
        let mut templates: Vec<Template> = Vec::new();
        let mut retry = Vec::new();
        for (key, fam) in members {
            // the key file first
            let mut order = vec![key];
            order.extend(fam.iter().copied().filter(|&i| i != key));
            let files: Vec<File> = order
                .iter()
                .map(|&i| File {
                    name: paths[i].file_name().unwrap().to_string_lossy().into_owned(),
                    data: &maps[i][..],
                })
                .collect();
            let t0 = Instant::now();
            let mut how = "template";
            let mut dirs: Option<Vec<Directory>> = templates.iter().find_map(|t| template::verify(t, &files, 0));
            if dirs.is_some() {
                t_tmpl += t0.elapsed();
                n_tmpl += 1;
            } else {
                how = "inferred";
                let (d, _) = infer(&files, &Options::default());
                if let Some(t) = template::learn(&files, 0, &d) {
                    if std::env::var_os("AQR_PAK_VERBOSE").is_some() {
                        eprintln!("    learned {:?}", t.dirs);
                    }
                    templates.push(t);
                }
                t_full += t0.elapsed();
                n_full += 1;
                dirs = Some(d);
            }
            let dirs = dirs.unwrap();
            if dirs.is_empty() {
                retry.push((key, fam.clone()));
            }
            dirs_found += dirs.len();
            let what: Vec<String> = dirs.iter().map(|d| format!("{} {}", d.target, d.members.len())).collect();
            println!(
                "  {:<40} {how} {:>8.1} ms: {}",
                files[0].name,
                t0.elapsed().as_secs_f64() * 1e3,
                if what.is_empty() { "nothing".to_string() } else { what.join(", ") }
            );
        }
        // those with nothing, again by the templates learned after them
        for (key, fam) in retry {
            let mut order = vec![key];
            order.extend(fam.iter().copied().filter(|&i| i != key));
            let files: Vec<File> = order
                .iter()
                .map(|&i| File {
                    name: paths[i].file_name().unwrap().to_string_lossy().into_owned(),
                    data: &maps[i][..],
                })
                .collect();
            let t0 = Instant::now();
            let dirs = templates.iter().find_map(|t| template::verify(t, &files, 0)).unwrap_or_default();
            t_tmpl += t0.elapsed();
            if dirs.is_empty() {
                n_none += 1;
                continue;
            }
            n_tmpl += 1;
            dirs_found += dirs.len();
            let what: Vec<String> = dirs.iter().map(|d| format!("{} {}", d.target, d.members.len())).collect();
            println!(
                "  {:<40} retried {:>8.1} ms: {}",
                files[0].name,
                t0.elapsed().as_secs_f64() * 1e3,
                what.join(", ")
            );
        }
    }
    println!(
        "inferred {n_full} families in {:.1} s, verified {n_tmpl} by templates in {:.1} s; {n_none} with nothing; {dirs_found} directories",
        t_full.as_secs_f64(),
        t_tmpl.as_secs_f64()
    );
}
