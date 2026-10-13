//! Face corner symbols per attribute (cache positions 0-31, new, escape, candidate 1, 2).
use obj_recons::block::*;

fn main() {
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut enc = Encoder::default();
        let mut s: [Vec<u8>; NSTREAMS] = std::array::from_fn(|_| Vec::new());
        let mut h = [[0u64; 36]; 3];
        for b in src.chunks(4 << 20) {
            enc.encode(b, &mut s);
            for k in 0..3 {
                for &c in &s[S_FSYM + k] {
                    h[k][(c as usize).min(35)] += 1;
                }
            }
        }
        println!("== {f}");
        for k in 0..3 {
            let t: u64 = h[k].iter().sum();
            if t == 0 {
                continue;
            }
            let pc = |a: u64| 100.0 * a as f64 / t as f64;
            let c: u64 = h[k][..32].iter().sum();
            println!(
                "  {}: cache {:.0}% (pos0-3 {:.0}%), new {:.0}%, esc {:.0}%, x1 {:.0}%, x2 {:.0}%",
                ["v", "vt", "vn"][k],
                pc(c),
                pc(h[k][..4].iter().sum()),
                pc(h[k][32]),
                pc(h[k][33]),
                pc(h[k][34]),
                pc(h[k][35])
            );
        }
    }
}
