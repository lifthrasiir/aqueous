//! Exploration: the cost of the Vorbis samples' packet values under aqr-vorbis's model (a model
//! per sample), by field, against their Huffman codes. `cmcost FILE...|-`
use std::sync::Mutex;
use std::time::Instant;

use aqr_fsb::fsb5::{CHUNK_VORBIS, Codec, Layout};
use aqr_fsb::setups;
use aqr_vorbis::bits::Reader;
use aqr_vorbis::cm::Cost;
use aqr_vorbis::model::Model;
use aqr_vorbis::packet::{self, Field, Source};
use aqr_vorbis::setup::{Ident, Setup};

const KINDS: [&str; 7] = ["header", "nonzero", "y01", "master", "y", "class", "vq"];

fn kind(f: Field) -> usize {
    match f {
        Field::Type | Field::Mode | Field::Window => 0,
        Field::Nonzero { .. } => 1,
        Field::Y01 { .. } => 2,
        Field::Master { .. } => 3,
        Field::Y { .. } => 4,
        Field::Class { .. } => 5,
        Field::Vq { .. } => 6,
    }
}

struct Rec<'a> {
    setup: &'a Setup,
    r: Reader<'a>,
    out: Vec<(Field, Option<usize>, u32, u32)>,
}

impl Source for Rec<'_> {
    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        let v = self.r.bits(n)?;
        self.out.push((f, None, v, n));
        Some(v)
    }
    fn entry(&mut self, f: Field, book: usize) -> Option<u32> {
        let p = self.r.pos();
        let e = self.setup.codebooks[book].read(&mut self.r)?;
        self.out.push((f, Some(book), e, (self.r.pos() - p) as u32));
        Some(e)
    }
}

#[derive(Default, Clone, Copy)]
struct Tot {
    huff: [f64; 7],
    cost: [f64; 7],
    tail: f64,
}

fn bank(path: &str, db: &setups::Setups) -> Tot {
    let d = std::fs::read(path).unwrap();
    let mut t = Tot::default();
    let Some(l) = Layout::of(&d).filter(|l| l.codec == Codec::Vorbis) else { return t };
    for (range, h) in l.samples.iter().zip(&l.headers) {
        let crc = u32::from_le_bytes(h.chunk(&d, CHUNK_VORBIS).unwrap()[..4].try_into().unwrap());
        let row = db.row(crc).unwrap();
        let s = Setup::parse(&db.assemble_row(row).unwrap(), Ident { channels: h.channels, blocksizes: [256, 2048] })
            .unwrap();
        let mut m = Model::new(&s);
        let data = &d[range.clone()];
        let mut at = 0;
        let mut costs = [Cost::default(); 7];
        while at + 2 <= data.len() {
            let n = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
            if n == 0 {
                break;
            }
            let p = &data[at + 2..at + 2 + n];
            at += 2 + n;
            let mut rec = Rec { setup: &s, r: Reader::new(p), out: Vec::new() };
            packet::walk(&s, &mut rec).unwrap();
            t.tail += (p.len() * 8 - rec.r.pos()) as f64;
            for &(f, book, v, bits) in &rec.out {
                let k = kind(f);
                t.huff[k] += bits as f64;
                let got = match book {
                    None => m.bits(&mut costs[k], f, bits, v),
                    Some(b) => m.entry(&mut costs[k], f, b, v),
                };
                assert_eq!(got, v);
            }
            m.end_packet();
        }
        for (t, c) in t.cost.iter_mut().zip(&costs) {
            *t += c.0;
        }
    }
    t
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files: Vec<String> = if args == ["-"] { std::io::stdin().lines().map(|l| l.unwrap()).collect() } else { args };
    let db = setups::database().unwrap();
    let start = Instant::now();
    let next = Mutex::new(0usize);
    let total = Mutex::new(Tot::default());
    std::thread::scope(|sc| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()) {
            sc.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        *n += 1;
                        *n - 1
                    };
                    let Some(f) = files.get(i) else { break };
                    let t = bank(f, db);
                    let mut g = total.lock().unwrap();
                    for k in 0..7 {
                        g.huff[k] += t.huff[k];
                        g.cost[k] += t.cost[k];
                    }
                    g.tail += t.tail;
                }
            });
        }
    });
    let t = total.into_inner().unwrap();
    let (h, c): (f64, f64) = (t.huff.iter().sum::<f64>() + t.tail, t.cost.iter().sum::<f64>() + t.tail);
    println!(
        "packets {:.3} MB -> {:.3} MB ({:.2}%), {:.1} s",
        h / 8e6,
        c / 8e6,
        100.0 * c / h,
        start.elapsed().as_secs_f64()
    );
    for (k, name) in KINDS.iter().enumerate() {
        println!(
            "  {:8} {:9.3} MB -> {:9.3} MB ({:6.2}%)",
            name,
            t.huff[k] / 8e6,
            t.cost[k] / 8e6,
            100.0 * t.cost[k] / t.huff[k].max(1.0)
        );
    }
}
