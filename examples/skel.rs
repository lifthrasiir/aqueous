//! zstd levels on the skeletons (everything but stream data) of the given files: ratio and
//! ns per byte, compressing and decompressing.
fn main() {
    let mut skel = vec![];
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        let mut at = 0;
        let mut s = vec![];
        for st in pdf_recons::scan::streams(&buf) {
            s.extend_from_slice(&buf[at..st.data.start]);
            at = st.data.end;
        }
        s.extend_from_slice(&buf[at..]);
        skel.push(s);
    }
    let n: usize = skel.iter().map(Vec::len).sum();
    for l in [3, 6, 9, 12, 15, 17, 19] {
        let (mut z, mut tc, mut td) = (0, 0.0, 0.0);
        for s in &skel {
            let t = std::time::Instant::now();
            let c = zstd::bulk::compress(s, l).unwrap();
            tc += t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            let _ = zstd::bulk::decompress(&c, s.len()).unwrap();
            td += t.elapsed().as_secs_f64();
            z += c.len();
        }
        println!("level {l:>2}: {:.4}  {:6.1} / {:4.1} ns/B", z as f64 / n as f64, tc * 1e9 / n as f64, td * 1e9 / n as f64);
    }
}
