//! Measurements for fitting the cost model (src/budget.rs): per directory and budget, the
//! model's features and the times of deconstruction and reconstruction (one thread), as
//! CSV lines. Budgets as arguments (`-all`, or `-NS` ns per byte); ORDER and SKIP from the
//! environment, as for examples/prof.rs.
use std::time::Instant;

fn read_all(dir: &std::path::Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = format!("{prefix}{}", e.file_name().to_str().unwrap());
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            read_all(&e.path(), &format!("{p}/"), out);
        } else if ft.is_file() {
            out.push((p, std::fs::read(e.path()).unwrap()));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (budgets, dirs): (Vec<&String>, Vec<&String>) = args.iter().partition(|a| a.starts_with('-'));
    println!(
        "dir,budget,files,bytes,pack_bytes,loose_bytes,work_bytes,objects,emulated,emulated_bytes,emulated_zbytes,emulated_est,enc_s,dec_s"
    );
    for dir in dirs {
        let mut files = vec![];
        read_all(std::path::Path::new(dir), "", &mut files);
        for b in &budgets {
            let b = &b[1..];
            let ns = if b == "all" { None } else { Some(b.parse::<f64>().unwrap()) };
            let opt = git_recons::Options {
                ns_per_byte: ns,
                order: git_recons::budget::Order::from_env(),
                skip: std::env::var_os("SKIP").is_some(),
                ..Default::default()
            };
            let f = files.clone();
            let t = Instant::now();
            let mut sink = git_recons::enc::VecSink::default();
            let (_, st) = git_recons::enc::encode(f, &opt, &mut sink);
            let enc = t.elapsed().as_secs_f64();
            let streams = sink.finish();
            let t = Instant::now();
            let a = git_recons::Archive::open(&streams.each_ref().map(|s| &s[..])).unwrap();
            a.for_each(|_, _| Ok(())).unwrap();
            let dec = t.elapsed().as_secs_f64();
            let i = st.input;
            println!(
                "{dir},{b},{},{},{},{},{},{},{},{},{},{:.0},{enc:.4},{dec:.4}",
                i.files,
                i.bytes,
                i.pack_bytes,
                i.loose_bytes,
                i.work_bytes,
                i.objects,
                st.zlib_emulated,
                st.emulated_bytes,
                st.emulated_zbytes,
                st.emulated_est
            );
        }
    }
}
