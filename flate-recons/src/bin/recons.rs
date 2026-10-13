//! Recompression of raw DEFLATE streams into `.flate.aqr` files (FORMAT.md): the magic, the
//! record's profile, the size of the rest of the record, the rest of the record, then the
//! data compressed by zstd.

use std::process::ExitCode;
use std::time::Instant;

use flate_recons::{self, ADAPTIVE, LAMBDAS};

/// The first bytes of a file (`aQRflat3` in base64).
const MAGIC: [u8; 6] = [0x69, 0x04, 0x5f, 0x95, 0xab, 0x77];
/// The extension of recompressed files.
const EXT: &str = ".flate.aqr";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn usage() -> String {
    format!(
        "usage:
  flate-recons compress [-o OUTPUT] [-p PROFILE] [-z LEVEL] [-c] INPUT
      Recompress the raw DEFLATE stream INPUT into OUTPUT (default INPUT{EXT}), with the
      data compressed by zstd at LEVEL (default 19). -c checks the round trip.
      PROFILE 0-9 codes corrections with static codes and the rule catalog of speed penalty
      lambda = {LAMBDAS:?} (fastest first); {ADAPTIVE} (default) codes them adaptively, with
      lookahead and zlib emulation.
  flate-recons decompress [-o OUTPUT] INPUT
      Rebuild the DEFLATE stream from INPUT into OUTPUT (default INPUT without {EXT}).
"
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("compress") => compress(&args[1..]),
        Some("decompress") => decompress(&args[1..]),
        _ => {
            eprint!("{}", usage());
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flate-recons: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Options (the last value of each) and a single positional argument; flags in `switches`
/// take no value.
fn parse(
    args: &[String],
    with_value: &[&str],
    switches: &[&str],
) -> Res<(Vec<(String, String)>, String)> {
    let mut o = vec![];
    let mut path = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if with_value.contains(&a.as_str()) {
            let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
            o.push((a.clone(), v.clone()));
        } else if switches.contains(&a.as_str()) {
            o.push((a.clone(), String::new()));
        } else if a.starts_with('-') || path.is_some() {
            return Err(format!("unexpected argument {a}").into());
        } else {
            path = Some(a.clone());
        }
    }
    Ok((o, path.ok_or("missing input")?))
}

fn get<'a>(o: &'a [(String, String)], k: &str) -> Option<&'a str> {
    o.iter()
        .rev()
        .find(|(x, _)| x == k)
        .map(|(_, v)| v.as_str())
}

fn rate(bytes: usize, secs: f64) -> String {
    format!("{:.1} MB/s", bytes as f64 / 1e6 / secs)
}

fn compress(args: &[String]) -> Res<()> {
    let (o, input) = parse(args, &["-o", "-p", "-z"], &["-c"])?;
    let output = get(&o, "-o").map_or_else(|| format!("{input}{EXT}"), str::to_owned);
    let level: i32 = get(&o, "-z").unwrap_or("19").parse()?;
    let profile: u16 = match get(&o, "-p") {
        Some(p) => p.parse()?,
        None => ADAPTIVE,
    };
    let opts = flate_recons::Options::for_profile(profile)?;
    let raw = std::fs::read(&input)?;

    let t = Instant::now();
    let (data, record) = flate_recons::deconstruct_with(&raw, &opts)?;
    let t_split = t.elapsed().as_secs_f64();
    if get(&o, "-c").is_some() && flate_recons::reconstruct(&data, &record)? != raw {
        return Err("round trip failed".into());
    }
    let t = Instant::now();
    let packed = zstd::bulk::compress(&data, level)?;
    let t_zstd = t.elapsed().as_secs_f64();

    let (head, body) = record.split_at(2);
    let mut out = Vec::with_capacity(12 + record.len() + packed.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(head);
    out.extend_from_slice(&u32::try_from(body.len())?.to_le_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(&packed);
    std::fs::write(&output, &out)?;
    eprintln!(
        "{input}: {} B deflate -> {} B data -> {} B metadata + {} B zstd = {} B ({:.2}%), profile {profile}",
        raw.len(),
        data.len(),
        record.len(),
        packed.len(),
        out.len(),
        out.len() as f64 * 100.0 / raw.len().max(1) as f64
    );
    eprintln!(
        "split {t_split:.3} s ({}), zstd -{level} {t_zstd:.3} s ({})",
        rate(data.len(), t_split),
        rate(data.len(), t_zstd)
    );
    Ok(())
}

fn decompress(args: &[String]) -> Res<()> {
    let (o, input) = parse(args, &["-o"], &[])?;
    let output = match get(&o, "-o") {
        Some(p) => p.to_owned(),
        None => input
            .strip_suffix(EXT)
            .ok_or(format!("{input} does not end in {EXT}; give -o"))?
            .to_owned(),
    };
    let file = std::fs::read(&input)?;
    let rest = file
        .strip_prefix(&MAGIC)
        .ok_or("not a .flate.aqr file (bad magic)")?;
    let (head, rest) = rest.split_first_chunk::<6>().ok_or("truncated file")?;
    let len = u32::from_le_bytes(head[2..].try_into().unwrap()) as usize;
    if rest.len() < len {
        return Err("truncated file".into());
    }
    let (body, packed) = rest.split_at(len);
    let mut record = Vec::with_capacity(2 + len);
    record.extend_from_slice(&head[..2]);
    record.extend_from_slice(body);

    let t = Instant::now();
    let data = zstd::stream::decode_all(packed)?;
    let t_zstd = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let raw = flate_recons::reconstruct(&data, &record)?;
    let t_join = t.elapsed().as_secs_f64();
    std::fs::write(&output, &raw)?;
    eprintln!(
        "{input}: {} B -> {} B data -> {} B deflate; zstd {t_zstd:.3} s ({}), join {t_join:.3} s ({})",
        file.len(),
        data.len(),
        raw.len(),
        rate(data.len(), t_zstd),
        rate(data.len(), t_join)
    );
    Ok(())
}
