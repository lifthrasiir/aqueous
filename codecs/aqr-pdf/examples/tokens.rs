//! What coding the DEFLATE tokens themselves could gain: entropy of the tokens of every
//! FlateDecode stream under a few context models, per stream class, with adaptive counts
//! over the whole class of a file (no table cost) and static tables (rough table cost).
//!
//! `cargo run --release --example aqr-pdf-tokens -- FILE...`

use aqr_flate::deflate::Token;
use aqr_flate::huffman::{DIST_BASE, LEN_BASE};
use aqr_pdf::kind::{self, CLASSES, Class, Filter};
use aqr_pdf::scan;
use std::collections::{BTreeMap, HashMap};

fn len_sym(l: u16) -> usize {
    LEN_BASE.iter().rposition(|&b| b <= l).unwrap()
}
fn dist_sym(d: u16) -> usize {
    DIST_BASE.iter().rposition(|&b| b <= d).unwrap()
}

/// An adaptive frequency model per context (counts start at 1/32 per symbol): cost in bits.
#[derive(Default)]
struct Adaptive {
    m: HashMap<(u32, u32), u32>,
    tot: HashMap<u32, u32>,
    bits: f64,
    // Static: counts per context, to estimate table cost and static cost afterwards.
}

impl Adaptive {
    fn code(&mut self, ctx: u32, sym: u32, alpha: u32) {
        let c = self.m.entry((ctx, sym)).or_insert(0);
        let t = self.tot.entry(ctx).or_insert(0);
        let p = (*c as f64 * 32.0 + 1.0) / (*t as f64 * 32.0 + alpha as f64);
        self.bits -= p.log2();
        *c += 1;
        *t += 1;
    }
    /// Static cost with exact counts, plus about 8 bits per nonzero entry for the table.
    fn static_bits(&self) -> (f64, f64) {
        let mut b = 0.0;
        for (&(ctx, _), &c) in &self.m {
            b -= c as f64 * (c as f64 / self.tot[&ctx] as f64).log2();
        }
        (b, self.m.len() as f64 * 8.0)
    }
}

#[derive(Default)]
struct Models {
    deflate: f64,
    extra: f64,
    m: [Adaptive; 6],
}

fn cls(b: u8) -> u32 {
    match b {
        b'0'..=b'9' => 0,
        b'.' => 1,
        b'-' => 2,
        b' ' => 3,
        b'\n' | b'\r' => 4,
        b'a'..=b'z' => 5,
        b'A'..=b'Z' => 6,
        b'/' => 7,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' => 8,
        0 => 9,
        0x80..=0xff => 10,
        _ => 11,
    }
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let mut tot: BTreeMap<Class, Models> = BTreeMap::new();
    let mut dups = (0usize, 0usize);
    for f in &files {
        let buf = std::fs::read(f).unwrap();
        let mut per: BTreeMap<Class, Models> = BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for s in scan::streams(&buf) {
            if kind::filter(&s.dict) != Filter::Flate {
                continue;
            }
            let data = &buf[s.data.clone()];
            if !seen.insert(xxhash_rust::xxh3::xxh3_64(data)) {
                dups.0 += 1;
                dups.1 += data.len();
                continue;
            }
            let raw = if data.len() > 2 && data[0] & 0xf == 8 { &data[2..] } else { data };
            let Ok(flat) = aqr_flate::inflate::parse(raw) else { continue };
            let m = per.entry(kind::class(&s.dict)).or_default();
            m.deflate += data.len() as f64 * 8.0;
            let mut p = 0usize;
            let d = &flat.data;
            let mut prev_match = 0u32;
            let mut skips = flat.skips.iter().peekable();
            for (k, t) in flat.tokens.iter().enumerate() {
                while let Some(&&(at, len)) = skips.peek()
                    && at == k
                {
                    p += len;
                    skips.next();
                }
                let b1 = if p > 0 { d[p - 1] } else { 0 } as u32;
                let b2 = if p > 1 { d[p - 2] } else { 0 } as u32;
                let (sym, ds) = match *t {
                    Token::Lit(b) => (b as u32, None),
                    Token::Match { len, dist, .. } => (257 + len_sym(len) as u32, Some(dist_sym(dist) as u32)),
                };
                // 0: order 0; 1: order 1; 2: class of prev byte + after-match;
                // 3: order 1 + class of the byte before; 4: order 2 hashed to 16 bits.
                m.m[0].code(0, sym, 286);
                m.m[1].code(b1, sym, 286);
                m.m[2].code(cls(b1 as u8) * 2 + prev_match, sym, 286);
                m.m[3].code(b1 * 16 + cls(b2 as u8), sym, 286);
                m.m[4].code((b1 << 8 | b2) & 0xffff, sym, 286);
                if let Token::Match { len, dist, .. } = *t {
                    let ls = len_sym(len);
                    m.extra +=
                        (aqr_flate::huffman::LEN_EXTRA[ls] + aqr_flate::huffman::DIST_EXTRA[dist_sym(dist)]) as f64;
                    m.m[5].code(ls.min(8) as u32, ds.unwrap(), 30);
                    prev_match = 1;
                } else {
                    prev_match = 0;
                }
                p += t.span();
            }
        }
        for (c, m) in per {
            let t = tot.entry(c).or_default();
            t.deflate += m.deflate;
            t.extra += m.extra;
            for i in 0..6 {
                t.m[i].bits += m.m[i].bits;
                let (sb, tb) = m.m[i].static_bits();
                // Reuse the maps' slots for static totals: keep them in `tot` keyed by 0.
                *t.m[i].tot.entry(u32::MAX).or_insert(0) += 0;
                t.m[i].m.entry((u32::MAX, 0)).or_insert(0);
                *t.m[i].m.get_mut(&(u32::MAX, 0)).unwrap() += 0;
                STATIC.with(|s| {
                    let mut s = s.borrow_mut();
                    let e = s.entry((c, i)).or_insert((0.0, 0.0));
                    e.0 += sb;
                    e.1 += tb;
                });
            }
        }
    }
    println!("exact duplicate streams: {} ({} B)", dups.0, dups.1);
    println!("bytes; adaptive (static + table) per model; ratio to deflate of the best");
    for c in CLASSES {
        let Some(m) = tot.get(&c) else { continue };
        let distb = m.m[5].bits;
        print!(
            "{:<8} deflate {:>11.0} extra {:>10.0} dist {:>10.0}",
            c.name(),
            m.deflate / 8.0,
            m.extra / 8.0,
            distb / 8.0
        );
        for i in 0..5 {
            let (sb, tb) = STATIC.with(|s| s.borrow()[&(c, i)]);
            let a = (m.m[i].bits + m.extra + distb) / m.deflate;
            print!("  m{i} {:.3} ({:.3}+{:.3})", a, (sb + m.extra + distb) / m.deflate, tb / m.deflate);
        }
        println!();
    }
}

thread_local! {
    static STATIC: std::cell::RefCell<HashMap<(Class, usize), (f64, f64)>> = Default::default();
}
