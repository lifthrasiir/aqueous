//! Experiment: face indices with the current scheme (MTF cache 32, next-new, v offset,
//! escapes against the last index) plus, for v, the neighbours of the previous corner in
//! the last face that had it, and for vt/vn the index last paired with the corner's v.
//! Costs are adaptive order-0 symbol costs per slot plus rough escape bits.
const C: usize = 32;
const NS: usize = C + 6;
const NEW: usize = C;
const ESC: usize = C + 1;
const OFF: usize = C + 2;
const MAP: usize = C + 3;
const EPREV: usize = C + 4;
const ENEXT: usize = C + 5;
fn tb() -> usize { 1 << std::env::var("TB").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(20) }

fn lg(d: i64) -> f64 {
    (d.unsigned_abs() as f64 + 1.0).log2() * 1.1 + 1.0
}

struct Model {
    cnt: Vec<[u32; NS]>,
    tot: Vec<u32>,
    bits: f64,
    hist: [u64; NS],
}
impl Model {
    fn new(n: usize) -> Self {
        Model { cnt: vec![[1; NS]; n], tot: vec![NS as u32; n], bits: 0.0, hist: [0; NS] }
    }
    fn code(&mut self, ctx: usize, s: usize) {
        self.hist[s] += 1;
        self.bits -= (self.cnt[ctx][s] as f64 / self.tot[ctx] as f64).log2();
        self.cnt[ctx][s] += 32;
        self.tot[ctx] += 32;
        if self.tot[ctx] > 1 << 16 {
            self.tot[ctx] = 0;
            for c in self.cnt[ctx].iter_mut() {
                *c = (*c).div_ceil(2);
                self.tot[ctx] += *c;
            }
        }
    }
}

struct Attr {
    c: Vec<i64>,
    max: i64,
    last: i64,
    off: i64,
    m: Model,
    esc: f64,
}
impl Attr {
    fn new() -> Self {
        Attr { c: vec![], max: 0, last: 0, off: 0, m: Model::new(3), esc: 0.0 }
    }
    /// Codes `a` given candidate symbols checked first (in order).
    fn code(&mut self, slot: usize, a: i64, v: i64, cands: &[(usize, Option<i64>)]) {
        let p = self.c.iter().position(|&x| x == a);
        let mut sym = None;
        for &(s, c) in cands {
            if c == Some(a) {
                sym = Some(s);
                break;
            }
        }
        let sym = sym.unwrap_or_else(|| {
            if let Some(p) = p {
                p
            } else if a == self.max + 1 {
                NEW
            } else {
                self.esc += lg(a - self.last);
                ESC
            }
        });
        self.m.code(slot, sym);
        if let Some(p) = p {
            self.c.remove(p);
        }
        self.c.insert(0, a);
        self.c.truncate(C);
        self.max = self.max.max(a);
        self.last = a;
        self.off = a - v;
    }
}

fn main() {
    let use_edge = std::env::var("EDGE").map_or(true, |s| s != "0");
    let use_map = std::env::var("MAP").map_or(true, |s| s != "0");
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut attrs = [Attr::new(), Attr::new(), Attr::new()];
        // per v (direct mapped): tag, vertex before and after it in its last face, paired t, n
        let mut tab = vec![(0i64, 0i64, 0i64, 0i64, 0i64); tb()];
        let mut cnt = [0i64; 3];
        let mut nf = 0;
        for l in src.split(|&c| c == b'\n') {
            match l.get(..3) {
                Some(b"vt ") => cnt[1] += 1,
                Some(b"vn ") => cnt[2] += 1,
                Some([b'v', b' ', _]) => cnt[0] += 1,
                _ => {}
            }
            if !l.starts_with(b"f ") {
                continue;
            }
            nf += 1;
            let s = std::str::from_utf8(l).unwrap();
            let mut face: Vec<[Option<i64>; 3]> = vec![];
            for t in s.split_whitespace().skip(1) {
                let mut ix = [None; 3];
                for (k, p) in t.split('/').enumerate().take(3) {
                    if let Ok(a) = p.parse::<i64>() {
                        ix[k] = Some(if a < 0 { cnt[k] + 1 + a } else { a });
                    }
                }
                face.push(ix);
            }
            let mut pv: Option<i64> = None;
            for (j, ix) in face.iter().enumerate() {
                let slot = j.min(2);
                let Some(v) = ix[0] else { continue };
                let mut cands = vec![];
                if use_edge && let Some(p) = pv {
                    let e = tab[p as usize % tb()];
                    if e.0 == p {
                        cands.push((EPREV, Some(e.1)));
                        cands.push((ENEXT, Some(e.2)));
                    }
                }
                attrs[0].code(slot, v, v, &cands);
                let e = tab[v as usize % tb()];
                for k in 1..3 {
                    if let Some(a) = ix[k] {
                        let mut c = vec![(OFF, Some(v + attrs[k].off))];
                        if use_map && e.0 == v {
                            c.push((MAP, Some(if k == 1 { e.3 } else { e.4 })));
                        }
                        attrs[k].code(slot, a, v, &c);
                    }
                }
                pv = Some(v);
            }
            let n = face.len();
            for (j, ix) in face.iter().enumerate() {
                let Some(v) = ix[0] else { continue };
                let b = face[(j + n - 1) % n][0].unwrap_or(0);
                let a = face[(j + 1) % n][0].unwrap_or(0);
                tab[v as usize % tb()] = (v, b, a, ix[1].unwrap_or(0), ix[2].unwrap_or(0));
            }
        }
        let n = nf.max(1) as f64;
        print!("{nf:>9} faces");
        for (k, a) in attrs.iter().enumerate() {
            if a.m.hist.iter().sum::<u64>() > 0 {
                let tot: u64 = a.m.hist.iter().sum();
                let pct = |s: usize| 100.0 * a.m.hist[s] as f64 / tot as f64;
                print!(" | {}: {:5.1} b/f (esc {:4.1}; edge {:.0}% map {:.0}% off {:.0}% esc {:.0}%)", ["v", "t", "n"][k], (a.m.bits + a.esc) / n, a.esc / n, pct(EPREV) + pct(ENEXT), pct(MAP), pct(OFF), pct(ESC));
            }
        }
        println!("  {f}");
    }
}
