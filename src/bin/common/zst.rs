//! The recompression demo shared by the binaries: a DEFLATE stream is split into its data and
//! metadata, and stored as the metadata size (u32, little endian), the metadata, then the data
//! compressed by zstd.

use std::time::Instant;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Usage of `compress` and `decompress` for files ending in `ext`.
pub fn usage(bin: &str, ext: &str, what: &str) -> String {
    format!(
        "  {bin} compress [-o OUTPUT] [-z LEVEL] [-c] INPUT
      Recompress the raw DEFLATE stream INPUT {what} into OUTPUT (default INPUT{ext}):
      the metadata size (u32, little endian), the metadata, then the data compressed by
      zstd at LEVEL (default 19). -c checks the round trip.
  {bin} decompress [-o OUTPUT] INPUT
      Rebuild the DEFLATE stream from INPUT into OUTPUT (default INPUT without {ext}).
"
    )
}

/// Options (the last value of each) and a single positional argument; flags in `switches`
/// take no value.
fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Res<(Vec<(String, String)>, String)> {
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
    o.iter().rev().find(|(x, _)| x == k).map(|(_, v)| v.as_str())
}

fn rate(bytes: usize, secs: f64) -> String {
    format!("{:.1} MB/s", bytes as f64 / 1e6 / secs)
}

/// `compress`: `split` gives the data and the metadata of a stream, `join` the stream back.
pub fn compress(
    args: &[String],
    ext: &str,
    split: impl Fn(&[u8]) -> Res<(Vec<u8>, Vec<u8>)>,
    join: impl Fn(&[u8], &[u8]) -> Res<Vec<u8>>,
) -> Res<()> {
    let (o, input) = parse(args, &["-o", "-z"], &["-c"])?;
    let output = get(&o, "-o").map_or_else(|| format!("{input}{ext}"), str::to_owned);
    let level: i32 = get(&o, "-z").unwrap_or("19").parse()?;
    let raw = std::fs::read(&input)?;

    let t = Instant::now();
    let (data, meta) = split(&raw)?;
    let t_split = t.elapsed().as_secs_f64();
    if get(&o, "-c").is_some() && join(&data, &meta)? != raw {
        return Err("round trip failed".into());
    }
    let t = Instant::now();
    let packed = zstd::bulk::compress(&data, level)?;
    let t_zstd = t.elapsed().as_secs_f64();

    let mut out = Vec::with_capacity(4 + meta.len() + packed.len());
    out.extend_from_slice(&u32::try_from(meta.len())?.to_le_bytes());
    out.extend_from_slice(&meta);
    out.extend_from_slice(&packed);
    std::fs::write(&output, &out)?;
    eprintln!(
        "{input}: {} B deflate -> {} B data -> {} B metadata + {} B zstd = {} B ({:.2}%)",
        raw.len(),
        data.len(),
        meta.len(),
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

/// `decompress`: `join` gives the stream from its data and metadata.
pub fn decompress(
    args: &[String],
    ext: &str,
    join: impl Fn(&[u8], &[u8]) -> Res<Vec<u8>>,
) -> Res<()> {
    let (o, input) = parse(args, &["-o"], &[])?;
    let output = match get(&o, "-o") {
        Some(p) => p.to_owned(),
        None => input
            .strip_suffix(ext)
            .ok_or(format!("{input} does not end in {ext}; give -o"))?
            .to_owned(),
    };
    let file = std::fs::read(&input)?;
    let (len, rest) = file.split_first_chunk::<4>().ok_or("truncated file")?;
    let len = u32::from_le_bytes(*len) as usize;
    if rest.len() < len {
        return Err("truncated file".into());
    }
    let (meta, packed) = rest.split_at(len);

    let t = Instant::now();
    let data = zstd::stream::decode_all(packed)?;
    let t_zstd = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let raw = join(&data, meta)?;
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
