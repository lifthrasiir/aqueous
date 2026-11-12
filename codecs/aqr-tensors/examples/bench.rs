//! `aqr-tensors-bench FILE [MAX_BYTES]`: the formula of a file of tensors (or of its first
//! MAX_BYTES of tensor data, cut there), its streams' sizes, and the speed of making it and of decoding its
//! applications natively (on all threads, as the archive's decoder would, and on one).

use std::time::Instant;

use aqr_formula::Node;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let file = std::fs::File::open(path).unwrap();
    // SAFETY: the file is not changed while it is read
    let map = unsafe { memmap2::Mmap::map(&file) }.unwrap();
    let max: usize = args.get(2).map_or(usize::MAX, |s| s.parse().unwrap());
    let threads = std::env::var("THREADS")
        .map_or_else(|_| std::thread::available_parallelism().map_or(1, |n| n.get()), |s| s.parse().unwrap());
    let d = &map[..map.len().min(max)];
    let t = Instant::now();
    let Some(f) = aqr_tensors::formula::encode(d, threads) else {
        println!("{path}: not taken");
        return;
    };
    let enc = t.elapsed().as_secs_f64();
    let mut sizes = std::collections::BTreeMap::<String, u64>::new();
    for s in &f.streams {
        *sizes.entry(s.class.to_string()).or_default() += s.data.len() as u64;
    }
    let Node::Concat(nodes) = &f.root else { panic!() };
    let apps: Vec<_> = nodes.iter().filter_map(|n| if let Node::Apply(a) = n { Some(a) } else { None }).collect();
    let coded_out: u64 = apps.iter().map(|a| a.len).sum();
    let input = |n: &Node| match n {
        &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
        _ => panic!(),
    };
    let t = Instant::now();
    let ok = aqr_common::pool::par_map(apps.len(), threads, |i| {
        let a = apps[i];
        let ins: Vec<&[u8]> = a.inputs.iter().map(input).collect();
        aqr_tensors::module::decode(&a.params, &ins, a.len, 1).map(|o| o.len() as u64 == a.len)
    });
    let dec = t.elapsed().as_secs_f64();
    assert!(ok.iter().all(|r| *r == Ok(true)));
    let one = apps.iter().take(4).fold((0u64, 0f64), |(b, s), a| {
        let ins: Vec<&[u8]> = a.inputs.iter().map(input).collect();
        let t = Instant::now();
        let o = aqr_tensors::module::decode(&a.params, &ins, a.len, 1).unwrap();
        (b + o.len() as u64, s + t.elapsed().as_secs_f64())
    });
    // (the whole, rebuilt, against the file)
    let mut at = 0usize;
    for n in nodes {
        let o = match n {
            Node::Apply(a) => {
                let ins: Vec<&[u8]> = a.inputs.iter().map(input).collect();
                aqr_tensors::module::decode(&a.params, &ins, a.len, 1).unwrap()
            }
            n => input(n).to_vec(),
        };
        assert!(d[at..at + o.len()] == o[..], "differs at {at}");
        at += o.len();
    }
    assert_eq!(at, d.len());
    let mb = d.len() as f64 / 1e6;
    let coded: u64 =
        sizes.iter().filter(|(k, _)| k.as_str() != "gguf/header" && k.as_str() != "tensors/data").map(|(_, v)| v).sum();
    println!(
        "{path}: {mb:.1} MB, {} applications of {:.1} MB -> {:.1} MB ({:.4})",
        apps.len(),
        coded_out as f64 / 1e6,
        coded as f64 / 1e6,
        coded as f64 / coded_out.max(1) as f64
    );
    for (k, v) in &sizes {
        println!("  {k}: {:.2} MB", *v as f64 / 1e6);
    }
    // (the side bytes as the archive would have them: compressed, here by zstd at level 3, each
    // application's apart)
    let side: usize = (f.streams.iter())
        .filter(|s| s.class == "tensors/side")
        .map(|s| zstd::bulk::compress(&s.data, 3).unwrap().len().min(s.data.len()))
        .sum();
    let side_raw = sizes.get("tensors/side").copied().unwrap_or(0) as usize;
    let coded_z = coded - side_raw as u64 + side as u64;
    // (and the whole file so: its header and the bytes not coded at zstd's level 3 too)
    let plain: usize = (f.streams.iter())
        .filter(|s| s.class.ends_with("/header") || s.class == "tensors/data")
        .map(|s| zstd::bulk::compress(&s.data, 3).unwrap().len().min(s.data.len()))
        .sum();
    println!(
        "  file: {:.1} MB ({:.4})",
        (coded_z + plain as u64) as f64 / 1e6,
        (coded_z + plain as u64) as f64 / d.len() as f64
    );
    println!(
        "  side zstd -3: {:.2} MB; coded with it {:.1} MB ({:.4})",
        side as f64 / 1e6,
        coded_z as f64 / 1e6,
        coded_z as f64 / coded_out.max(1) as f64
    );
    println!(
        "  encode {:.0} MB/s, decode {:.0} MB/s ({threads} threads), one thread {:.0} MB/s",
        mb / enc,
        coded_out as f64 / 1e6 / dec,
        one.0 as f64 / 1e6 / one.1
    );
}
