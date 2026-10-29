//! `aqr-fsb vorbis FILE...` (or `-` for the files' names on standard input): the Vorbis samples of FSB5 banks parsed to the values their
//! packets code (aqr-vorbis), with their setups from the database, and written back; what the
//! packets' bits code, by field.

use std::time::Instant;

use aqr_fsb::fsb5::{CHUNK_VORBIS, Codec, Layout};
use aqr_fsb::setups;
use aqr_vorbis::bits::Reader;
use aqr_vorbis::packet::{self, Field, Source};
use aqr_vorbis::setup::{Ident, Setup};

/// The field kinds counted.
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

/// Reads a packet, counting the bits of each kind of field.
struct Count<'a> {
    setup: &'a Setup,
    r: Reader<'a>,
    bits: [u64; KINDS.len()],
    values: [u64; KINDS.len()],
}

impl Source for Count<'_> {
    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        let v = self.r.bits(n)?;
        self.bits[kind(f)] += n as u64;
        self.values[kind(f)] += 1;
        Some(v)
    }

    fn entry(&mut self, f: Field, book: usize) -> Option<u32> {
        let start = self.r.pos();
        let e = self.setup.codebooks[book].read(&mut self.r)?;
        self.bits[kind(f)] += (self.r.pos() - start) as u64;
        self.values[kind(f)] += 1;
        Some(e)
    }
}

#[derive(Default)]
struct Stats {
    record: u64,
    coded: u64,
    t_encode: f64,
    t_decode: f64,
    t_parse: f64,
    samples: usize,
    no_setup: usize,
    failed: usize,
    packets: usize,
    short: usize,
    nonzero_tails: usize,
    bytes: u64,
    tail_bits: u64,
    bits: [u64; KINDS.len()],
    values: [u64; KINDS.len()],
}

fn sample(s: &mut Stats, setup: &Setup, data: &[u8]) -> Result<(), String> {
    let t = Instant::now();
    let (used, record) = aqr_vorbis::codec::encode(setup, data).ok_or("not encoded")?;
    s.t_encode += t.elapsed().as_secs_f64();
    let t = Instant::now();
    let back = aqr_vorbis::codec::decode(setup, &record, used).map_err(|e| e.to_string())?;
    s.t_decode += t.elapsed().as_secs_f64();
    if back != data[..used] {
        return Err("record not decoded back".into());
    }
    s.record += record.len() as u64;
    s.coded += used as u64;
    let mut at = 0;
    while at + 2 <= data.len() {
        let n = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        if n == 0 {
            break;
        }
        let p = data.get(at + 2..at + 2 + n).ok_or("packet past the sample")?;
        at += 2 + n;
        let t = Instant::now();
        let parsed = packet::parse(setup, p).map_err(|e| e.to_string())?;
        s.t_parse += t.elapsed().as_secs_f64();
        let back = packet::write(setup, &parsed).map_err(|e| e.to_string())?;
        if back != p {
            return Err("packet not written back".into());
        }
        let mut c = Count { setup, r: Reader::new(p), bits: [0; KINDS.len()], values: [0; KINDS.len()] };
        packet::walk(setup, &mut c).map_err(|e| e.to_string())?;
        (0..KINDS.len()).for_each(|k| {
            s.bits[k] += c.bits[k];
            s.values[k] += c.values[k];
        });
        s.packets += 1;
        s.short += parsed.short as usize;
        s.nonzero_tails += (!parsed.short && parsed.tail.iter().any(|&b| b != 0)) as usize;
        s.tail_bits += parsed.tail.len() as u64;
        s.bytes += n as u64 + 2;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, files)) = args.split_first().filter(|(c, _)| *c == "vorbis") else {
        eprintln!("usage: aqr-fsb vorbis FILE... | -");
        std::process::exit(2);
    };
    let _ = cmd;
    let files: Vec<String> =
        if files == ["-"] { std::io::stdin().lines().map(|l| l.expect("a name")).collect() } else { files.to_vec() };
    let db = setups::database().expect("the database");
    let mut s = Stats::default();
    let t = Instant::now();
    for f in &files {
        let d = std::fs::read(f).unwrap_or_else(|e| panic!("{f}: {e}"));
        let Some(l) = Layout::of(&d).filter(|l| l.codec == Codec::Vorbis) else { continue };
        for (i, (range, h)) in l.samples.iter().zip(&l.headers).enumerate() {
            s.samples += 1;
            let crc =
                h.chunk(&d, CHUNK_VORBIS).and_then(|c| c.get(..4)).map(|c| u32::from_le_bytes(c.try_into().unwrap()));
            let Some(row) = crc.and_then(|crc| db.row(crc)) else {
                s.no_setup += 1;
                continue;
            };
            let bytes = db.assemble_row(row).expect("a row assembles");
            let setup = Setup::parse(&bytes, Ident { channels: h.channels, blocksizes: [256, 2048] });
            let r = setup.map_err(|e| e.to_string()).and_then(|setup| sample(&mut s, &setup, &d[range.clone()]));
            if let Err(e) = r {
                s.failed += 1;
                eprintln!("{f} sample {i}: {e}");
            }
        }
    }
    let total: u64 = s.bits.iter().sum::<u64>() + s.tail_bits;
    println!(
        "{} samples ({} without a setup, {} failed), {} packets ({} short, {} with nonzero padding), {:.3} MB in {:.2} s",
        s.samples,
        s.no_setup,
        s.failed,
        s.packets,
        s.short,
        s.nonzero_tails,
        s.bytes as f64 / 1e6,
        t.elapsed().as_secs_f64()
    );
    for (k, name) in KINDS.iter().enumerate() {
        println!(
            "  {:8} {:6.2}% of packet bits, {:12} values, {:.2} bits each",
            name,
            100.0 * s.bits[k] as f64 / total as f64,
            s.values[k],
            s.bits[k] as f64 / s.values[k].max(1) as f64
        );
    }
    println!(
        "  record: {:.3} MB of {:.3} MB of packets ({:.2}%); encoding {:.1} s, decoding {:.1} s, parsing alone {:.1} s",
        s.record as f64 / 1e6,
        s.coded as f64 / 1e6,
        100.0 * s.record as f64 / s.coded as f64,
        s.t_encode,
        s.t_decode,
        s.t_parse
    );
    println!(
        "  {:8} {:6.2}%; length prefixes {:.2}% of the bytes",
        "tail",
        100.0 * s.tail_bits as f64 / total as f64,
        200.0 * s.packets as f64 / s.bytes as f64
    );
}
