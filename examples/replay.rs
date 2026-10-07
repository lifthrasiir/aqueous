//! Profiles the model + Huffman path of decoding with replayed symbols (no rANS).
use mp3_recons::model::Coder;

#[derive(Default)]
struct Rec {
    syms: Vec<u32>,
    raws: Vec<u32>,
}
impl Coder for Rec {
    fn sym(&mut self, _ctx: usize, s: u32) -> u32 {
        self.syms.push(s);
        s
    }
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        let v = if n == 0 { 0 } else { v & (u32::MAX >> (32 - n)) };
        self.raws.push(v);
        v
    }
}
struct Play<'a> {
    r: &'a Rec,
    i: usize,
    j: usize,
}
impl Coder for Play<'_> {
    #[inline(always)]
    fn sym(&mut self, _ctx: usize, _s: u32) -> u32 {
        let v = unsafe { *self.r.syms.get_unchecked(self.i) };
        self.i += 1;
        v
    }
    #[inline(always)]
    fn raw(&mut self, _n: u32, _v: u32) -> u32 {
        let v = unsafe { *self.r.raws.get_unchecked(self.j) };
        self.j += 1;
        v
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let iters: usize = args[0].parse().unwrap();
    let mut files = Vec::new();
    for a in &args[1..] {
        for e in std::fs::read_dir(a).unwrap().flatten() {
            files.push(std::fs::read(e.path()).unwrap());
        }
    }
    let tin: usize = files.iter().map(|d| d.len()).sum();
    let mut recs = Vec::new();
    let mut coded = Vec::new();
    for d in &files {
        let mut r = Rec::default();
        mp3_recons::codec::count(d, &mut r);
        recs.push(r);
        coded.push(mp3_recons::codec::encode(d));
    }
    let t = std::time::Instant::now();
    for _ in 0..iters {
        for (r, c) in recs.iter().zip(&coded) {
            let mut p = Play { r, i: 0, j: 0 };
            std::hint::black_box(mp3_recons::codec::decode_with(c, &mut p).unwrap());
        }
    }
    println!("replay: {:.1} MB/s", (tin * iters) as f64 / t.elapsed().as_secs_f64() / 1e6);
}
