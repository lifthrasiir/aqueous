//! How RAR5's parses stand to the matches the data offers: per token, what the longest pair and
//! the repeated distances give, and what was taken. (An analysis, not a tool.)

use aqr_rar::rec::{Mf, lcp};
use aqr_rar::{lz5, rar5};
use std::collections::BTreeMap;

fn main() {
    let mut stats: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |k: String| *stats.entry(k).or_default() += 1;
    for p in std::env::args().skip(1) {
        let d = std::fs::read(&p).unwrap();
        let Some(a) = rar5::parse(&d) else { continue };
        for e in a.entries.iter().filter(|e| !e.service && e.method != 0 && !e.solid && !e.data.is_empty()) {
            let Some(size) = e.size else { continue };
            let mut dec = lz5::Decoder5::new(e.version == 1, e.dict);
            let Ok(st) = dec.file(&d[e.data.clone()], size as usize) else { continue };
            let u = &dec.out;
            let mut mf = Mf::new(u, 1 << 30);
            let (mut pos, mut cache, mut last) = (0usize, [0usize; 4], 0u32);
            let mut pairs = Vec::new();
            for t in &st.tokens {
                if let lz5::Token::Filter(_) = t {
                    continue;
                }
                mf.advance(pos);
                let c = cache;
                mf.pairs(
                    pos,
                    (u.len() - pos).min(4100),
                    |l, dd| l >= 2 + lz5::bonus(dd) as usize && !c.contains(&dd),
                    &mut pairs,
                );
                let longest = pairs.last().map_or(0, |p| p.0);
                let rl: Vec<usize> = cache
                    .iter()
                    .map(|&dd| if dd != 0 && dd <= pos { lcp(u, pos - dd, pos, 4100) } else { 0 })
                    .collect();
                let rbest = *rl.iter().max().unwrap();
                let lb = |l: usize| match l {
                    0 => "0",
                    1 => "1",
                    2 => "2",
                    3 => "3",
                    4..=5 => "4-5",
                    6..=9 => "6-9",
                    10..=31 => "10-31",
                    _ => "32+",
                };
                let (len, dist) = match *t {
                    lz5::Token::Lit(_) => {
                        add(format!("lit   longest {:>5} rep {:>5}", lb(longest), lb(rbest)));
                        pos += 1;
                        continue;
                    }
                    lz5::Token::Copy { len, dist } => {
                        cache.copy_within(0..3, 1);
                        cache[0] = dist;
                        last = len;
                        (len as usize, dist)
                    }
                    lz5::Token::Rep { idx, len } => {
                        let dd = cache[idx as usize];
                        cache.copy_within(0..idx as usize, 1);
                        cache[0] = dd;
                        last = len;
                        (len as usize, dd)
                    }
                    lz5::Token::Last => (last as usize, cache[0]),
                    _ => unreachable!(),
                };
                let kind = if c.contains(&dist) {
                    let i = c.iter().position(|&x| x == dist).unwrap();
                    let m = rl[i];
                    format!(
                        "rep{i} {} of {} (rbest {}, longest {})",
                        if len == m.min(4096) { "full" } else { "short" },
                        lb(m),
                        lb(rbest),
                        lb(longest)
                    )
                } else if let Some(j) = pairs.iter().rev().position(|&(l, dd)| dd == dist && l >= len) {
                    let top = pairs[pairs.len() - 1 - j].0;
                    format!(
                        "pair j{} {} (longest {}, rbest {})",
                        j.min(3),
                        if len == top.min(4096) { "full" } else { "short" },
                        lb(longest),
                        lb(rbest)
                    )
                } else {
                    format!("escape len {} (longest {}, rbest {})", lb(len), lb(longest), lb(rbest))
                };
                add(kind);
                pos += len;
            }
        }
    }
    let total: usize = stats.values().sum();
    let mut v: Vec<_> = stats.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (k, n) in v.iter().take(45) {
        println!("{:6.2}% {n:8} {k}", 100.0 * *n as f64 / total as f64);
    }
}
