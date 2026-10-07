//! Separates the costs of decoding: model + Huffman re-encoding with replayed symbols, and
//! the rANS decoder alone on the same symbol stream.
use mp3_recons::model::Coder;
use std::time::Instant;

#[derive(Default)]
struct Rec {
    syms: Vec<(u32, u32)>,
    raws: Vec<(u32, u32)>,
}
impl Coder for Rec {
    fn sym(&mut self, ctx: usize, s: u32) -> u32 {
        self.syms.push((ctx as u32, s));
        s
    }
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        let v = if n == 0 { 0 } else { v & (u32::MAX >> (32 - n)) };
        self.raws.push((n, v));
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
        let v = self.r.syms[self.i].1;
        self.i += 1;
        v
    }
    #[inline(always)]
    fn raw(&mut self, _n: u32, _v: u32) -> u32 {
        let v = self.r.raws[self.j].1;
        self.j += 1;
        v
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut files = Vec::new();
    for a in &args {
        for e in std::fs::read_dir(a).unwrap().flatten() {
            files.push(std::fs::read(e.path()).unwrap());
        }
    }
    let tin: usize = files.iter().map(|d| d.len()).sum();
    let t = mp3_recons::codec::tables();
    let mut recs = Vec::new();
    let mut coded = Vec::new();
    for d in &files {
        let mut r = Rec::default();
        mp3_recons::codec::count(d, &mut r);
        recs.push(r);
        coded.push(mp3_recons::codec::encode(d));
    }
    let nsym: usize = recs.iter().map(|r| r.syms.len()).sum();
    // Model + Huffman with replayed symbols.
    let tm = Instant::now();
    for (r, c) in recs.iter().zip(&coded) {
        let mut p = Play { r, i: 0, j: 0 };
        std::hint::black_box(mp3_recons::codec::decode_with(c, &mut p).unwrap());
    }
    let tm = tm.elapsed().as_secs_f64();
    // rANS decode only, contexts from the record.
    let tr = Instant::now();
    let mut acc = 0u32;
    for (r, c) in recs.iter().zip(&coded) {
        let mut d = mp3_recons::rans::Decoder::new(t, &c.words);
        for &(ctx, _) in &r.syms {
            acc = acc.wrapping_add(d.get(ctx as usize));
        }
    }
    std::hint::black_box(acc);
    let tr = tr.elapsed().as_secs_f64();
    // rANS decode with the same symbols, but all contexts remapped to the most used
    // context of each alphabet size (cache-resident tables).
    {
        let mut best: std::collections::HashMap<usize, (usize, usize)> = Default::default();
        let mut cnt = vec![0usize; mp3_recons::model::N_CTX];
        for r in &recs {
            for &(c, _) in &r.syms {
                cnt[c as usize] += 1;
            }
        }
        for c in 0..cnt.len() {
            let n = mp3_recons::model::alphabet(c);
            let e = best.entry(n).or_insert((c, 0));
            if cnt[c] > e.1 {
                *e = (c, cnt[c]);
            }
        }
        let map: Vec<usize> = (0..cnt.len()).map(|c| best[&mp3_recons::model::alphabet(c)].0).collect();
        let r = &recs[0];
        let mut e = mp3_recons::rans::Encoder::with_capacity(r.syms.len() + 8);
        for &(c, s) in &r.syms {
            e.put(t, map[c as usize], s);
        }
        let w = e.finish(t);
        let tt = Instant::now();
        let mut d = mp3_recons::rans::Decoder::new(t, &w);
        let mut acc = 0u32;
        for &(c, _) in &r.syms {
            acc = acc.wrapping_add(d.get(map[c as usize]));
        }
        std::hint::black_box(acc);
        println!("rans, few contexts:     {:.1} ns/sym", tt.elapsed().as_secs_f64() * 1e9 / r.syms.len() as f64);
    }
    // Full decode.
    let tf = Instant::now();
    for c in &coded {
        std::hint::black_box(mp3_recons::codec::decode(c).unwrap());
    }
    let tf = tf.elapsed().as_secs_f64();
    println!("{nsym} symbols, {:.2} per input byte", nsym as f64 / tin as f64);
    println!("replayed model+huffman: {:.1} MB/s ({:.1} ns/sym)", tin as f64 / tm / 1e6, tm * 1e9 / nsym as f64);
    println!("rans alone:             {:.1} MB/s ({:.1} ns/sym)", tin as f64 / tr / 1e6, tr * 1e9 / nsym as f64);
    println!("full decode:            {:.1} MB/s ({:.1} ns/sym)", tin as f64 / tf / 1e6, tf * 1e9 / nsym as f64);
}

#[allow(dead_code)]
fn unused() {}
