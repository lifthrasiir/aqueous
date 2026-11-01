//! Finds the ProRes frames in files and checks that each is written again bit for bit from
//! its parse: frames found, rebuilt, failed (and why), what was kept as is (alpha, zero bytes
//! after the coefficients) and the thread seconds of parsing and of writing. With `--zstd`,
//! also zstd -3 of each frame as stored and of its parse: the headers, then the coefficients
//! by block (as parsed) or by scan position (each position across the frame's blocks).
//!
//! ```sh
//! cargo run --release -p aqr-prores --example aqr-prores-check -- [--zstd] FILE_OR_DIR...
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
    zstd: [usize; 3],
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
        (0..3).for_each(|k| self.zstd[k] += o.zstd[k]);
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
        if self.zstd[0] > 0 {
            let r = |n: usize| n as f64 / self.bytes as f64;
            println!(
                "    zstd -3: stored {:.4}, parse by block {:.4}, by scan position {:.4}",
                r(self.zstd[0]),
                r(self.zstd[1]),
                r(self.zstd[2])
            );
        }
        for (k, v) in &self.kinds {
            println!("    {k}: {v}");
        }
        for (k, v) in &self.failed {
            println!("    FAILED {k}: {v}");
        }
    }
}

fn zstd3(d: &[u8]) -> usize {
    zstd::bulk::compress(d, 3).unwrap().len()
}

/// The parse as bytes: headers, alpha and trailer, then the coefficients (little-endian i16) by
/// block or by scan position.
fn layouts(f: &Frame) -> [Vec<u8>; 2] {
    let mut head = f.header.raw.clone();
    let (mut by_block, mut by_pos) = (Vec::new(), Vec::new());
    for p in &f.pictures {
        head.extend_from_slice(&p.header);
        for s in &p.slices {
            head.extend_from_slice(&s.header);
            head.extend_from_slice(&s.alpha);
        }
        for k in 0..3 {
            for s in &p.slices {
                s.comps[k].0.iter().for_each(|v| by_block.extend_from_slice(&v.to_le_bytes()));
            }
            for i in 0..64 {
                for s in &p.slices {
                    s.comps[k].0.as_chunks::<64>().0.iter().for_each(|b| by_pos.extend_from_slice(&b[i].to_le_bytes()));
                }
            }
        }
    }
    head.extend_from_slice(&f.trailer);
    let mut a = head.clone();
    a.extend_from_slice(&by_block);
    head.extend_from_slice(&by_pos);
    [a, head]
}

fn check(d: &[u8], at: usize, zstd: bool) -> Stats {
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
    if zstd {
        let [a, b] = layouts(&f);
        s.zstd = [zstd3(orig), zstd3(&a), zstd3(&b)];
    }
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
    let mut zstd = false;
    for a in std::env::args().skip(1) {
        if a == "--zstd" {
            zstd = true;
        } else {
            walk(Path::new(&a), &mut paths);
        }
    }
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
            let s = check(&d, at, zstd);
            st.lock().unwrap().add(&s);
        });
        let st = st.into_inner().unwrap();
        st.print(&p.file_name().unwrap().to_string_lossy());
        all.add(&st);
    }
    all.print("all");
}
