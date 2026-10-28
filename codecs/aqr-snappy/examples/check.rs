//! Which LevelDB tables in files the module rebuilds, what that is worth, and how fast: tables
//! and blocks rebuilt or kept, zstd -3 of the tables and of the module's inputs, the hashes
//! found and the module's speed. Only these totals are printed, never names or contents.
//!
//! ```sh
//! cargo run --release -p aqr-snappy --example aqr-snappy-check -- FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_formula::Node;
use aqr_snappy::{formula, leveldb};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

fn main() {
    let mut paths = Vec::new();
    std::env::args().skip(1).for_each(|a| walk(Path::new(&a), &mut paths));
    let (mut tables, mut rebuilt, mut bytes, mut zstd_tables, mut zstd_inputs) = (0, 0, 0, 0, 0);
    let (mut snappy_blocks, mut kept_bytes, mut snappy_bytes) = (0, 0, 0);
    let (mut hashes, mut secs, mut made) = ([0; 2], 0.0, 0);
    let zstd = |d: &[u8]| zstd::bulk::compress(d, 3).unwrap().len();
    for p in paths {
        let Ok(d) = std::fs::read(&p) else { continue };
        let Ok(blocks) = leveldb::blocks(&d) else { continue };
        tables += 1;
        bytes += d.len();
        let zt = zstd(&d);
        zstd_tables += zt;
        let snappy: Vec<_> = blocks.iter().filter(|b| b.kind == leveldb::SNAPPY).collect();
        snappy_blocks += snappy.len();
        snappy_bytes += snappy.iter().map(|b| b.size).sum::<usize>();
        let Some(f) = formula::encode(&d, 1) else {
            zstd_inputs += zt;
            kept_bytes += snappy.iter().map(|b| b.size).sum::<usize>();
            continue;
        };
        rebuilt += 1;
        let Node::Apply(a) = &f.root else { unreachable!() };
        hashes[a.params[0] as usize] += 1;
        kept_bytes += f.streams[2].data.len();
        zstd_inputs += f.streams.iter().map(|s| zstd(&s.data)).sum::<usize>();
        let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let start = Instant::now();
        formula::decode_table(&a.params, &ins, a.len, 1).unwrap();
        secs += start.elapsed().as_secs_f64();
        made += d.len();
    }
    println!("tables {tables}, rebuilt {rebuilt} (hash mul {}, crc {})", hashes[0], hashes[1]);
    println!(
        "snappy blocks {snappy_blocks}, {snappy_bytes} bytes, of which kept as they are {kept_bytes} ({:.1}%)",
        100.0 * kept_bytes as f64 / snappy_bytes.max(1) as f64
    );
    println!(
        "{bytes} bytes; zstd -3 {zstd_tables} -> {zstd_inputs} ({:.1}%); module {:.0} MB/s of tables",
        100.0 * (zstd_inputs as f64 / zstd_tables.max(1) as f64 - 1.0),
        made as f64 / secs / 1e6
    );
}
