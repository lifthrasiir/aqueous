//! Distant Horizons' parents made of their children (`aqr_minecraft::dh`), compared blob by blob
//! with the database's: `dhcheck FILE [THREADS]`. The rows are decompressed as each parent needs
//! them (its children and itself), a few at a time on each thread.

use std::collections::HashMap;

use aqr_minecraft::dh::{self, Blob, Section, States};

/// The columns of `FullData` that are the blobs, in the order of [`Blob::ALL`].
const COLUMNS: [u16; 8] = [5, 15, 16, 17, 18, 6, 7, 8];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = std::fs::read(&args[0]).unwrap();
    let threads: usize = args.get(1).map_or(16, |t| t.parse().unwrap());
    let t = std::time::Instant::now();
    let blobs = aqr_sqlite::blobs::blobs(&f, 8, &|h| h.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])).unwrap();
    // the rows: (level, x, z) -> its blobs
    let mut rows: HashMap<[i64; 3], [Option<usize>; 8]> = HashMap::new();
    for (i, b) in blobs.iter().enumerate() {
        if let Some(k) = COLUMNS.iter().position(|&c| c == b.column) {
            rows.entry(b.key).or_default()[k] = Some(i);
        }
    }
    rows.retain(|_, b| b.iter().all(|b| b.is_some()));
    let content = |i: usize| zstd::bulk::decompress(&blobs[i].bytes(&f), 1 << 20).unwrap();
    let raw = |k: &[i64; 3]| rows[k].map(|b| content(b.unwrap()));
    // the states of all the mappings, one at a time
    let mut states = States::default();
    for b in rows.values() {
        let m = content(b[7].unwrap());
        for r in dh::mapping(&m).unwrap() {
            states.id(&m[r]);
        }
    }
    let section = |k: &[i64; 3], states: &States| {
        let r = raw(k);
        let b: [&[u8]; 8] = std::array::from_fn(|j| &r[j][..]);
        (dh::section_of(&b, states).unwrap(), r)
    };
    let child = |p: &[i64; 3], a: usize, b: usize| [p[0] - 1, 2 * p[1] + a as i64, 2 * p[2] + b as i64];
    let parents: Vec<[i64; 3]> = rows
        .keys()
        .copied()
        .filter(|p| p[0] > 0 && (0..4).all(|q| rows.contains_key(&child(p, q / 2, q % 2))))
        .collect();
    println!(
        "{} rows, {} parents of four children, {} states, in {:.1} s",
        rows.len(),
        parents.len(),
        states.len(),
        t.elapsed().as_secs_f64()
    );
    let quads = |p: &[i64; 3], states: &States| -> [[Section; 2]; 2] {
        std::array::from_fn(|a| std::array::from_fn(|b| dh::quadrant(&section(&child(p, a, b), states).0, states)))
    };
    // the states culled: of the merged columns' points, those whose tops are not of the parent's
    let t = std::time::Instant::now();
    let culled: Vec<Vec<u32>> = aqr_common::pool::par_map(parents.len(), threads, |k| {
        let p = &parents[k];
        let (s, _) = section(p, &states);
        let q = quads(p, &states);
        let mut out = Vec::new();
        for x in 0..dh::W {
            for z in 0..dh::W {
                let tops: Vec<i32> = s.cols[x * dh::W + z].iter().map(|q| q.bottom + q.height as i32).collect();
                for pt in &q[x / 32][z / 32].cols[(x % 32) * 32 + z % 32] {
                    if pt.id != u32::MAX && !tops.contains(&(pt.bottom + pt.height as i32)) {
                        out.push(pt.id);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    });
    for c in culled.iter().flatten() {
        states.opaque[*c as usize] = true;
    }
    println!("{} opaque, learned in {:.1} s", states.opaque.iter().filter(|&&o| o).count(), t.elapsed().as_secs_f64());
    let t = std::time::Instant::now();
    let results: Vec<([bool; 8], usize)> = aqr_common::pool::par_map(parents.len(), threads, |k| {
        let p = &parents[k];
        let (actual, r) = section(p, &states);
        let q = quads(p, &states);
        let s = dh::parent([[&q[0][0], &q[0][1]], [&q[1][0], &q[1][1]]], &states);
        let differ = (0..dh::COLUMNS).filter(|&i| s.cols[i] != actual.cols[i]).count();
        let same = match dh::blobs(&s, &states) {
            Some(b) => std::array::from_fn(|j| b[j] == r[j]),
            None => [false; 8],
        };
        (same, differ)
    });
    let mut same = [0usize; 8];
    let (mut all, mut cols, mut differing) = (0, 0, 0);
    for (s, d) in &results {
        for j in 0..8 {
            same[j] += s[j] as usize;
        }
        all += s.iter().all(|&x| x) as usize;
        cols += d;
        differing += (*d > 0) as usize;
    }
    println!("derived in {:.1} s", t.elapsed().as_secs_f64());
    for (j, b) in Blob::ALL.iter().enumerate() {
        println!("  {b:?}: {} of {} the same", same[j], parents.len());
    }
    println!("  all: {all}; columns differing {cols} (of {} parents)", differing);
}
