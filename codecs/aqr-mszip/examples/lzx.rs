//! The LZX folders of CAB files: whether each is read and written back bit for bit, whether its
//! frames end where its data blocks do, and (with `-x DIR`, files extracted there by another
//! tool, by their names) whether the files' contents are those. With `-r`, each is split into
//! its data and record and rebuilt: the record's bytes, and speeds. Only totals are printed.
//!
//! ```sh
//! cargo run --release -p aqr-mszip --example aqr-mszip-lzx -- [-x DIR] [-r] FILE...
//! ```

use aqr_lzx::lzx::{self, Variant};
use aqr_mszip::{cab, chm};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.iter().position(|a| a == "-x").map(|i| {
        let d = args[i + 1].clone();
        args.drain(i..i + 2);
        d
    });
    let (mut folders, mut read, mut written, mut aligned, mut same, mut differ) = (0, 0, 0, 0, 0, 0);
    let (mut bytes, mut e8s) = (0usize, 0);
    let record = args.iter().any(|a| a == "-r");
    args.retain(|a| a != "-r");
    let (mut rebuilt, mut rec_bytes, mut data_bytes, mut split_secs, mut join_secs) = (0, 0usize, 0usize, 0.0, 0.0);
    for p in &args {
        let d = std::fs::read(p).unwrap();
        if let Some(c) = chm::parse(&d) {
            chm_probe(p, &d, &c, record, dir.as_deref());
            continue;
        }
        let Some(c) = cab::parse(&d) else { continue };
        for (i, f) in c.folders.iter().enumerate() {
            if f.method() != cab::LZX {
                continue;
            }
            folders += 1;
            let v = Variant::cab((f.type_compress >> 8 & 0x1f) as u8).unwrap();
            let src: Vec<u8> = f.blocks.iter().flat_map(|b| d[b.payload.clone()].to_vec()).collect();
            let Some((mut data, parse)) = lzx::read_as(&src, f.size(), v) else {
                println!("{p} folder {i}: not read");
                continue;
            };
            read += 1;
            bytes += src.len();
            if lzx::write_as(&data, &parse, v).as_deref() == Some(&src[..]) {
                written += 1;
            } else {
                println!("{p} folder {i}: written otherwise");
            }
            // the frames' ends: where the data blocks end
            let mut acc = 0;
            let blocks: Vec<usize> = f
                .blocks
                .iter()
                .map(|b| {
                    acc += b.payload.len();
                    acc
                })
                .collect();
            if lzx::frame_ends(&src, f.size(), v).as_deref() == Some(&blocks[..blocks.len() - 1]) {
                aligned += 1;
            }
            if record {
                let t = std::time::Instant::now();
                match aqr_lzx::recons::split_stream(&src, f.size(), v) {
                    Ok(sp) => {
                        split_secs += t.elapsed().as_secs_f64();
                        let t = std::time::Instant::now();
                        let mut out = Vec::new();
                        let ok = aqr_lzx::recons::join_stream_onto(&sp.data, &sp.record, v, &mut out).is_ok();
                        join_secs += t.elapsed().as_secs_f64();
                        if ok && out == src {
                            rebuilt += 1;
                            rec_bytes += sp.record.len();
                            data_bytes += sp.data.len();
                        } else {
                            println!("{p} folder {i}: rebuilt otherwise");
                        }
                    }
                    Err(e) => println!("{p} folder {i}: not split: {e}"),
                }
            }
            if let Some(e8) = parse.e8 {
                e8s += 1;
                lzx::e8_decode_frames(&mut data, e8);
            }
            if let Some(dir) = &dir {
                for file in c.files_of(i) {
                    let name = String::from_utf8_lossy(&file.name).replace('\\', "/");
                    let Ok(x) = std::fs::read(format!("{dir}/{name}")) else { continue };
                    let r = file.offset as usize..(file.offset + file.size) as usize;
                    if data.get(r) == Some(&x[..]) {
                        same += 1;
                    } else {
                        differ += 1;
                        if differ < 5 {
                            println!("{p} folder {i}: {name} differs");
                        }
                    }
                }
            }
        }
    }
    println!(
        "{folders} LZX folders, {read} read ({:.1} MB), {written} written back, {aligned} with frames ending where the data blocks do, {e8s} with e8; files the same {same}, otherwise {differ}",
        bytes as f64 / 1e6
    );
    if record {
        println!(
            "  rebuilt {rebuilt}: records {:.3} MB ({:.2}% of the streams), data {:.1} MB; split {:.1} MB/s, joined {:.1} MB/s of data",
            rec_bytes as f64 / 1e6,
            100.0 * rec_bytes as f64 / bytes.max(1) as f64,
            data_bytes as f64 / 1e6,
            data_bytes as f64 / 1e6 / split_secs,
            data_bytes as f64 / 1e6 / join_secs
        );
    }
}

/// A CHM file's LZX section: whether each reset interval reads alone (at the frames' starts the
/// reset table tells), and is written back, and with `record` split and rebuilt.
fn chm_probe(p: &str, d: &[u8], c: &chm::Chm, record: bool, dir: Option<&str>) {
    let Some(z) = &c.lzx else {
        println!("{p}: no LZX section taken");
        return;
    };
    let v = Variant::cab(z.window_bits).unwrap();
    let content = &d[z.content.clone()];
    let (mut ok, mut bad, mut rec, mut e8) = (0, 0, 0, 0);
    let mut section = Vec::new();
    let step = z.reset_frames;
    for (k, f) in (0..z.frames.len()).step_by(step).enumerate() {
        let start = z.frames[f];
        let end = z.frames.get(f + step).copied().unwrap_or(content.len());
        // (the last interval is of whole frames, past the section's data)
        let frames = (z.frames.len() - f).min(step);
        let n = frames * lzx::FRAME;
        let real = (z.len - f * lzx::FRAME).min(n);
        let Some(src) = content.get(start..end) else { return };
        match lzx::read_as(src, n, v) {
            Some((data, parse)) if lzx::write_as(&data, &parse, v).as_deref() == Some(src) => {
                ok += 1;
                section.extend_from_slice(&data[..real]);
                if real < n {
                    let tail = &data[real..];
                    let zeros = tail.iter().filter(|&&b| b == 0).count();
                    println!(
                        "{p}: last interval: {} bytes past the data, {zeros} of them zero; trailing {}",
                        tail.len(),
                        parse.trailing.len()
                    );
                }
                e8 += parse.e8.is_some() as usize;
                if record {
                    let sp = aqr_lzx::recons::split_stream(src, n, v).unwrap();
                    rec += sp.record.len();
                }
            }
            _ => {
                bad += 1;
                if bad < 3 {
                    let r = lzx::read_as(src, n, v);
                    let how = match &r {
                        None => {
                            // as far as it reads
                            let mut m = n;
                            while m > 0 && lzx::read_as(src, m, v).is_none() {
                                m -= 1;
                            }
                            let up = (1..=4 * lzx::FRAME).find(|&k| lzx::read_as(src, n + k, v).is_some());
                            let blocks = up.map(|k| {
                                let (_, parse) = lzx::read_as(src, n + k, v).unwrap();
                                (
                                    parse.blocks.iter().map(|b| (b.kind, b.size)).collect::<Vec<_>>(),
                                    parse.trailing.len(),
                                    parse.tail_bits,
                                )
                            });
                            format!("not read; reads {m} bytes, or {up:?} more: {blocks:?}")
                        }
                        Some((data, parse)) => {
                            let w = lzx::write_as(data, parse, v).unwrap_or_default();
                            let at = w.iter().zip(src).position(|(a, b)| a != b);
                            format!(
                                "written otherwise: {} bytes against {}, first difference at {at:?}, blocks {:?}, trailing {}",
                                w.len(),
                                src.len(),
                                parse.blocks.iter().map(|b| (b.kind, b.size)).collect::<Vec<_>>(),
                                parse.trailing.len()
                            )
                        }
                    };
                    println!("{p}: interval {k} of {n} bytes, {} compressed: {how}", src.len());
                }
            }
        }
    }
    if let Some(dir) = dir {
        let (mut same, mut differ) = (0, 0);
        for e in c.lzx_entries().filter(|e| e.len > 0) {
            let name = String::from_utf8_lossy(&e.name);
            let Ok(x) = std::fs::read(format!("{dir}/{}", name.trim_start_matches('/'))) else { continue };
            if section.get(e.offset as usize..(e.offset + e.len) as usize) == Some(&x[..]) {
                same += 1;
            } else {
                differ += 1;
            }
        }
        println!("{p}: files the same {same}, otherwise {differ}");
    }
    println!(
        "{p}: window 2^{}, {ok} intervals taken ({e8} with e8), {bad} not; records {rec} of {} bytes",
        z.window_bits,
        content.len()
    );
}
