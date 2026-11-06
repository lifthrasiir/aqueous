//! Round trip of the sections dumped in each DIR of `check DIR...` (named as `debug_str`,
//! `apple_names`, ...): `.debug_info` (with `debug_abbrev` and `debug_str`) and Apple's
//! accelerator tables. Prints each section's size, its streams' and zstd -3 of both.

fn z(d: &[u8]) -> usize {
    zstd::bulk::compress(d, 3).unwrap().len()
}

fn main() {
    for dir in std::env::args().skip(1) {
        let dir = std::path::Path::new(&dir);
        let strs = std::fs::read(dir.join("debug_str")).unwrap_or_default();
        if let (Ok(d), Ok(abbrev)) = (std::fs::read(dir.join("debug_info")), std::fs::read(dir.join("debug_abbrev"))) {
            info(dir, &d, &abbrev, &strs);
        }
        for name in ["apple_names", "apple_types", "apple_namespac", "apple_objc"] {
            let Ok(d) = std::fs::read(dir.join(name)) else { continue };
            let t = std::time::Instant::now();
            let Some((params, st)) = aqr_dwarf::apple::encode(&d, &strs) else {
                println!("{}/{name}: not taken", dir.display());
                continue;
            };
            let enc = t.elapsed();
            let inputs: Vec<&[u8]> = [&strs[..]].into_iter().chain(st.iter().map(|s| &s[..])).collect();
            let t = std::time::Instant::now();
            let back = aqr_dwarf::apple::decode(&params, &inputs, d.len() as u64).unwrap();
            let dec = t.elapsed();
            assert!(back == d);
            let zs: usize = st.iter().map(|s| z(s)).sum();
            println!(
                "{}/{name}: {} -> {} B; zstd -3 {} -> {} ({:?}) B; encode {enc:.2?}, decode {dec:.2?}",
                dir.display(),
                d.len(),
                st.iter().map(Vec::len).sum::<usize>(),
                z(&d),
                zs,
                st.iter().map(|s| z(s)).collect::<Vec<_>>(),
            );
        }
    }
}

fn info(dir: &std::path::Path, d: &[u8], abbrev: &[u8], strs: &[u8]) {
    let t = std::time::Instant::now();
    let Some((params, st)) = aqr_dwarf::info::encode(d, abbrev, strs) else {
        println!("{}/debug_info: not taken", dir.display());
        return;
    };
    let enc = t.elapsed();
    let inputs: Vec<&[u8]> = [abbrev, strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    let t = std::time::Instant::now();
    let back = aqr_dwarf::info::decode(&params, &inputs, d.len() as u64).unwrap();
    let dec = t.elapsed();
    assert!(back == d);
    let zs: Vec<usize> = st.iter().map(|s| z(s)).collect();
    println!(
        "{}/debug_info: {} -> {} B; zstd -3 {} -> {} B; encode {enc:.2?}, decode {dec:.2?}",
        dir.display(),
        d.len(),
        st.iter().map(Vec::len).sum::<usize>(),
        z(d),
        zs.iter().sum::<usize>(),
    );
    for ((name, s), z) in aqr_dwarf::info::STREAM_NAMES.iter().zip(&st).zip(&zs) {
        println!("  {name:6} {:>10} {:>10}", s.len(), z);
    }
    // groupings of the streams, concatenated, under zstd -3
    let names = aqr_dwarf::info::STREAM_NAMES;
    let idx = |n: &str| names.iter().position(|&m| m == n).unwrap();
    let groups: &[&[&[&str]]] = &[
        &[
            &["code"],
            &["str"],
            &["ref"],
            &["addr", "line", "call", "file", "sec", "idx", "data", "pclen"],
            &["unit", "expr", "cstr", "raw"],
        ],
        &[
            &["code"],
            &["str"],
            &["ref"],
            &["addr", "pclen"],
            &["line", "call", "file"],
            &["sec", "idx", "data"],
            &["unit", "expr", "cstr", "raw"],
        ],
        &[
            &["code", "str", "ref"],
            &["addr", "line", "call", "file", "sec", "idx", "data", "pclen"],
            &["unit", "expr", "cstr", "raw"],
        ],
    ];
    for g in groups {
        let total: usize =
            g.iter().map(|grp| z(&grp.iter().flat_map(|n| st[idx(n)].iter().copied()).collect::<Vec<u8>>())).sum();
        println!("  groups {}: {total}", g.len());
    }
}
