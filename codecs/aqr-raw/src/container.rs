//! Trivial containers: a header, the samples as they are (of a layout the header tells), and
//! perhaps a trailer. Taking one apart is finding the samples and their layout; the rest is
//! kept as it is ([`crate::formula::encode`]).
//!
//! Audio: WAV (PCM, also WAVE_FORMAT_EXTENSIBLE) and AIFF / AIFC (uncompressed, `sowt`).
//! Images: BMP (8, 24 and 32 bits per pixel, uncompressed) and PNM (PGM, PPM, PAM; 8-bit).

use crate::audio::pcm;
use crate::image::raster;

/// What the samples of a container are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Pcm(pcm::Format),
    Raster(raster::Format),
}

/// Where a container's samples are, and their layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    /// The container's name, as aqr-compress names formats: `wav`, `aiff`, `bmp`, `pnm`.
    pub name: &'static str,
    pub layout: Layout,
    /// The samples are `d[start..start + len]`: whole sample frames for PCM (a partial one
    /// at the end is left to the trailer), whole rows for an image.
    pub start: usize,
    pub len: usize,
}

/// The container `d` is, if it is one of those taken apart here.
pub fn detect(d: &[u8]) -> Option<Found> {
    wav(d).or_else(|| aiff(d)).or_else(|| bmp(d)).or_else(|| pnm(d))
}

fn u16le(d: &[u8], p: usize) -> Option<usize> {
    Some(u16::from_le_bytes(*d.get(p..)?.first_chunk()?) as usize)
}

fn u32le(d: &[u8], p: usize) -> Option<usize> {
    Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?) as usize)
}

fn u16be(d: &[u8], p: usize) -> Option<usize> {
    Some(u16::from_be_bytes(*d.get(p..)?.first_chunk()?) as usize)
}

fn u32be(d: &[u8], p: usize) -> Option<usize> {
    Some(u32::from_be_bytes(*d.get(p..)?.first_chunk()?) as usize)
}

/// PCM samples at `start`, `len` bytes of them as the header says (clamped to the file),
/// cut to whole sample frames.
fn pcm_found(name: &'static str, d: &[u8], f: pcm::Format, start: usize, len: usize) -> Option<Found> {
    if !f.valid() || start > d.len() {
        return None;
    }
    let len = len.min(d.len() - start);
    let len = len - len % f.frame_bytes();
    Some(Found { name, layout: Layout::Pcm(f), start, len })
}

/// A WAV file: RIFF chunks, `fmt ` (PCM, or WAVE_FORMAT_EXTENSIBLE of PCM) before `data`.
fn wav(d: &[u8]) -> Option<Found> {
    if d.get(0..4)? != b"RIFF" || d.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    let mut p = 12;
    while p + 8 <= d.len() {
        let id = &d[p..p + 4];
        let len = u32le(d, p + 4)?;
        let body = p + 8;
        if id == b"fmt " && len >= 16 && body + 16 <= d.len() {
            let tag = u16le(d, body)?;
            let channels = u16le(d, body + 2)?;
            let align = u16le(d, body + 12)?;
            let bits = u16le(d, body + 14)?;
            let pcm = tag == 1 || (tag == 0xfffe && len >= 40 && body + 26 <= d.len() && u16le(d, body + 24)? == 1);
            if !pcm || channels == 0 || align % channels != 0 || bits == 0 || bits > 8 * (align / channels) {
                return None;
            }
            let bytes = align / channels;
            fmt = Some(pcm::Format { channels, bytes, big_endian: false, unsigned: bytes == 1 });
        } else if id == b"data" {
            // The size may be missing (streamed) or wrong: clamped to the file.
            let len = if len == 0 || len == 0xffff_ffff { d.len() - body } else { len };
            return pcm_found("wav", d, fmt?, body, len);
        }
        p = body.checked_add(len)?.checked_add(len & 1)?;
    }
    None
}

/// An AIFF or AIFC file: IFF chunks, `COMM` (uncompressed, `NONE`, `twos` or `sowt`) before
/// `SSND`.
fn aiff(d: &[u8]) -> Option<Found> {
    if d.get(0..4)? != b"FORM" || !matches!(d.get(8..12)?, b"AIFF" | b"AIFC") {
        return None;
    }
    let aifc = &d[8..12] == b"AIFC";
    let mut fmt = None;
    let mut p = 12;
    while p + 8 <= d.len() {
        let id = &d[p..p + 4];
        let len = u32be(d, p + 4)?;
        let body = p + 8;
        if id == b"COMM" && len >= 18 && body + 18 <= d.len() {
            let channels = u16be(d, body)?;
            let bits = u16be(d, body + 6)?;
            let little = if aifc {
                match d.get(body + 18..body + 22)? {
                    b"NONE" | b"twos" => false,
                    b"sowt" => true,
                    _ => return None,
                }
            } else {
                false
            };
            if bits == 0 || bits > 32 {
                return None;
            }
            fmt = Some(pcm::Format { channels, bytes: bits.div_ceil(8), big_endian: !little, unsigned: false });
        } else if id == b"SSND" && body + 8 <= d.len() {
            let f = fmt?;
            let offset = u32be(d, body)?;
            let start = body.checked_add(8 + offset)?;
            return pcm_found("aiff", d, f, start, len.saturating_sub(8 + offset));
        }
        p = body.checked_add(len)?.checked_add(len & 1)?;
    }
    None
}

/// Raster samples at `start`, if the file holds all of them.
fn raster_found(name: &'static str, d: &[u8], f: raster::Format, start: usize) -> Option<Found> {
    let len = f.len()?;
    (f.valid() && start.checked_add(len)? <= d.len()).then_some(Found { name, layout: Layout::Raster(f), start, len })
}

/// A BMP file of 8 (palette indices), 24 or 32 bits per pixel, uncompressed (or of bit
/// fields, for 32 bits), rows padded to 4 bytes, bottom-up or top-down alike.
fn bmp(d: &[u8]) -> Option<Found> {
    if d.get(0..2)? != b"BM" {
        return None;
    }
    let off = u32le(d, 10)?;
    let dib = u32le(d, 14)?;
    let (w, h, planes, bpp, comp) = match dib {
        12 => (u16le(d, 18)? as i64, u16le(d, 20)? as i64, u16le(d, 22)?, u16le(d, 24)?, 0),
        40 | 52 | 56 | 64 | 108 | 124 => {
            (u32le(d, 18)? as i32 as i64, u32le(d, 22)? as i32 as i64, u16le(d, 26)?, u16le(d, 28)?, u32le(d, 30)?)
        }
        _ => return None,
    };
    let channels = match (bpp, comp) {
        (8, 0) => 1,
        (24, 0) => 3,
        (32, 0 | 3) => 4,
        _ => return None,
    };
    let (w, h) = (usize::try_from(w).ok()?, h.unsigned_abs() as usize);
    if planes != 1 || w == 0 || h == 0 {
        return None;
    }
    let stride = (w.checked_mul(channels)?).checked_add(3)? & !3;
    raster_found("bmp", d, raster::Format { width: w, height: h, channels, stride }, off)
}

/// A binary PNM file of 8-bit samples: PGM (`P5`), PPM (`P6`) or PAM (`P7`, of 1 to 4
/// channels). Only the first image of a file is taken.
fn pnm(d: &[u8]) -> Option<Found> {
    let kind = match d.get(0..2)? {
        b"P5" => 1,
        b"P6" => 3,
        b"P7" => 0,
        _ => return None,
    };
    let mut p = 2;
    let (w, h, channels, maxval) = if kind == 0 {
        pam_header(d, &mut p)?
    } else {
        let mut v = [0usize; 3];
        for x in &mut v {
            *x = pnm_number(d, &mut p)?;
        }
        // One whitespace byte ends the header.
        if !d.get(p)?.is_ascii_whitespace() {
            return None;
        }
        p += 1;
        (v[0], v[1], kind, v[2])
    };
    if !(1..256).contains(&maxval) || w == 0 || h == 0 {
        return None;
    }
    let stride = w.checked_mul(channels)?;
    raster_found("pnm", d, raster::Format { width: w, height: h, channels, stride }, p)
}

/// A decimal number of a PNM header after whitespace and comments.
fn pnm_number(d: &[u8], p: &mut usize) -> Option<usize> {
    loop {
        match *d.get(*p)? {
            b'#' => *p += d[*p..].iter().position(|&c| c == b'\n')?,
            c if c.is_ascii_whitespace() => *p += 1,
            _ => break,
        }
    }
    let n = d[*p..].iter().take_while(|c| c.is_ascii_digit()).count();
    if n == 0 || n > 9 {
        return None;
    }
    let v = std::str::from_utf8(&d[*p..*p + n]).ok()?.parse().ok()?;
    *p += n;
    Some(v)
}

/// The header of a PAM file after `P7`, up to and past `ENDHDR\n`: width, height, depth and
/// maximum value.
fn pam_header(d: &[u8], p: &mut usize) -> Option<(usize, usize, usize, usize)> {
    let (mut w, mut h, mut depth, mut maxval) = (0, 0, 0, 0);
    let end = (*p + 4096).min(d.len());
    for line in d[*p..end].split_inclusive(|&c| c == b'\n') {
        *p += line.len();
        if line.last() != Some(&b'\n') {
            return None;
        }
        let line = std::str::from_utf8(line).ok()?.trim();
        let mut words = line.split_ascii_whitespace();
        let (Some(key), value) = (words.next(), words.next()) else { continue };
        let num = || value.and_then(|v| v.parse().ok());
        match key {
            "ENDHDR" => return (1..=4).contains(&depth).then_some((w, h, depth, maxval)),
            "WIDTH" => w = num()?,
            "HEIGHT" => h = num()?,
            "DEPTH" => depth = num()?,
            "MAXVAL" => maxval = num()?,
            _ => {}
        }
    }
    None
}
