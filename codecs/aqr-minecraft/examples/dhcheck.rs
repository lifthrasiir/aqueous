//! Distant Horizons' parents made of their children (`aqr_minecraft::dh`), compared blob by blob
//! with the database's: `dhcheck FILE [THREADS]`. The rows are decompressed as each parent needs
//! them (its children and itself), a few at a time on each thread.

use std::collections::HashMap;

use aqr_minecraft::dh::{self, Blob, Section, States, Work};

/// The columns of `FullData` that are the blobs, in the order of [`Blob::ALL`].
const COLUMNS: [u16; 8] = [5, 15, 16, 17, 18, 6, 7, 8];

/// The buffers of one parent's making: its children, their quadrants, itself and its blobs.
#[derive(Default)]
struct Bufs {
    w: Work,
    kid: Section,
    quads: [Section; 4],
    parent: Section,
    actual: Section,
    blobs: [Vec<u8>; 8],
}

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
    let read = |k: &[i64; 3], states: &States, w: &mut Work, out: &mut Section| {
        let r = raw(k);
        let b: [&[u8]; 8] = std::array::from_fn(|j| &r[j][..]);
        dh::section_of(&b, states, w, out).unwrap();
        r
    };
    let child = |p: &[i64; 3], q: usize| [p[0] - 1, 2 * p[1] + (q / 2) as i64, 2 * p[2] + (q % 2) as i64];
    let parents: Vec<[i64; 3]> =
        rows.keys().copied().filter(|p| p[0] > 0 && (0..4).all(|q| rows.contains_key(&child(p, q)))).collect();
    println!(
        "{} rows, {} parents of four children, {} states, in {:.1} s",
        rows.len(),
        parents.len(),
        states.len(),
        t.elapsed().as_secs_f64()
    );
    // the four quadrants of a parent into `b.quads`
    let quads = |p: &[i64; 3], states: &States, b: &mut Bufs| {
        for q in 0..4 {
            read(&child(p, q), states, &mut b.w, &mut b.kid);
            dh::quadrant(&b.kid, states, &mut b.w, &mut b.quads[q]).unwrap();
        }
    };
    let make = |b: &mut Bufs, states: &States| {
        let [q0, q1, q2, q3] = &b.quads;
        dh::parent([[q0, q1], [q2, q3]], states, &mut b.parent).unwrap();
    };
    // the states culled: of the merged columns' points, those whose tops are not of the parent's
    let t = std::time::Instant::now();
    let culled: Vec<Vec<u32>> = aqr_common::pool::par_map(parents.len(), threads, |k| {
        let p = &parents[k];
        let mut b = Bufs::default();
        read(p, &states, &mut b.w, &mut b.actual);
        quads(p, &states, &mut b);
        let mut out = Vec::new();
        for x in 0..dh::W {
            for z in 0..dh::W {
                let col = b.actual.grid.col(x * dh::W + z);
                for pt in b.quads[x / 32 * 2 + z / 32].grid.col((x % 32) * 32 + z % 32) {
                    let top = pt.bottom + pt.height as i32;
                    if pt.id != u32::MAX && !col.iter().any(|q| q.bottom + q.height as i32 == top) {
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
    // the phases of one thread over the first parents, its buffers kept
    {
        let n = parents.len().min(300);
        let (mut tp, mut tq, mut tpar, mut tb, mut td) = (0f64, 0f64, 0f64, 0f64, 0f64);
        let mut b = Bufs::default();
        for p in &parents[..n] {
            for q in 0..4 {
                let t = std::time::Instant::now();
                let r = raw(&child(p, q));
                td += t.elapsed().as_secs_f64();
                let t = std::time::Instant::now();
                let bl: [&[u8]; 8] = std::array::from_fn(|j| &r[j][..]);
                dh::section_of(&bl, &states, &mut b.w, &mut b.kid).unwrap();
                tp += t.elapsed().as_secs_f64();
                let t = std::time::Instant::now();
                dh::quadrant(&b.kid, &states, &mut b.w, &mut b.quads[q]).unwrap();
                tq += t.elapsed().as_secs_f64();
            }
            let t = std::time::Instant::now();
            make(&mut b, &states);
            tpar += t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            dh::blobs(&b.parent, &states, &mut b.w, &mut b.blobs);
            tb += t.elapsed().as_secs_f64();
        }
        let ms = |x: f64| x * 1000.0 / n as f64;
        println!(
            "a parent, one thread: decompress 4 children {:.2} ms, parse them {:.2} ms, quadrants {:.2} ms, parent {:.2} ms, blobs {:.2} ms",
            ms(td),
            ms(tp),
            ms(tq),
            ms(tpar),
            ms(tb)
        );
    }
    let t = std::time::Instant::now();
    let results: Vec<([bool; 8], usize)> = aqr_common::pool::par_map(parents.len(), threads, |k| {
        let p = &parents[k];
        let mut b = Bufs::default();
        let r = read(p, &states, &mut b.w, &mut b.actual);
        quads(p, &states, &mut b);
        // (each quadrant through its bytes: the module's as it holds or stores them)
        let mut st = states.clone();
        for q in 0..4 {
            let mut bytes = Vec::new();
            if dh::quadrant_bytes(&b.quads[q], &st, &mut b.w, &mut bytes) {
                dh::quadrant_of(&bytes, &mut st, &mut b.w, &mut b.kid).unwrap();
                std::mem::swap(&mut b.quads[q], &mut b.kid);
            }
        }
        make(&mut b, &states);
        let differ = (0..dh::COLUMNS).filter(|&i| b.parent.grid.col(i) != b.actual.grid.col(i)).count();
        let same = match dh::blobs(&b.parent, &states, &mut b.w, &mut b.blobs) {
            true => std::array::from_fn(|j| b.blobs[j] == r[j]),
            false => [false; 8],
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
    println!("  all: {all}; columns differing {cols} (of {differing} parents)");
}
