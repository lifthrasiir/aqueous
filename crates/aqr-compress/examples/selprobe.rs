//! Research tool: aqr-log's and aqr-xsv's formulas of a file or of samples of it, as the archive
//! encoder sizes them (and zstd -3 of their streams), for choosing between them.
//!
//! usage: aqr-compress-selprobe FILE...

use std::time::Instant;

use aqr_formula::Formula;
use aqr_formula::aqr::{Encoder, Level};

fn z(f: &Formula) -> usize {
    f.streams.iter().map(|s| zstd::bulk::compress(&s.data, 3).map_or(s.data.len(), |c| c.len())).sum()
}

fn arch(f: &Formula) -> usize {
    Encoder::new(Level::MID).encode(f, None).expect("encodes").len()
}

/// `n` bytes of `d` from `at`, at lines.
fn window(d: &[u8], at: usize, n: usize) -> &[u8] {
    let s = if at == 0 { 0 } else { d[at..].iter().position(|&b| b == b'\n').map_or(d.len(), |i| at + i + 1) };
    let e = (s + n).min(d.len());
    let e = if e < d.len() { d[..e].iter().rposition(|&b| b == b'\n').map_or(e, |i| i + 1) } else { e };
    &d[s..e.max(s)]
}

fn main() {
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let name = path.rsplit('/').next().unwrap();
        let m = 1 << 20;
        let k = 256 << 10;
        let spread = |parts: usize, each: usize| -> Vec<u8> {
            let mut v = Vec::new();
            for i in 0..parts {
                let at = if parts == 1 { 0 } else { (d.len().saturating_sub(each)) / (parts - 1) * i };
                v.extend_from_slice(window(&d, at, each));
            }
            v
        };
        let samples: Vec<(&str, Vec<u8>)> = vec![
            ("pre1M", window(&d, 0, m).to_vec()),
            ("pre4M", window(&d, 0, 4 * m).to_vec()),
            ("sp4x256K", spread(4, k)),
            ("sp8x256K", spread(8, k)),
            ("sp4x1M", spread(4, m)),
            ("whole", d.clone()),
        ];
        for (label, s) in samples {
            if label == "whole" && d.len() > 64 << 20 {
                continue;
            }
            let t = Instant::now();
            let l = aqr_log::formula::encode(&s, 1);
            let x = aqr_xsv::formula::encode(&s, 1);
            let te = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let (lz, xz) = (l.as_ref().map(z), x.as_ref().map(z));
            let tz = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let (la, xa) = (l.as_ref().map(arch), x.as_ref().map(arch));
            let ta = t.elapsed().as_secs_f64();
            println!(
                "{name:32} {label:9} z3 log {lz:?} xsv {xz:?} | arch log {la:?} xsv {xa:?} | enc {te:.2}s z {tz:.2}s arch {ta:.2}s"
            );
        }
    }
}
