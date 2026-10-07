// Estimates the coded size of 8-bit RGB layer pixels under candidate models: static
// per-file tables per context, as png-recons codes them. Prints per file: bytes the layers'
// pixel data take in the PSD, and per model variant the estimate (tables included).
use png_recons::rans::{self, BitW};
use psd_recons::pixels;

fn med(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i16, b as i16, c as i16);
    let (lo, hi) = (a.min(b), a.max(b));
    (if c >= hi { lo } else if c <= lo { hi } else { a + b - c }) as u8
}

fn bucket(v: u32, n: usize) -> usize {
    ((32 - v.leading_zeros()) as usize).min(n - 1)
}

/// Signed residual magnitude.
fn mag(r: u8) -> u32 {
    (r as i8).unsigned_abs() as u32
}

struct Hist {
    t: Vec<Vec<u32>>,
}

impl Hist {
    fn new(n: usize) -> Self {
        Hist { t: vec![vec![0; 256]; n] }
    }
    fn bytes(&self) -> f64 {
        let mut bits = 0.0;
        let mut w = BitW::default();
        for t in &self.t {
            if t.iter().any(|&c| c > 0) {
                rans::write_table(&mut w, t);
                bits += rans::cost(t);
            }
        }
        bits / 8.0 + w.finish().len() as f64
    }
}

const NV: usize = 5;
const CTX: usize = 64;

/// Codes planes `p` (the color planes, then alpha if any) of a w x h layer into `hist`
/// under variant `v`.
fn code(v: usize, planes: &[Vec<u8>], alpha: Option<&[u8]>, w: usize, h: usize, hist: &mut Hist) {
    let n = w * h;
    let mut res = vec![vec![0u8; n]; planes.len()];
    for (k, p) in planes.iter().enumerate() {
        let is_alpha = alpha.is_some() && k == planes.len() - 1;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let mut a = if x > 0 { p[i - 1] } else if y > 0 { p[i - w] } else { 0 };
                let mut b = if y > 0 { p[i - w] } else { a };
                let mut c = if x > 0 && y > 0 { p[i - w - 1] } else { b };
                let d = if y > 0 && x + 1 < w { p[i - w + 1] } else { b };
                // Alpha-aware: for a visible pixel, replace transparent neighbors by a visible one.
                if v >= 4 && !is_alpha {
                    if let Some(al) = alpha {
                        if al[i] > 0 {
                            let vis = |j: Option<usize>| j.filter(|&j| al[j] > 0);
                            let (jw, jn, jnw) = (
                                vis((x > 0).then(|| i - 1)),
                                vis((y > 0).then(|| i - w)),
                                vis((x > 0 && y > 0).then(|| i - w - 1)),
                            );
                            let any = jw.or(jn).or(jnw).map(|j| p[j]);
                            if let Some(fb) = any {
                                a = jw.map_or(fb, |j| p[j]);
                                b = jn.map_or(fb, |j| p[j]);
                                c = jnw.map_or(fb, |j| p[j]);
                            }
                        }
                    }
                }
                let r = p[i].wrapping_sub(med(a, b, c));
                res[k][i] = r;
                let rw = if x > 0 { mag(res[k][i - 1]) } else { 0 };
                let rn = if y > 0 { mag(res[k][i - w]) } else { 0 };
                let rnw = if x > 0 && y > 0 { mag(res[k][i - w - 1]) } else { 0 };
                let rne = if y > 0 && x + 1 < w { mag(res[k][i - w + 1]) } else { 0 };
                let act = a.abs_diff(c) as u32 + b.abs_diff(c) as u32 + b.abs_diff(d) as u32;
                let transparent = !is_alpha && alpha.is_some_and(|al| al[i] == 0);
                // Color planes after the first are coded relative to the first's residual.
                let (sym, cross) = if k > 0 && !is_alpha {
                    (r.wrapping_sub(res[0][i]), mag(res[0][i]))
                } else {
                    (r, 0)
                };
                let ctx = match v {
                    0 => bucket(act, 12),
                    1 => bucket(rw + rn + (rnw + rne) / 2, 12),
                    2 => bucket(rw + rn + (rnw + rne) / 2, 8) * 4 + bucket(cross, 4),
                    _ => bucket(act + 2 * (rw + rn), 8) * 4 + bucket(cross, 4),
                };
                let kind = if is_alpha { 3 } else { k.min(2) };
                let t = ((kind * 2 + transparent as usize) * CTX) + ctx;
                hist.t[t][sym as usize] += 1;
            }
        }
    }
}

fn main() {
    let mut tot = (0usize, [0.0f64; NV]);
    for f in std::env::args().skip(1) {
        let d = std::fs::read(&f).unwrap();
        let Ok(psd) = psd_recons::parse(&d) else { continue };
        if psd.depth != 8 || psd.mode != 3 {
            continue;
        }
        let mut hists: Vec<Hist> = (0..NV).map(|_| Hist::new(8 * CTX)).collect();
        let mut packed = 0usize;
        for (li, l) in psd.layer_info.layers.iter().enumerate() {
            let find = |id: i16| l.channels.iter().position(|c| c.id == id);
            let (w, h) = (l.rect.width() as usize, l.rect.height() as usize);
            if w == 0 || h == 0 || [0, 1, 2].iter().any(|&id| find(id).is_none()) {
                continue;
            }
            let mut planes: Vec<Vec<u8>> = Vec::new();
            for id in [0, 1, 2, -1] {
                if let Some(j) = find(id) {
                    packed += l.channels[j].span.len();
                    planes.push(pixels::layer_channel(&psd, &d, li, j).unwrap());
                }
            }
            let alpha = (planes.len() == 4).then(|| planes[3].clone());
            for (v, hist) in hists.iter_mut().enumerate() {
                code(v, &planes, alpha.as_deref(), w, h, hist);
            }
            if std::env::var("LAYERS").is_ok() {
                let mut one = Hist::new(8 * CTX);
                code(4, &planes, alpha.as_deref(), w, h, &mut one);
                let per: Vec<String> = (0..4)
                    .map(|k| {
                        let mut h = Hist::new(0);
                        h.t = one.t[k * 2 * CTX..(k + 1) * 2 * CTX].to_vec();
                        format!("{:.0}", h.bytes())
                    })
                    .collect();
                println!("  L{li} {w}x{h} est {:.0} (R,G,B,A: {})", one.bytes(), per.join(","));
            }
        }
        if packed == 0 {
            continue;
        }
        let est: Vec<f64> = hists.iter().map(|h| h.bytes()).collect();
        println!("{f}\t{packed}\t{}", est.iter().map(|e| format!("{e:.0}")).collect::<Vec<_>>().join("\t"));
        tot.0 += packed;
        for v in 0..NV {
            tot.1[v] += est[v];
        }
    }
    eprintln!("total\t{}\t{:?}", tot.0, tot.1);
}
