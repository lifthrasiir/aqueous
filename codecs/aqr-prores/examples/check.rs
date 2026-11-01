//! Finds the ProRes frames in files and checks that each is written again bit for bit from
//! its parse: frames found, rebuilt, failed (and why), what was kept as is (alpha, zero bytes
//! after the coefficients) and the thread seconds of parsing and of writing.
//!
//! ```sh
//! cargo run --release -p aqr-prores --example aqr-prores-check -- FILE_OR_DIR...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use aqr_prores::frame::Frame;
use rayon::prelude::*;

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
struct Stats {
    frames: usize,
    rebuilt: usize,
    bytes: usize,
    alpha: usize,
    zero_bytes: usize,
    trailer: usize,
    parse: f64,
    write: f64,
    failed: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.frames += o.frames;
        self.rebuilt += o.rebuilt;
        self.bytes += o.bytes;
        self.alpha += o.alpha;
        self.zero_bytes += o.zero_bytes;
        self.trailer += o.trailer;
        self.parse += o.parse;
        self.write += o.write;
        for (k, v) in &o.failed {
            *self.failed.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.kinds {
            *self.kinds.entry(k.clone()).or_default() += v;
        }
    }

    fn print(&self, name: &str) {
        let mb = self.bytes as f64 / 1e6;
        println!(
            "{name}: {}/{} frames rebuilt, {mb:.1} MB; alpha {:.1} MB, zero bytes {}, trailers {}; \
             parse {:.1} s ({:.0} MB/s), write {:.1} s ({:.0} MB/s)",
            self.rebuilt,
            self.frames,
            self.alpha as f64 / 1e6,
            self.zero_bytes,
            self.trailer,
            self.parse,
            mb / self.parse,
            self.write,
            mb / self.write,
        );
        for (k, v) in &self.kinds {
            println!("    {k}: {v}");
        }
        for (k, v) in &self.failed {
            println!("    FAILED {k}: {v}");
        }
    }
}

fn check(d: &[u8], at: usize) -> Stats {
    let mut s = Stats { frames: 1, ..Default::default() };
    let t = Instant::now();
    let f = match Frame::parse(&d[at..]) {
        Ok(f) => f,
        Err(e) => {
            s.parse = t.elapsed().as_secs_f64();
            s.failed.insert(format!("parse: {e}"), 1);
            return s;
        }
    };
    s.parse = t.elapsed().as_secs_f64();
    let h = &f.header;
    let kind = format!(
        "{} v{} {}x{} {} {}{}",
        String::from_utf8_lossy(&h.creator),
        h.version,
        h.width,
        h.height,
        if h.chroma == 3 { "4:4:4" } else { "4:2:2" },
        ["progressive", "tff", "bff", "?"][h.interlace as usize],
        ["", " alpha8", " alpha16"].get(h.alpha as usize).unwrap_or(&" alpha?"),
    );
    s.kinds.insert(kind, 1);
    for p in &f.pictures {
        for sl in &p.slices {
            s.alpha += sl.alpha.len();
            s.zero_bytes += sl.comps.iter().map(|c| c.1.zero_bytes as usize).sum::<usize>();
        }
    }
    s.trailer = f.trailer.len();
    let t = Instant::now();
    let out = f.write();
    s.write = t.elapsed().as_secs_f64();
    let orig = &d[at..at + out.len().min(d.len() - at)];
    s.bytes = orig.len();
    if out == orig {
        s.rebuilt = 1;
    } else {
        let i = out.iter().zip(orig).position(|(a, b)| a != b).unwrap_or(orig.len());
        s.failed.insert(format!("mismatch (first at {i} of {})", out.len()), 1);
    }
    s
}

fn main() {
    let mut paths = Vec::new();
    std::env::args().skip(1).for_each(|a| walk(Path::new(&a), &mut paths));
    let mut all = Stats::default();
    for p in paths {
        let Ok(file) = std::fs::File::open(&p) else { continue };
        // SAFETY: the corpus is not written while it is read.
        let Ok(d) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
        let frames = aqr_prores::find(&d);
        if frames.is_empty() {
            continue;
        }
        let st = Mutex::new(Stats::default());
        frames.par_iter().for_each(|&at| {
            let s = check(&d, at);
            st.lock().unwrap().add(&s);
        });
        let st = st.into_inner().unwrap();
        st.print(&p.file_name().unwrap().to_string_lossy());
        all.add(&st);
    }
    all.print("all");
}
