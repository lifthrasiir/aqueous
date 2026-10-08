//! Experiment: exact duplicate v/vt/vn lines and the distance (in lines of the kind) to
//! their most recent copy, by log2 bucket.
use std::collections::HashMap;

fn main() {
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut last: [HashMap<&[u8], usize>; 3] = Default::default();
        let mut count = [0usize; 3];
        let mut hist = [[0usize; 33]; 3];
        for l in src.split(|&c| c == b'\n') {
            let k = match l.get(..3) {
                Some(b"vt ") => 1,
                Some(b"vn ") => 2,
                Some([b'v', b' ', _]) => 0,
                _ => continue,
            };
            let key = l.strip_suffix(b"\r").unwrap_or(l);
            if let Some(&p) = last[k].get(key) {
                hist[k][(usize::BITS - (count[k] - p).leading_zeros()) as usize] += 1;
            }
            last[k].insert(key, count[k]);
            count[k] += 1;
        }
        println!("== {f}");
        for k in 0..3 {
            if count[k] == 0 {
                continue;
            }
            let d: usize = hist[k].iter().sum();
            let mut cum = 0;
            let cs: Vec<String> = [4, 8, 12, 16, 20, 24]
                .iter()
                .map(|&b| {
                    cum = hist[k][..=b].iter().sum::<usize>();
                    format!("<=2^{b}: {:.0}%", 100.0 * cum as f64 / d.max(1) as f64)
                })
                .collect();
            println!("  {}: {} lines, {:.1}% duplicates; {}", ["v", "vt", "vn"][k], count[k], 100.0 * d as f64 / count[k] as f64, cs.join(" "));
        }
    }
}
