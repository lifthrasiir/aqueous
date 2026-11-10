//! Counts the binary decisions of the model by kind, with the bytes each would cost (the
//! adaptive bits' ideal code lengths) and the raw bits, over the first frames of each file.
//!
//! ```sh
//! cargo run --release -p aqr-prores --example aqr-prores-decisions -- [-n FRAMES] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};

use aqr_prores::frame::Frame;
use aqr_prores::model::{Coder, Model, Prob, kind};
use aqr_prores::plane::{Plane, planes};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

#[derive(Default)]
struct Count {
    k: usize,
    n: [u64; 9],
    bits: [f64; 9],
    raw: u64,
}

impl Coder for Count {
    const DECODE: bool = false;
    fn kind(&mut self, k: usize) {
        self.k = k;
    }
    fn bit(&mut self, p: &mut Prob, b: bool) -> bool {
        let p1 = p.p1() as f64 / 65536.0;
        self.n[self.k] += 1;
        self.bits[self.k] -= if b { p1 } else { 1.0 - p1 }.log2();
        p.update(b);
        b
    }
    fn raw(&mut self, v: u32, n: u32) -> u32 {
        self.raw += n as u64;
        v
    }
}

fn main() {
    let mut paths = Vec::new();
    let mut nframes = 20;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-n" => nframes = args.next().unwrap().parse().unwrap(),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let mut all = Count::default();
    let mut all_in = 0usize;
    for p in paths {
        let Ok(d) = std::fs::read(&p) else { continue };
        let at = aqr_prores::find(&d);
        let mut c = Count::default();
        let mut model = Model::new();
        let mut prev: Vec<[Plane; 3]> = Vec::new();
        let mut input = 0;
        for &a in at.iter().take(nframes) {
            let Ok(f) = Frame::parse(&d[a..]) else { continue };
            input += u32::from_be_bytes(d[a..a + 4].try_into().unwrap()) as usize;
            let mut cur = Vec::new();
            for (pi, pic) in f.pictures.iter().enumerate() {
                let mut pl = planes(&pic.slices, &f.header, &pic.widths(&f.header));
                for (k, q) in pl.iter_mut().enumerate() {
                    model.plane(&mut c, q, prev.get(pi).map(|x| &x[k]), (k > 0) as usize);
                }
                cur.push(pl);
            }
            prev = cur;
        }
        report(&p.file_name().unwrap().to_string_lossy(), &c, input);
        for i in 0..9 {
            all.n[i] += c.n[i];
            all.bits[i] += c.bits[i];
        }
        all.raw += c.raw;
        all_in += input;
    }
    report("all", &all, all_in);
}

fn report(name: &str, c: &Count, input: usize) {
    let tot: u64 = c.n.iter().sum();
    let bytes: f64 = c.bits.iter().sum::<f64>() / 8.0 + c.raw as f64 / 8.0;
    print!(
        "{name}: {:.1} MB in, ratio {:.4}, {:.2} decisions/byte:",
        input as f64 / 1e6,
        bytes / input as f64,
        tot as f64 / input as f64
    );
    for (i, nm) in kind::NAMES.iter().enumerate() {
        print!(" {nm} {:.2} ({:.3})", c.n[i] as f64 / input as f64, c.bits[i] / 8.0 / input as f64);
    }
    println!(" raw ({:.3})", c.raw as f64 / 8.0 / input as f64);
}
