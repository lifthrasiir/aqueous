//! Which LZ4 streams in files the module rebuilds, what that is worth, and how fast: per kind of
//! container, files reproduced, zstd -3 of the files and of their data, and the module's speed.
//! Only these totals are printed, never names or contents.
//!
//! ```sh
//! cargo run --release -p aqr-lz4 --example check -- FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_formula::{Formula, Node};
use aqr_lz4::formula;

#[derive(Default)]
struct Totals {
    files: usize,
    reproduced: usize,
    bytes: usize,
    data: usize,
    zstd_file: usize,
    zstd_data: usize,
    /// Seconds of the module making the reproduced streams.
    secs: f64,
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

/// The application of `f`, and its data.
fn applied(f: &Formula) -> (&aqr_formula::Apply, &[u8]) {
    let mut found = None;
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            found = Some(&**a);
        }
    });
    let a = found.unwrap();
    let Node::Slice { stream, start, len } = a.inputs[0] else { unreachable!() };
    (a, &f.streams[stream].data[start as usize..(start + len) as usize])
}

fn main() {
    let mut paths = Vec::new();
    std::env::args().skip(1).for_each(|a| walk(Path::new(&a), &mut paths));
    let mut kinds: [(&str, Totals); 2] = [("mozlz4", Totals::default()), ("frame", Totals::default())];
    for p in paths {
        let Ok(d) = std::fs::read(&p) else { continue };
        if !formula::starts(&d) {
            continue;
        }
        let t = &mut kinds[usize::from(!d.starts_with(&formula::MOZLZ4))].1;
        t.files += 1;
        t.bytes += d.len();
        let zstd = |d: &[u8]| zstd::bulk::compress(d, 3).unwrap().len();
        let zf = zstd(&d);
        t.zstd_file += zf;
        let Some(f) = formula::encode(&d, 1) else {
            t.zstd_data += zf;
            continue;
        };
        t.reproduced += 1;
        let (a, data) = applied(&f);
        t.data += data.len();
        t.zstd_data += zstd(data) + (d.len() - a.len as usize);
        let start = Instant::now();
        formula::decode(&a.params, &[data], a.len, 1).unwrap();
        t.secs += start.elapsed().as_secs_f64();
    }
    println!(
        "{:7} {:>6} {:>10} {:>12} {:>12} {:>12} {:>7} {:>9}",
        "kind", "files", "reproduced", "bytes", "zstd", "zstd(data)", "gain", "MB/s"
    );
    for (name, t) in &kinds {
        if t.files == 0 {
            continue;
        }
        println!(
            "{name:7} {:6} {:10} {:12} {:12} {:12} {:6.1}% {:9.0}",
            t.files,
            t.reproduced,
            t.bytes,
            t.zstd_file,
            t.zstd_data,
            100.0 * (1.0 - t.zstd_data as f64 / t.zstd_file as f64),
            t.data as f64 / t.secs / 1e6,
        );
    }
}
