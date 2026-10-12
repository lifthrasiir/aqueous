//! Where the bits of FLAC files go.
use flac_recons::{bits, flac};

fn main() {
    let mut t = [0f64; 9];
    let names = ["metadata", "headers", "verbatim", "warm+const", "coefs", "params", "residual", "junk", "total"];
    let mut types = [0usize; 64];
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let Some((si, start)) = flac::parse_metadata(&d) else { continue };
        let pd = bits::padded(&d);
        t[0] += start as f64 * 8.0;
        t[8] += d.len() as f64 * 8.0;
        let mut p = start;
        let mut f = flac::Frame::default();
        while p < d.len() {
            let Some(e) = flac::parse_frame(&pd, p, d.len(), si.bps, &mut f) else {
                p += 1;
                t[7] += 8.0;
                continue;
            };
            p = e;
            t[1] += (f.header.len() + 3) as f64 * 8.0 + 8.0 * f.channels as f64;
            for ch in 0..f.channels {
                let s = &f.sub[ch];
                types[s.typ as usize] += 1;
                let b = s.bits as f64;
                match s.typ {
                    0 => t[3] += b,
                    1 => t[2] += b * f.block_size as f64,
                    _ => {
                        t[3] += b * s.order as f64;
                        if s.typ >= 32 {
                            t[4] += 9.0 + (s.precision as usize * s.order as usize) as f64;
                        }
                        let ps = f.block_size >> s.porder;
                        for (i, &r) in f.res[ch].iter().enumerate() {
                            let k = s.params[(i + s.order as usize) / ps];
                            let u = ((r << 1) ^ (r >> 31)) as u32;
                            t[6] +=
                                if k & flac::ESCAPE != 0 { (k & 31) as f64 } else { ((u >> k) + 1 + k as u32) as f64 };
                        }
                        t[5] += 6.0 + s.params.len() as f64 * (4 + s.method) as f64;
                    }
                }
            }
        }
    }
    for (n, v) in names.iter().zip(&t) {
        println!("{n:12} {:14.0} B {:7.3}%", v / 8.0, v / t[8] * 100.0);
    }
    let tt: Vec<String> = types.iter().enumerate().filter(|x| *x.1 > 0).map(|(i, n)| format!("{i}:{n}")).collect();
    println!("subframe types: {}", tt.join(" "));
}
