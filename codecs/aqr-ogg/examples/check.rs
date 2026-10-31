//! Ogg files encoded and decoded: per file the bytes, the streams of the formula by class, the
//! pages whose fields are not as predicted, and the time of both.
//!
//! cargo run -r -p aqr-ogg --example aqr-ogg-check -- FILE...

use std::collections::BTreeMap;
use std::time::Instant;

use aqr_formula::{Formula, Node};

fn eval(f: &Formula, n: &Node) -> Vec<u8> {
    match n {
        Node::Slice { stream, start, len } => f.streams[*stream].data[*start as usize..][..*len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| eval(f, n)).collect(),
        Node::Apply(a) => {
            let inputs: Vec<Vec<u8>> = a.inputs.iter().map(|n| eval(f, n)).collect();
            let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
            match a.module.name {
                "ogg-recons" => aqr_ogg::formula::decode(&a.params, &inputs, a.len).unwrap(),
                "vorbis-recons" => aqr_vorbis::formula::decode(&a.params, &inputs, a.len).unwrap(),
                m => panic!("{m}"),
            }
        }
        n => panic!("{n:?}"),
    }
}

fn main() {
    let (mut all, mut stored, mut enc, mut dec) = (0usize, 0usize, 0f64, 0f64);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let t = Instant::now();
        let Some(f) = aqr_ogg::formula::encode(&d, 1) else {
            println!("{path}: not taken");
            continue;
        };
        let e = t.elapsed().as_secs_f64();
        let t = Instant::now();
        assert_eq!(eval(&f, &f.root), d, "{path}");
        let x = t.elapsed().as_secs_f64();
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        f.streams.iter().for_each(|s| *by.entry(&s.class).or_default() += s.data.len());
        let sum: usize = by.values().sum();
        // the pages whose fields are not as predicted
        let Node::Apply(a) = &f.root else { unreachable!() };
        let l = aqr_ogg::layout::Layout::read(&eval(&f, &a.inputs[0]), a.len).unwrap();
        let odd = |g: fn(&aqr_ogg::layout::Page) -> bool| l.pages.iter().filter(|p| g(p)).count();
        println!(
            "{path}: {} -> {sum} ({:.2}%), {by:?}; {} pages, {} types, {} sequence numbers and {} granules not \
             predicted; {:.1} MB/s, {:.1} MB/s",
            d.len(),
            100.0 * sum as f64 / d.len() as f64,
            l.pages.len(),
            odd(|p| p.kind != 0),
            odd(|p| p.seq != 0),
            odd(|p| p.granule != 0),
            d.len() as f64 / e / 1e6,
            d.len() as f64 / x / 1e6
        );
        (all, stored, enc, dec) = (all + d.len(), stored + sum, enc + e, dec + x);
    }
    println!(
        "all: {all} -> {stored} ({:.2}%); {:.1} MB/s, {:.1} MB/s",
        100.0 * stored as f64 / all as f64,
        all as f64 / enc / 1e6,
        all as f64 / dec / 1e6
    );
}
