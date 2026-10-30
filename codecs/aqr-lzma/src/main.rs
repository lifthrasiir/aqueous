use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use aqr_lzma::{alone, lzma, lzma2, recons, xz};

#[derive(Default, Clone)]
struct Row {
    name: String,
    stream: usize,
    data: usize,
    record: usize,
    packets: usize,
    cost: [f64; recons::CATS],
    t_decode: f64,
    t_encode: f64,
    t_split: f64,
    t_join: f64,
    ok: bool,
    note: &'static str,
}

/// An `.xz` file: its blocks of LZMA2 alone, the rest counted as record.
fn run_xz(name: &str, f: &[u8]) -> Row {
    let mut r = Row { name: name.to_string(), stream: f.len(), ..Default::default() };
    let Some(st) = xz::parse(f) else {
        r.note = "not .xz";
        return r;
    };
    let mut taken = 0;
    for b in st.blocks {
        let s = &f[b.stream.clone()];
        let t = Instant::now();
        let Ok(dec) = lzma2::decode(s, 1 << 32) else {
            r.note = "corrupt";
            return r;
        };
        r.t_decode += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let n = dec.segments.len();
        for (i, seg) in dec.segments.iter().enumerate() {
            let last = (i + 1 == n) as usize;
            if lzma2::encode(&seg.data, &seg.chunks, &seg.packets).as_deref()
                != Some(&s[seg.span.start..seg.span.end - last])
            {
                r.note = "re-encoding differs";
                return r;
            }
        }
        r.t_encode += t.elapsed().as_secs_f64();
        for (i, seg) in dec.segments.iter().enumerate() {
            let t = Instant::now();
            let Some((record, cost)) = lzma2::deconstruct(seg, b.dict, i + 1 == n) else {
                r.note = "deconstruct failed";
                return r;
            };
            r.t_split += t.elapsed().as_secs_f64();
            (0..recons::CATS).for_each(|c| r.cost[c] += cost[c]);
            r.packets += seg.packets.len();
            r.data += seg.data.len();
            r.record += record.len();
            let t = Instant::now();
            let ok = lzma2::join(&seg.data, &record).as_deref() == Some(&s[seg.span.clone()]);
            r.t_join += t.elapsed().as_secs_f64();
            if !ok {
                r.note = "round trip failed";
                return r;
            }
        }
        taken += s.len();
    }
    r.record += f.len() - taken;
    r.ok = true;
    r
}

/// A `.lzma` file, or a raw stream as `FILE@OFFSET+LENGTH:PROPS:DICT:SIZE` (decimal; PROPS the
/// properties byte), as in a 7z folder.
fn run(arg: &str) -> Row {
    let mut r = Row { name: arg.to_string(), ..Default::default() };
    let (f, h, start, end) = if let Some((path, rest)) = arg.split_once('@') {
        let v: Vec<u64> = rest.split(['+', ':']).map(|x| x.parse().expect(arg)).collect();
        let f = std::fs::read(path).expect(path);
        let props = lzma::Props::from_byte(v[2] as u8).expect(arg);
        (f, alone::Header { props, dict: v[3] as u32, size: Some(v[4]) }, v[0] as usize, (v[0] + v[1]) as usize)
    } else {
        let f = std::fs::read(arg).expect(arg);
        if xz::starts(&f) {
            return run_xz(arg, &f);
        }
        let Some(h) = alone::parse(&f) else {
            r.note = "not .lzma";
            return r;
        };
        let n = f.len();
        (f, h, alone::HEADER, n)
    };
    let s = &f[start..end];
    r.stream = end - start + if start == alone::HEADER { alone::HEADER } else { 0 };
    let t = Instant::now();
    let Ok(dec) = lzma::decode(s, h.props, h.size, 1 << 32) else {
        r.note = "corrupt";
        return r;
    };
    r.t_decode = t.elapsed().as_secs_f64();
    r.data = dec.data.len();
    r.packets = dec.packets.len();
    let t = Instant::now();
    let again = lzma::encode(&dec.data, h.props, &dec.packets, dec.marker);
    r.t_encode = t.elapsed().as_secs_f64();
    if again[..] != s[..dec.used] || dec.used != s.len() {
        r.note = "re-encoding differs";
        return r;
    }
    let t = Instant::now();
    let Some((record, cost)) = recons::deconstruct(&dec.data, &dec.packets, h.dict, &[]) else {
        r.note = "deconstruct failed";
        return r;
    };
    r.t_split = t.elapsed().as_secs_f64();
    r.record = record.len() + recons::HEADER + if start == alone::HEADER { alone::HEADER } else { 0 };
    r.cost = cost;
    let t = Instant::now();
    drop(dec.packets);
    let rebuilt = recons::rebuild(&dec.data, &record, h.props, h.dict, dec.marker);
    r.t_join = t.elapsed().as_secs_f64();
    r.ok = rebuilt.as_deref() == Some(s);
    if !r.ok {
        r.note = "round trip failed";
    }
    r
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("bench") {
        eprintln!("usage: aqr-lzma bench [-t THREADS] [-v] FILE...   (.lzma or .xz files, or raw LZMA streams)");
        std::process::exit(2);
    }
    args.remove(0);
    let mut threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut verbose = false;
    while let Some(a) = args.first() {
        match a.as_str() {
            "-t" => {
                threads = args[1].parse().unwrap();
                args.drain(..2);
            }
            "-v" => {
                verbose = true;
                args.remove(0);
            }
            _ => break,
        }
    }
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(vec![]);
    let t = Instant::now();
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = args.get(i) else { break };
                    let r = run(p);
                    rows.lock().unwrap().push(r);
                }
            });
        }
    });
    let wall = t.elapsed().as_secs_f64();
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|r| std::cmp::Reverse(r.stream));
    let mut tot = Row::default();
    let mut failed = 0;
    for r in &rows {
        if verbose || !r.ok {
            println!(
                "{:>10} {:>10} {:>9} {:>8} {:5.2}% {:6.3}s {:6.3}s {} {}",
                r.stream,
                r.packets,
                r.data,
                r.record,
                100.0 * r.record as f64 / r.stream.max(1) as f64,
                r.t_split,
                r.t_join,
                r.name,
                r.note
            );
        }
        if !r.ok {
            failed += 1;
            continue;
        }
        tot.stream += r.stream;
        tot.data += r.data;
        tot.record += r.record;
        for c in 0..recons::CATS {
            tot.cost[c] += r.cost[c];
        }
        tot.t_decode += r.t_decode;
        tot.t_encode += r.t_encode;
        tot.t_split += r.t_split;
        tot.t_join += r.t_join;
    }
    let mb = |x: usize| x as f64 / 1e6;
    println!(
        "{} files ({} failed): streams {:.3} MB, data {:.3} MB, records {:.3} MB ({:.2}% of the streams, {:.3}% of the data)",
        rows.len(),
        failed,
        mb(tot.stream),
        mb(tot.data),
        mb(tot.record),
        100.0 * tot.record as f64 / tot.stream as f64,
        100.0 * tot.record as f64 / tot.data as f64
    );
    println!(
        "  record by what it codes: type {:.3} MB, length of matches {:.3} MB, of repeated ones {:.3} MB, distance {:.3} MB",
        tot.cost[0] / 8e6,
        tot.cost[1] / 8e6,
        tot.cost[3] / 8e6,
        tot.cost[2] / 8e6
    );
    let rate = |t: f64| mb(tot.data) / t;
    println!(
        "  CPU-s (MB/s of data per thread): decode {:.2} ({:.0}), encode {:.2} ({:.0}), deconstruct {:.2} ({:.0}), reconstruct + encode {:.2} ({:.0}); wall {:.2} s",
        tot.t_decode,
        rate(tot.t_decode),
        tot.t_encode,
        rate(tot.t_encode),
        tot.t_split,
        rate(tot.t_split),
        tot.t_join,
        rate(tot.t_join),
        wall
    );
}
