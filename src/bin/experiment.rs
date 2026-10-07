//! Entropy estimates of token-domain models (research tool).
use flate_recons::deflate::{Body, Token};
use flate_recons::fast::inflate;
use png_recons::{filter, png};

struct H { n: usize, t: Vec<u32> }
impl H {
    fn new(ctx: usize, n: usize) -> H { H { n, t: vec![0; ctx * n] } }
    fn add(&mut self, c: usize, s: usize) { self.t[c * self.n + s] += 1; }
    /// Static entropy in bits plus a rough table cost.
    fn bits(&self) -> (f64, f64) {
        let mut e = 0.0; let mut tb = 0.0;
        for c in self.t.chunks(self.n) {
            let tot: u32 = c.iter().sum();
            if tot == 0 { continue; }
            let nz = c.iter().filter(|&&x| x > 0).count();
            tb += 8.0 + nz as f64 * 6.0f64.min((tot as f64).log2() + 1.0);
            for &x in c { if x > 0 { e += x as f64 * (tot as f64 / x as f64).log2(); } }
        }
        (e, tb)
    }
}
fn bk(v: u32) -> usize { match v { 0 => 0, 1 => 1, 2 => 2, 3..=4 => 3, 5..=8 => 4, 9..=16 => 5, 17..=32 => 6, 33..=64 => 7, _ => 8 } }
fn sm(x: u8) -> u32 { (x as i8).unsigned_abs() as u32 }
fn len_sym(l: u16) -> (usize, u32) { let i = flate_recons::huffman::len_code(l); (i, flate_recons::huffman::LEN_EXTRA[i] as u32) }
fn dist_sym(d: u16) -> (usize, u32) { let i = flate_recons::huffman::dist_code(d); (i, flate_recons::huffman::DIST_EXTRA[i] as u32) }

fn main() {
    let names = ["deflate.lit", "deflate.match", "deflate.other", "L1 ch", "L2 ch*fact", "L3 grad*act", "L4 grad+dec*act", "L5 L4*eact", "flag", "len", "dist", "dist+rep", "raw extra", "L6 L4*sum", "L7 med*sum", "flag*act", "len*act", "dist*len"];
    let mut tot = vec![0f64; names.len()];
    let mut tottab = vec![0f64; names.len()];
    let mut png_bytes = 0f64;
    for p in std::env::args().skip(1) {
        let file = std::fs::read(&p).unwrap();
        let Some((chunks, _)) = png::chunks(&file) else { continue };
        let Some(ih) = png::Ihdr::parse(chunks[0].data) else { continue };
        if ih.depth != 8 || ih.interlace != 0 { eprintln!("skip {p}"); continue; }
        let idat: Vec<u8> = chunks.iter().filter(|c| &c.kind == b"IDAT").flat_map(|c| c.data.iter().copied()).collect();
        let Ok(f) = inflate::parse(&idat[2..idat.len() - 4]) else { eprintln!("bad {p}"); continue };
        let bpp = ih.channels();
        let stride = ih.width as usize * bpp;
        let rs = stride + 1;
        let mut raw = vec![];
        if f.data.len() != rs * ih.height as usize || !filter::unfilter(&f.data, stride, bpp, &mut raw) { eprintln!("size {p}"); continue; }
        png_bytes += file.len() as f64;
        let w = ih.width as usize;
        // Raw-domain residuals (gradient, with decorrelation from the previous channel).
        let at = |x: isize, y: isize, c: usize| -> i32 { if x < 0 || y < 0 { 0 } else { raw[y as usize * stride + x as usize * bpp + c] as i32 } };
        let mut e3 = vec![0u8; raw.len()];
        let mut e4 = vec![0u8; raw.len()];
        let mut e7 = vec![0u8; raw.len()];
        let mut act = vec![0u32; raw.len()];
        for y in 0..ih.height as isize {
            for x in 0..w as isize {
                for c in 0..bpp {
                    let (a, b, cc) = (at(x - 1, y, c), at(x, y - 1, c), at(x - 1, y - 1, c));
                    let g = (a + b - cc).clamp(0, 255);
                    let v = at(x, y, c);
                    let i = y as usize * stride + x as usize * bpp + c;
                    e3[i] = (v - g) as u8;
                    let mut g4 = g;
                    if c > 0 && bpp >= 3 && c < 3 {
                        let (a1, b1, c1) = (at(x - 1, y, c - 1), at(x, y - 1, c - 1), at(x - 1, y - 1, c - 1));
                        let g1 = (a1 + b1 - c1).clamp(0, 255);
                        g4 = (g + at(x, y, c - 1) - g1).clamp(0, 255);
                    }
                    e4[i] = (v - g4) as u8;
                    let med = if cc >= a.max(b) { a.min(b) } else if cc <= a.min(b) { a.max(b) } else { a + b - cc };
                    let mut g7 = med;
                    if c > 0 && bpp >= 3 && c < 3 {
                        let (a1, b1, c1) = (at(x - 1, y, c - 1), at(x, y - 1, c - 1), at(x - 1, y - 1, c - 1));
                        let m1 = if c1 >= a1.max(b1) { a1.min(b1) } else if c1 <= a1.min(b1) { a1.max(b1) } else { a1 + b1 - c1 };
                        g7 = (med + at(x, y, c - 1) - m1).clamp(0, 255);
                    }
                    e7[i] = (v - g7) as u8;
                    act[i] = ((a - cc).abs() + (b - cc).abs()) as u32;
                }
            }
        }
        let mut hs: Vec<H> = vec![H::new(5 * bpp, 256), H::new(9 * bpp, 256), H::new(9 * bpp, 256), H::new(9 * bpp, 256), H::new(81 * bpp, 256), H::new(4, 2), H::new(1, 29), H::new(1, 30), H::new(1, 33), H::new(1,1), H::new(12 * bpp, 256), H::new(12 * bpp, 256), H::new(40, 2), H::new(10, 29), H::new(6, 30)];
        let mut d = [0f64; 18];
        let mut pos = 0usize;
        let mut prevm = 0usize; let mut prevm2 = 0usize;
        let mut lastd = [0u16; 2];
        for (bi, b) in f.stream.blocks.iter().enumerate() {
            let toks = &f.tokens[f.starts[bi]..f.starts[bi + 1]];
            let (ll, dl) = match &b.body {
                Body::Dynamic { header, .. } => (header.lit_lens().to_vec(), header.dist_lens().to_vec()),
                Body::Fixed { .. } => (flate_recons::huffman::fixed_lit_lens().to_vec(), vec![5u8; 30]),
                Body::Stored { len, .. } => { pos += *len as usize; d[2] += *len as f64 * 8.0; continue; }
            };
            for &t in toks {
                let r = pos / rs; let cpos = pos % rs;
                let fctx = prevm * 2 + prevm2;
                let ai = if cpos == 0 { 9 } else { bk(act[r * stride + cpos - 1]) };
                let fctx2 = prevm * 10 + ai + 20 * prevm2;
                match t {
                    Token::Lit(v) => {
                        d[0] += ll[v as usize] as f64;
                        hs[5].add(fctx, 0); hs[12].add(fctx2, 0);
                        if cpos == 0 {
                            hs[0].add(4 * bpp, v as usize);
                        } else {
                            let xb = cpos - 1; let ch = xb % bpp;
                            let ft = f.data[r * rs] as usize;
                            let _ = ft;
                            hs[0].add(ch, v as usize);
                            let fl = if xb >= bpp { sm(f.data[pos - bpp]) } else { 0 };
                            let fu = if r > 0 { sm(f.data[pos - rs]) } else { 0 };
                            hs[1].add(ch * 9 + bk(fl + fu), v as usize);
                            let i = r * stride + xb;
                            hs[2].add(ch * 9 + bk(act[i]), e3[i] as usize);
                            hs[3].add(ch * 9 + bk(act[i]), e4[i] as usize);
                            let el = if xb >= bpp { sm(e4[i - bpp]) } else { 0 };
                            let eu = if r > 0 { sm(e4[i - stride]) } else { 0 };
                            hs[4].add(ch * 81 + bk(act[i]) * 9 + bk(el + eu), e4[i] as usize);
                            let q = |v: u32| -> usize { (32 - v.leading_zeros()) as usize };
                            hs[10].add(ch * 12 + q(act[i] + 2 * (el + eu)).min(11), e4[i] as usize);
                            let el7 = if xb >= bpp { sm(e7[i - bpp]) } else { 0 };
                            let eu7 = if r > 0 { sm(e7[i - stride]) } else { 0 };
                            hs[11].add(ch * 12 + q(act[i] + 2 * (el7 + eu7)).min(11), e7[i] as usize);
                        }
                        prevm2 = prevm; prevm = 0;
                        pos += 1;
                    }
                    Token::Match { len, dist, alt258 } => {
                        let (ls, le) = len_sym(len);
                        let (ds, de) = dist_sym(dist);
                        d[1] += ll[257 + if alt258 { 27 } else { ls }] as f64 + le as f64 + dl[ds] as f64 + de as f64;
                        hs[5].add(fctx, 1); hs[12].add(fctx2, 1); hs[13].add(ai, ls);
                        let lb = match len { 3 => 0, 4 => 1, 5 => 2, 6..=8 => 3, 9..=32 => 4, _ => 5 }; hs[14].add(lb, ds); d[17] += de as f64;
                        hs[6].add(0, ls); hs[7].add(0, ds);
                        d[12] += le as f64;
                        let rep = if dist == lastd[0] { Some(30) } else if dist == lastd[1] { Some(31) } else if dist as usize == rs { Some(32) } else { None };
                        if let Some(s) = rep { hs[8].add(0, s) } else { hs[8].add(0, ds); d[11] += de as f64; }
                        d[10] += de as f64;
                        if dist != lastd[0] { lastd[1] = lastd[0]; lastd[0] = dist; }
                        prevm2 = prevm; prevm = 1;
                        pos += len as usize;
                    }
                }
            }
        }
        let total_bits = (idat.len() as f64) * 8.0;
        d[2] += total_bits - d[0] - d[1];
        let mut tab = [0f64; 18];
        for (k, h) in hs.iter().enumerate() { let (e, t) = h.bits(); d[3 + k] += e; tab[3 + k] = t; }
        for k in 0..18 { tot[k] += d[k] / 8.0; tottab[k] += tab[k] / 8.0; }
    }
    println!("png bytes {png_bytes:.0}");
    for k in 0..names.len() { println!("{:20} {:12.0} B  (+tables {:9.0})  {:6.2}% of png", names[k], tot[k], tottab[k], (tot[k] + tottab[k]) * 100.0 / png_bytes); }
}
