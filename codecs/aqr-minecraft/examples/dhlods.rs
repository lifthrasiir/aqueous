//! Distant Horizons' database through `dh-lods` and `zstd-frames`: `dhlods FILE [THREADS]` encodes
//! it, tells the streams, and makes the file again natively (both modules whole) and compares.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = std::fs::read(&args[0]).unwrap();
    let threads: usize = args.get(1).map_or(16, |t| t.parse().unwrap());
    let t = std::time::Instant::now();
    let blobs = aqr_sqlite::blobs::blobs(&f, 8, &|h| h.starts_with(&aqr_zstd::MAGIC)).unwrap();
    println!("blobs found in {:.1} s", t.elapsed().as_secs_f64());
    let Some(fm) = aqr_minecraft::lods_formula::encode(&f, &blobs, threads) else {
        println!("not taken");
        return;
    };
    println!("encoded in {:.1} s", t.elapsed().as_secs_f64());
    for s in &fm.streams {
        println!("  {}: {} bytes", s.class, s.data.len());
    }
    let aqr_formula::Node::Apply(z) = &fm.root else { panic!() };
    let aqr_formula::Node::Apply(l) = &z.inputs[3] else { panic!() };
    let slice = |n: &aqr_formula::Node| match *n {
        aqr_formula::Node::Slice { stream, start, len } => &fm.streams[stream].data[start as usize..][..len as usize],
        _ => panic!(),
    };
    let t = std::time::Instant::now();
    let li: Vec<&[u8]> = l.inputs.iter().map(slice).collect();
    let contents = aqr_minecraft::lods::decode(&l.params, &li, l.len, threads).unwrap();
    println!("contents made in {:.1} s", t.elapsed().as_secs_f64());
    let mut zi: Vec<&[u8]> = z.inputs[..3].iter().map(slice).collect();
    zi.push(&contents);
    let out = aqr_zstd::frames::decode(&z.params, &zi, z.len, threads).unwrap();
    println!("the same: {}", out == f);
}
