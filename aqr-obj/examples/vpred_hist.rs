//! Which vertex predictors the encoder picks (v-pred stream symbols), per file.
use aqr_obj::block::*;

fn main() {
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut enc = Encoder::default();
        let mut s: [Vec<u8>; NSTREAMS] = std::array::from_fn(|_| Vec::new());
        let k: usize = std::env::var("KIND").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut h = [0u64; 32];
        for b in src.chunks(4 << 20) {
            enc.encode(b, &mut s);
            for &c in &s[S_PRED + k] {
                h[c as usize] += 1;
            }
        }
        let t: u64 = h.iter().sum::<u64>().max(1);
        let top: Vec<String> = {
            let mut v: Vec<(u64, usize)> = h.iter().enumerate().map(|(i, &c)| (c, i)).collect();
            v.sort_by(|a, b| b.cmp(a));
            v.iter().take(8).map(|&(c, i)| format!("{}:{:.0}%", i, 100.0 * c as f64 / t as f64)).collect()
        };
        println!("{:<55} {}", f, top.join(" "));
    }
}
