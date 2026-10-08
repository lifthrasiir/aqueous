//! Experiment: face indices (v, vt, vn separately) by plain delta, by delta of the offset
//! from the v index, and through a move-to-front cache of recent indices with "next new
//! index" (max+1) and escapes coded against the last index; rough bit costs.
fn lg(d: i64) -> f64 {
    (d.unsigned_abs() as f64 + 1.0).log2() * 1.1 + 1.0
}

struct Cache {
    size: usize,
    c: Vec<i64>,
    max: i64,
    last: i64,
    cnt: Vec<Vec<u32>>,
    tot: [u32; 3],
    bits: f64,
}
impl Cache {
    fn new() -> Self {
        let size: usize = std::env::var("CACHE").ok().and_then(|s| s.parse().ok()).unwrap_or(32);
        Cache { size, c: vec![], max: 0, last: 0, cnt: vec![vec![1; size + 2]; 3], tot: [(size + 2) as u32; 3], bits: 0.0 }
    }
    fn code(&mut self, slot: usize, a: i64) {
        let sym = if let Some(p) = self.c.iter().position(|&x| x == a) {
            self.c.remove(p);
            p
        } else if a == self.max + 1 {
            self.size
        } else {
            self.bits += lg(a - self.last);
            self.size + 1
        };
        self.bits += -((self.cnt[slot][sym] as f64) / self.tot[slot] as f64).log2();
        self.cnt[slot][sym] += 32;
        self.tot[slot] += 32;
        if self.tot[slot] > 1 << 16 {
            self.tot[slot] = 0;
            for c in self.cnt[slot].iter_mut() {
                *c = (*c).div_ceil(2);
                self.tot[slot] += *c;
            }
        }
        self.c.insert(0, a);
        self.c.truncate(self.size);
        self.max = self.max.max(a);
        self.last = a;
    }
}

fn main() {
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut nf = 0;
        let mut cnt = [0i64; 3];
        let mut caches = [Cache::new(), Cache::new(), Cache::new()];
        let mut delta = [0f64; 3];
        let mut off = [0f64; 3];
        let mut last = [0i64; 3];
        let mut last_off = [0i64; 3];
        let mut has = [false; 3];
        for l in src.split(|&c| c == b'\n') {
            match l.get(..3) {
                Some(b"v  ") | Some([b'v', b' ', _]) => cnt[0] += 1,
                Some(b"vt ") => cnt[1] += 1,
                Some(b"vn ") => cnt[2] += 1,
                _ => {}
            }
            if !l.starts_with(b"f ") {
                continue;
            }
            nf += 1;
            let s = std::str::from_utf8(l).unwrap();
            for (j, t) in s.split_whitespace().skip(1).enumerate() {
                let mut ix = [0i64; 3];
                for (k, p) in t.split('/').enumerate().take(3) {
                    if p.is_empty() {
                        continue;
                    }
                    let a: i64 = p.parse().unwrap_or(0);
                    ix[k] = if a < 0 { cnt[k] + 1 + a } else { a };
                    has[k] = true;
                    delta[k] += lg(ix[k] - last[k]);
                    last[k] = ix[k];
                    caches[k].code(j.min(2), ix[k]);
                    if k > 0 {
                        let o = ix[k] - ix[0];
                        off[k] += lg(o - last_off[k]);
                        last_off[k] = o;
                    }
                }
            }
        }
        let n = nf.max(1) as f64;
        print!("{nf:>9} faces");
        for k in 0..3 {
            if has[k] {
                print!(" | {}: delta {:5.1} off {:5.1} cache {:5.1}", ["v", "t", "n"][k], delta[k] / n, if k > 0 { off[k] / n } else { f64::NAN }, caches[k].bits / n);
            }
        }
        println!("  {f}");
    }
}
