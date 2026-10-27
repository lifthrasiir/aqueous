use std::time::Instant;

use aqr_lzo::{lzo1x, lzop, recons};

#[derive(Default, Clone)]
struct Row {
    streams: usize,
    stream: usize,
    data: usize,
    record: usize,
    matches: usize,
    cost: [f64; recons::CATS],
    t_split: f64,
    t_join: f64,
    failed: usize,
}

impl Row {
    fn add(&mut self, o: &Row) {
        self.streams += o.streams;
        self.stream += o.stream;
        self.data += o.data;
        self.record += o.record;
        self.matches += o.matches;
        for i in 0..recons::CATS {
            self.cost[i] += o.cost[i];
        }
        self.t_split += o.t_split;
        self.t_join += o.t_join;
        self.failed += o.failed;
    }

    fn print(&self, name: &str) {
        let mb = |x: usize| x as f64 / 1e6;
        let c = self.cost.map(|x| x / 8.0 / self.record.max(1) as f64 * 100.0);
        println!(
            "{name}: {} streams ({} failed), {:.3} MB -> data {:.3} MB + record {:.4} MB ({:.2}% of the streams; \
             start {:.0}% len {:.0}% dist {:.0}% form {:.0}%), {} matches, split {:.0} MB/s, join {:.0} MB/s",
            self.streams,
            self.failed,
            mb(self.stream),
            mb(self.data),
            mb(self.record),
            self.record as f64 / self.stream.max(1) as f64 * 100.0,
            c[0],
            c[1],
            c[2],
            c[3],
            self.matches,
            mb(self.data) / self.t_split,
            mb(self.data) / self.t_join,
        );
    }
}

fn stream(s: &[u8], limit: usize) -> Row {
    let mut r = Row { streams: 1, stream: s.len(), ..Default::default() };
    let t = Instant::now();
    let sp = match recons::split(s, limit) {
        Ok(sp) if sp.used == s.len() => sp,
        _ => {
            r.failed = 1;
            return r;
        }
    };
    r.t_split = t.elapsed().as_secs_f64();
    r.data = sp.data.len();
    r.record = sp.record.len();
    r.cost = sp.cost;
    r.matches = lzo1x::decode(s, limit).map_or(0, |d| d.parse.matches.len());
    let t = Instant::now();
    let back = recons::join(&sp.data, &sp.record);
    r.t_join = t.elapsed().as_secs_f64();
    if back.as_deref() != Ok(s) {
        r.failed = 1;
    }
    r
}

/// An lzop file (its compressed blocks), or else a raw LZO1X stream.
fn file(path: &str) -> Row {
    let f = std::fs::read(path).expect(path);
    let mut row = Row::default();
    match lzop::parse(&f) {
        Some(lz) => {
            for b in lz.blocks.iter().filter(|b| !b.stored()) {
                row.add(&stream(&f[b.at..b.at + b.stream], b.data));
            }
        }
        None => row.add(&stream(&f, 1 << 30)),
    }
    row
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("bench") {
        eprintln!("usage: aqr-lzo bench [-v] FILE...   (lzop files, or raw LZO1X streams)");
        std::process::exit(2);
    }
    let verbose = args.iter().any(|a| a == "-v");
    let mut total = Row::default();
    for a in args[1..].iter().filter(|a| *a != "-v") {
        let r = file(a);
        if verbose {
            r.print(a);
        }
        total.add(&r);
    }
    total.print("total");
}
