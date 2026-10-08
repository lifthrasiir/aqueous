// Counts layer channel data that duplicates an earlier channel byte for byte, per file, and
// how much of it duplicates a channel of another layer.
use std::collections::HashMap;
fn main() {
    let (mut tot, mut dup, mut other) = (0usize, 0usize, 0usize);
    for f in std::env::args().skip(1) {
        let d = std::fs::read(&f).unwrap();
        let Ok(p) = psd_recons::parse(&d) else { continue };
        let mut seen: HashMap<&[u8], usize> = HashMap::new();
        let (mut t, mut u) = (0, 0);
        for (li, l) in p.layer_info.layers.iter().enumerate() {
            for c in &l.channels {
                let s = &d[c.span.clone()];
                t += s.len();
                if s.len() > 64 {
                    if let Some(&src) = seen.get(s) {
                        u += s.len();
                        if src != li {
                            other += s.len();
                        }
                    } else {
                        seen.insert(s, li);
                    }
                }
            }
        }
        tot += t;
        dup += u;
        if u * 20 > t {
            println!("{f}\t{t}\t{u}");
        }
    }
    eprintln!("channel bytes {tot}, duplicates {dup} ({:.3}), of another layer {other} ({:.3})", dup as f64 / tot as f64, other as f64 / tot as f64);
}
