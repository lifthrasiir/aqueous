//! Bytes of streams by filter chain.
use std::collections::BTreeMap;
fn main() {
    let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in pdf_recons::scan::streams(&buf) {
            let n: Vec<String> = s
                .dict
                .get(b"Filter")
                .map(|f| f.names())
                .unwrap_or_default()
                .iter()
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .collect();
            let e = by.entry(n.join(",")).or_default();
            e.0 += 1;
            e.1 += s.data.len();
        }
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1.1));
    for (k, (n, b)) in v.iter().take(20) {
        println!("{n:>7} {b:>11}  {k}");
    }
}
