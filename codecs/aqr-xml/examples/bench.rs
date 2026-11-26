//! usage: aqr-xml-bench FILE...
//!
//! For each file: its skeleton, table and lens as zstd compresses them and each container on its
//! own (as aqr-compress recompresses them), against the file itself, the module's encoding and
//! decoding speeds (of the file's bytes, one thread), and that it decodes back (a segment at a
//! time). Environment: `LEVEL` (zstd's, 3), `TOP=1` (the 20 largest containers, by context).

use std::collections::HashMap;
use std::time::Instant;

use aqr_formula::{Formula, Node};

fn bytes(f: &Formula, n: &Node) -> Vec<u8> {
    match n {
        &Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| bytes(f, n)).collect(),
        _ => unreachable!(),
    }
}

fn main() {
    let level: i32 = std::env::var("LEVEL").ok().and_then(|l| l.parse().ok()).unwrap_or(3);
    let top = std::env::var("TOP").is_ok_and(|t| t != "0");
    let z = |b: &[u8]| zstd::bulk::compress(b, level).unwrap().len();
    let (mut total, mut packed, mut plain) = (0usize, 0usize, 0usize);
    let (mut te, mut td) = (0f64, 0f64);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let mut report = Vec::new();
        let t = Instant::now();
        let (f, containers) =
            aqr_xml::formula::encode_report(&d, &aqr_xml::formula::Limits::default(), Some(&mut report));
        let e = t.elapsed().as_secs_f64();
        let segments: Vec<&Node> = match &f.root {
            Node::Concat(ns) => ns.iter().collect(),
            n => vec![n],
        };
        let inputs: Vec<Vec<Vec<u8>>> = segments
            .iter()
            .map(|n| {
                let Node::Apply(a) = n else { unreachable!() };
                a.inputs.iter().map(|n| bytes(&f, n)).collect()
            })
            .collect();
        let t = Instant::now();
        let mut at = 0;
        for (n, ins) in segments.iter().zip(&inputs) {
            let Node::Apply(a) = n else { unreachable!() };
            let ins: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
            let out = aqr_xml::formula::decode(&a.params, &ins, a.len, 1).unwrap();
            assert!(d[at..].starts_with(&out), "{path}: not given back");
            at += out.len();
        }
        let dt = t.elapsed().as_secs_f64();
        assert!(at == d.len(), "{path}: not given back");
        let (mut skeleton, mut table, mut lens, mut values) = (0, 0, 0, 0);
        let mut by_context: HashMap<(u8, Vec<u8>, Vec<u8>), [usize; 3]> = HashMap::new();
        let (mut sep, mut all) = (0, 0);
        for (ins, ctx) in inputs.iter().zip(&report) {
            skeleton += z(&ins[0]);
            table += z(&ins[1]);
            lens += z(&ins[2]);
            values += ins[3].len();
            let (mut at, mut from) = (0, 0);
            for c in ctx {
                at += 1;
                let n = aqr_common::varint::get(&ins[1], &mut at).unwrap() as usize;
                let part = &ins[3][from..from + n];
                from += n;
                let zc = z(part);
                sep += zc;
                let e = by_context.entry((c.kind, c.a.clone(), c.b.clone())).or_default();
                *e = [e[0] + c.values, e[1] + n, e[2] + zc];
            }
            all += z(&ins[3]);
        }
        let sum = skeleton + table + lens + sep;
        let base = z(&d);
        let mb = d.len() as f64 / 1e6;
        println!(
            "{path}: {mb:.2} MB, {} segments, {containers} containers, zstd {base} -> {sum} B ({:.3}), encode {:.0} MB/s, decode {:.0} MB/s",
            segments.len(),
            sum as f64 / base as f64,
            mb / e,
            mb / dt
        );
        println!("  skeleton {skeleton}, table {table}, lens {lens}, values {values} -> {sep} apart, {all} together");
        if top {
            let mut v: Vec<_> = by_context.into_iter().collect();
            v.sort_by_key(|(_, s)| std::cmp::Reverse(s[1]));
            for ((kind, a, b), [n, bytes, zc]) in v.into_iter().take(20) {
                let (a, b) = (String::from_utf8_lossy(&a), String::from_utf8_lossy(&b));
                let name = if kind == 0 { format!("{a}/{b}") } else { format!("{a}@{b}") };
                println!("  {name:40} {n:10} values {bytes:11} B -> {zc:10}");
            }
        }
        (total, packed, plain, te, td) = (total + d.len(), packed + sum, plain + base, te + e, td + dt);
    }
    println!(
        "total: {:.2} MB, zstd {plain} -> {packed} B, encode {:.0} MB/s, decode {:.0} MB/s (one thread)",
        total as f64 / 1e6,
        total as f64 / 1e6 / te,
        total as f64 / 1e6 / td
    );
}
