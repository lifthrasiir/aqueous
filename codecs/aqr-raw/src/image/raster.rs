//! Raw rasters: rows of 8-bit samples, interleaved pixels of 1 to 4 channels, each row
//! perhaps padded. Every sample is coded with the pixel model ([`super::pixel`]): its
//! residual against the prediction, in a table chosen by its channel and its activity (the
//! neighbours' gradients plus the magnitudes of the left and upper residuals), with static
//! rANS tables per chunk of rows. The padding is kept apart, as it is.
//!
//! The model looks at the current and previous rows only, so memory does not grow with the
//! image beyond the encoder's symbols of a chunk.

use super::pixel::{self, ACT, get, mag, put};
use aqr_common::{rans32, varint};

/// The layout of a raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub width: usize,
    pub height: usize,
    /// Samples per pixel (1..=4).
    pub channels: usize,
    /// Bytes from a row to the next, at least `width * channels`.
    pub stride: usize,
}

impl Format {
    /// Bytes of the samples of a row.
    pub fn row_bytes(&self) -> usize {
        self.width * self.channels
    }

    /// Bytes of the raster, padding included.
    pub fn len(&self) -> Option<usize> {
        self.stride.checked_mul(self.height)
    }

    pub fn valid(&self) -> bool {
        (1..=4).contains(&self.channels)
            && self.width > 0
            && self.height > 0
            && self.width.checked_mul(self.channels).is_some_and(|r| r <= self.stride && (r as u64) < 1 << 32)
            && self.len().is_some_and(|l| (l as u64) < 1 << 48)
    }

    /// The format as bytes: varints of the width, the height and the row padding
    /// (`stride - width * channels`), then the channels.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        varint::put(&mut v, self.width as u64);
        varint::put(&mut v, self.height as u64);
        varint::put(&mut v, (self.stride - self.row_bytes()) as u64);
        v.push(self.channels as u8);
        v
    }

    /// The format of [`Format::to_bytes`], if valid and nothing follows.
    pub fn from_bytes(b: &[u8]) -> Option<Format> {
        let mut p = 0;
        let mut num = || usize::try_from(varint::get(b, &mut p).ok()?).ok();
        let (width, height, pad) = (num()?, num()?, num()?);
        let (&channels, rest) = b.get(p..)?.split_first()?;
        let channels = channels as usize;
        let f = Format { width, height, channels, stride: width.checked_mul(channels)?.checked_add(pad)? };
        (rest.is_empty() && f.valid()).then_some(f)
    }
}

#[derive(Debug)]
pub struct Error(pub &'static str);

/// Probabilities of the tables (as aqr-png's).
const PROB_BITS: u32 = 12;
type Tables = rans32::Tables<u8, PROB_BITS>;
type BitW = rans32::BitW<false>;
type BitR<'a> = rans32::BitR<'a, false>;

/// Tables: by channel (the fourth for any after it) and activity bucket.
const TABLES: usize = 4 * ACT;

/// Samples per chunk of rows (at least one row), each with tables of its own.
const CHUNK: usize = 1 << 22;

/// Rows per chunk.
fn chunk_rows(f: &Format) -> usize {
    (CHUNK / f.row_bytes()).max(1)
}

/// The previous and the current row of the samples and of their residuals, as raw buffers
/// ([`pixel`]): a margin, then two rows of `rs2` bytes, each `bpp` zero bytes then the row.
struct Rows {
    raw: Vec<u8>,
    res: Vec<u8>,
    rs2: usize,
}

/// The margin before the first row.
const MARGIN: usize = 8;

impl Rows {
    fn new(f: &Format) -> Rows {
        let rs2 = f.channels + f.row_bytes();
        Rows { raw: vec![0; MARGIN + 2 * rs2], res: vec![0; MARGIN + 2 * rs2], rs2 }
    }

    /// Offset of the current row's first sample.
    fn q0(&self, bpp: usize) -> usize {
        MARGIN + self.rs2 + bpp
    }

    /// Makes the current row the previous one.
    fn next(&mut self) {
        let (a, b) = (MARGIN, MARGIN + self.rs2);
        self.raw.copy_within(b..b + self.rs2, a);
        self.res.copy_within(b..b + self.rs2, a);
    }
}

/// The table of the sample at `q` with the model `p` and channel `ch`.
#[inline(always)]
fn table<const BPP: usize>(r: &Rows, q: usize, p: &pixel::Pred, ch: usize) -> usize {
    let ext = mag(get(&r.res, q - BPP)) + mag(get(&r.res, q - r.rs2));
    ch.min(3) * ACT + pixel::bucket(p.act + ext)
}

/// The coded samples and the row padding.
pub struct Coded {
    /// Per chunk of rows: varints of the lengths of its tables and of its rANS stream, then
    /// both.
    pub coded: Vec<u8>,
    pub pad: Vec<u8>,
}

/// Codes the raster `d` of layout `f` (`d` exactly as long as it).
pub fn encode(d: &[u8], f: Format) -> Option<Coded> {
    if !f.valid() || f.len() != Some(d.len()) {
        return None;
    }
    Some(match f.channels {
        1 => encode_bpp::<1>(d, &f),
        2 => encode_bpp::<2>(d, &f),
        3 => encode_bpp::<3>(d, &f),
        _ => encode_bpp::<4>(d, &f),
    })
}

fn encode_bpp<const BPP: usize>(d: &[u8], f: &Format) -> Coded {
    let rb = f.row_bytes();
    let mut c = Coded { coded: Vec::with_capacity(d.len() / 2), pad: Vec::new() };
    let mut r = Rows::new(f);
    let q0 = r.q0(BPP);
    let per = chunk_rows(f);
    let mut syms: Vec<(u8, u8)> = Vec::with_capacity(per.min(f.height) * rb);
    for (y, row) in d.chunks_exact(f.stride).enumerate() {
        r.raw[q0..q0 + rb].copy_from_slice(&row[..rb]);
        c.pad.extend_from_slice(&row[rb..]);
        for x in 0..rb {
            let q = q0 + x;
            let ch = x % BPP;
            let p = pixel::predict::<BPP>(&r.raw, q, r.rs2, ch);
            let e = get(&r.raw, q).wrapping_sub(p.pred);
            put(&mut r.res, q, e);
            syms.push((table::<BPP>(&r, q, &p, ch) as u8, pixel::zigzag(e) as u8));
        }
        r.next();
        if (y + 1) % per == 0 || y + 1 == f.height {
            put_chunk(&syms, &mut c.coded);
            syms.clear();
        }
    }
    c
}

/// The tables of `syms` (a bitmap of those used, then their levels) and their rANS stream,
/// each after its length.
fn put_chunk(syms: &[(u8, u8)], out: &mut Vec<u8>) {
    let mut h = vec![0u32; TABLES * 256];
    for &(t, v) in syms {
        h[t as usize * 256 + v as usize] += 1;
    }
    let mut w = BitW::default();
    let used: Vec<bool> = h.as_chunks::<256>().0.iter().map(|c| c.iter().any(|&x| x > 0)).collect();
    for &u in &used {
        w.bits(1, u as u32);
    }
    let freqs: Vec<Vec<u32>> = h
        .as_chunks::<256>()
        .0
        .iter()
        .zip(&used)
        .map(|(c, &u)| if u { rans32::write_table::<PROB_BITS, false>(&mut w, c) } else { vec![1 << PROB_BITS] })
        .collect();
    let tb = w.finish();
    let st = rans32::encode::<PROB_BITS>(syms, &freqs);
    varint::put(out, tb.len() as u64);
    varint::put(out, st.len() as u64);
    out.extend_from_slice(&tb);
    out.extend_from_slice(&st);
}

/// A chunk's tables and the decoder of its stream.
fn get_chunk<'a>(coded: &'a [u8], at: &mut usize) -> Result<(Tables, rans32::Decoder<'a>, &'a [u8]), Error> {
    let corrupt = Error("corrupt raster");
    let mut len = || varint::get(coded, at).ok().and_then(|v| usize::try_from(v).ok()).ok_or(Error("corrupt raster"));
    let (nt, ns) = (len()?, len()?);
    let tb = coded.get(*at..at.checked_add(nt).ok_or(Error("corrupt raster"))?).ok_or(Error("corrupt raster"))?;
    *at += nt;
    let st = coded.get(*at..at.checked_add(ns).ok_or(Error("corrupt raster"))?).ok_or(corrupt)?;
    *at += ns;
    let mut r = BitR::new(tb);
    let used: Vec<bool> = (0..TABLES).map(|_| r.bits(1) == 1).collect();
    let mut freqs = Vec::with_capacity(TABLES);
    for &u in &used {
        freqs.push(if u {
            rans32::read_table::<PROB_BITS, false>(&mut r, 256).map_err(|_| Error("corrupt raster tables"))?
        } else {
            vec![]
        });
    }
    if r.overrun() {
        return Err(Error("corrupt raster tables"));
    }
    Ok((Tables::new(&freqs), rans32::Decoder::new(st), st))
}

/// Decodes the raster of layout `f` from `coded` and `pad` into `out` (appending).
pub fn decode(coded: &[u8], pad: &[u8], f: Format, out: &mut Vec<u8>) -> Result<(), Error> {
    let len = f.len().filter(|_| f.valid()).ok_or(Error("format"))?;
    if pad.len() != (f.stride - f.row_bytes()) * f.height {
        return Err(Error("padding"));
    }
    // (all of it at once, as the output the module gives back is of it anyway: a flat image,
    // which takes next to nothing coded, grows no vector twice as large)
    out.reserve(len);
    match f.channels {
        1 => decode_bpp::<1>(coded, pad, &f, out),
        2 => decode_bpp::<2>(coded, pad, &f, out),
        3 => decode_bpp::<3>(coded, pad, &f, out),
        _ => decode_bpp::<4>(coded, pad, &f, out),
    }
}

fn decode_bpp<const BPP: usize>(coded: &[u8], pad: &[u8], f: &Format, out: &mut Vec<u8>) -> Result<(), Error> {
    let rb = f.row_bytes();
    let pb = f.stride - rb;
    let mut r = Rows::new(f);
    let q0 = r.q0(BPP);
    let per = chunk_rows(f);
    let mut at = 0;
    let mut y = 0;
    while y < f.height {
        let (tb, mut dec, _) = get_chunk(coded, &mut at)?;
        let end = (y + per).min(f.height);
        for y in y..end {
            for x in 0..rb {
                let q = q0 + x;
                let ch = x % BPP;
                let p = pixel::predict::<BPP>(&r.raw, q, r.rs2, ch);
                let s = dec.get(&tb, table::<BPP>(&r, q, &p, ch));
                let e = pixel::unzigzag(s);
                put(&mut r.raw, q, p.pred.wrapping_add(e));
                put(&mut r.res, q, e);
            }
            out.extend_from_slice(&r.raw[q0..q0 + rb]);
            out.extend_from_slice(&pad[y * pb..(y + 1) * pb]);
            r.next();
        }
        if !dec.finished() {
            return Err(Error("corrupt raster"));
        }
        y = end;
    }
    if at != coded.len() {
        return Err(Error("corrupt raster"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes() {
        let f = Format { width: 300, height: 7, channels: 3, stride: 904 };
        assert_eq!(Format::from_bytes(&f.to_bytes()), Some(f));
        assert_eq!(Format::from_bytes(&[f.to_bytes(), vec![0]].concat()), None);
        assert_eq!(Format::from_bytes(&[1, 1, 0, 5]), None);
        assert_eq!(Format::from_bytes(&[0, 1, 0, 1]), None);
    }

    #[test]
    fn round_trip() {
        let mut seed = 7u32;
        for f in [
            Format { width: 1, height: 1, channels: 1, stride: 1 },
            Format { width: 37, height: 5, channels: 3, stride: 112 },
            Format { width: 64, height: 40, channels: 4, stride: 256 },
            Format { width: 9, height: 3000, channels: 2, stride: 18 },
            Format { width: 2000, height: 2100, channels: 1, stride: 2000 },
        ] {
            let d: Vec<u8> = (0..f.len().unwrap())
                .map(|i| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    ((i % f.stride) as u32 / 3 + (seed >> 29)) as u8
                })
                .collect();
            let c = encode(&d, f).unwrap();
            let mut out = vec![];
            decode(&c.coded, &c.pad, f, &mut out).unwrap();
            assert!(out == d, "{f:?}");
            assert!(decode(&c.coded[..c.coded.len() - 1], &c.pad, f, &mut vec![]).is_err());
        }
    }
}
