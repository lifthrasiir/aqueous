//! Round trip of PDB files: `check FILE_OR_DIR...`. Prints each file's size, its inputs' and
//! zstd -3 of both.

fn z(d: &[u8]) -> usize {
    zstd::bulk::compress(d, 3).unwrap().len()
}

fn files(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if p.is_dir() {
        let mut e: Vec<_> = std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
        e.sort();
        e.iter().for_each(|p| files(p, out));
    } else {
        out.push(p.to_path_buf());
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--dump") {
        // the formula's streams, each to OUT.k: `--dump FILE OUT`
        let a: Vec<String> = std::env::args().collect();
        let f = aqr_pdb::formula::encode(&std::fs::read(&a[2]).unwrap(), 1).unwrap();
        for (k, s) in f.streams.iter().enumerate() {
            std::fs::write(format!("{}.{k}", a[3]), &s.data[..]).unwrap();
            println!("{k} {} {}", s.class, s.data.len());
        }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--types") {
        // the TPI and IPI streams coded: `--types FILE... [--dump DIR]`
        let a: Vec<String> = std::env::args().skip(2).collect();
        let dump = a.iter().position(|x| x == "--dump").map(|k| a[k + 1].clone());
        a.iter().filter(|x| x.ends_with(".pdb")).for_each(|p| types(p, dump.as_deref()));
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--formula") {
        std::env::args().skip(2).for_each(|p| time_formula(&p));
        return;
    }
    let mut paths = Vec::new();
    std::env::args().skip(1).for_each(|a| files(a.as_ref(), &mut paths));
    let (mut tot, mut tot_z, mut tot_in) = (0, 0, 0);
    for p in paths {
        let d = std::fs::read(&p).unwrap();
        if !aqr_pdb::msf::starts(&d) {
            continue;
        }
        let t = std::time::Instant::now();
        let Some((inputs, spans)) = aqr_pdb::msf::encode_streams(&d) else {
            println!("{}: not taken", p.display());
            continue;
        };
        let enc = t.elapsed();
        let refs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
        let t = std::time::Instant::now();
        assert!(aqr_pdb::msf::decode(&refs, d.len() as u64).unwrap() == d);
        let dec = t.elapsed();
        let zi: Vec<usize> = inputs.iter().map(|s| z(s)).collect();
        let (zd, zs) = (z(&d), zi.iter().sum::<usize>());
        println!("{}: {} B; zstd -3 {zd} -> {zs} ({zi:?}); encode {enc:.2?}, decode {dec:.2?}", p.display(), d.len());
        (tot, tot_z, tot_in) = (tot + d.len(), tot_z + zd, tot_in + zs);
        symbols(&inputs[0], &spans);
    }
    println!("all: {tot} B; zstd -3 {tot_z} -> {tot_in}");
}

/// The modules' symbols through `pdb-symbols`.
fn symbols(streams: &[u8], spans: &[Option<std::ops::Range<usize>>]) {
    let stream = |k: usize| spans.get(k).cloned().flatten().map(|r| &streams[r]);
    let Some(mods) = stream(3).and_then(aqr_pdb::dbi::modules) else { return };
    let subs: Vec<(&[u8], aqr_pdb::dbi::Module)> =
        mods.iter().filter_map(|&m| stream(m.stream).filter(|s| m.symbols <= s.len()).map(|s| (s, m))).collect();
    let raw: Vec<u8> = subs.iter().flat_map(|&(s, _)| s).copied().collect();
    let t = std::time::Instant::now();
    let Some((_, st)) = aqr_pdb::symbols::encode(&subs) else {
        println!("  symbols: not taken");
        return;
    };
    let enc = t.elapsed();
    let zs: Vec<usize> = st.iter().map(|s| z(s)).collect();
    println!(
        "  modules' streams, {}: {} B; zstd -3 {} -> {}; encode {enc:.2?}",
        subs.len(),
        raw.len(),
        z(&raw),
        zs.iter().sum::<usize>()
    );
    for ((name, s), z) in aqr_pdb::symbols::STREAM_NAMES.iter().zip(&st).zip(&zs) {
        println!("    {name:6} {:>10} {:>10}", s.len(), z);
    }
}

/// Times `formula::encode` of a file, for `check --formula FILE`.
#[allow(dead_code)]
fn time_formula(p: &str) {
    let d = std::fs::read(p).unwrap();
    let t = std::time::Instant::now();
    let f = aqr_pdb::formula::encode(&d, 1);
    println!("formula::encode {p}: {:?} in {:.2?}", f.map(|f| f.streams.len()), t.elapsed());
}

/// The TPI and IPI streams of a file through `pdb-types`: what each input is, and zstd -3 of it.
fn types(p: &str, dump: Option<&str>) {
    let d = std::fs::read(p).unwrap();
    let Some(([streams, _, _], spans)) = aqr_pdb::msf::encode_streams(&d) else { return };
    let name = std::path::Path::new(p).file_stem().unwrap().to_string_lossy().to_string();
    let t = std::time::Instant::now();
    let Some(e) = aqr_pdb::types::encode(&streams, &spans) else {
        println!("{name}: types not taken");
        return;
    };
    let enc = t.elapsed();
    let whole: usize = e.places.iter().map(|p| p.bytes.len()).sum();
    let inputs: Vec<&[u8]> = e.streams.iter().map(|v| &v[..]).collect();
    let t = std::time::Instant::now();
    let back = aqr_pdb::module::types(&e.params, &inputs, e.sizes.iter().sum()).unwrap();
    let dec = t.elapsed();
    let zo: usize = e.places.iter().map(|p| z(&streams[p.bytes.clone()])).sum();
    let zs: Vec<usize> = e.streams.iter().map(|s| z(s)).collect();
    println!(
        "{name}: {whole} B in {} streams, zstd -3 {zo} -> {} ({} B); encode {enc:.2?}, decode {dec:.2?}, {} B out",
        e.places.len(),
        zs.iter().sum::<usize>(),
        e.streams.iter().map(|s| s.len()).sum::<usize>(),
        back.len()
    );
    for (index, own) in [(2, aqr_pdb::types::Space::Type), (4, aqr_pdb::types::Space::Id)] {
        let Some(r) = spans.get(index).cloned().flatten() else { continue };
        for (kind, n, bytes, kept, by) in aqr_pdb::types::by_kind(&streams[r], own) {
            println!("  {index} {kind:04x}: {n} records {bytes} B, kept {kept}; streams {by:?}");
        }
    }
    println!("  sizes {:?}, kept records {}", e.sizes, e.streams[0].iter().filter(|&&c| c == 255).count());
    for ((n, s), z) in aqr_pdb::types::STREAM_NAMES.iter().zip(&e.streams).zip(&zs) {
        println!("    {n:6} {:>10} {:>10}", s.len(), z);
        if let Some(dir) = dump {
            std::fs::write(format!("{dir}/{name}.{n}"), s).unwrap();
        }
    }
}
