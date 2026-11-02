//! Round trip of the Apple accelerator tables in dumped sections: `check DIR...`, each DIR
//! holding `debug_str` and any of `apple_names`, `apple_types`, `apple_namespac`, `apple_objc`.
//! Prints each table's size, its streams' and zstd -3 of both.

fn z(d: &[u8]) -> usize {
    zstd::bulk::compress(d, 3).unwrap().len()
}

fn main() {
    for dir in std::env::args().skip(1) {
        let dir = std::path::Path::new(&dir);
        let strs = std::fs::read(dir.join("debug_str")).unwrap();
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
