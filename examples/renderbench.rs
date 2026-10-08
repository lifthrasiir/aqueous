// Composite rendering time alone, and exact-match share against the stored composite.
use psd_recons::composite::{Report, RowRenderer};
use std::time::Instant;

fn main() {
    for f in std::env::args().skip(1) {
        let d = std::fs::read(&f).unwrap();
        let psd = psd_recons::parse(&d).unwrap();
        let (w, h) = (psd.width as usize, psd.height as usize);
        let comp = psd_recons::pixels::composite(&psd, &d).ok();
        let mut best = f64::MAX;
        let mut exact = 0usize;
        for _ in 0..3 {
            let t = Instant::now();
            let mut r = RowRenderer::new(&psd, &d, &mut Report::default()).unwrap();
            let mut rows = vec![vec![0u8; w]; 3];
            exact = 0;
            let mut spent = 0.0;
            for y in 0..h {
                let t = Instant::now();
                let mut refs: Vec<&mut [u8]> = rows.iter_mut().map(|v| v.as_mut_slice()).collect();
                r.next_row(&mut refs).unwrap();
                spent += t.elapsed().as_secs_f64();
                if let Some(c) = &comp {
                    exact += (0..w).filter(|&x| (0..3).all(|k| c[k * w * h + y * w + x] == rows[k][x])).count();
                }
            }
            let _ = t;
            best = best.min(spent);
        }
        let layers = psd.layer_info.layers.len();
        println!("{f}\t{w}x{h}\t{layers} layers\t{:.3}s\t{:.1} ns/px/layer\texact {:.3}", best, best * 1e9 / (w * h * layers.max(1)) as f64, exact as f64 / (w * h) as f64);
    }
}
