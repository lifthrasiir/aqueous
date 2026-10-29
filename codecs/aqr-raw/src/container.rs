//! Trivial containers: a header, the samples as they are (of a layout the header tells), and
//! perhaps a trailer. Taking one apart is finding the samples and their layout; the rest is
//! kept as it is ([`crate::formula::encode`]).
//!
//! Audio: WAV (PCM, also WAVE_FORMAT_EXTENSIBLE) and AIFF / AIFC (uncompressed, `sowt`).
//! Images: BMP (8, 24 and 32 bits per pixel, uncompressed), PNM (PGM, PPM, PAM; 8-bit), TIFF
//! (uncompressed, 8-bit, its first image) and DICOM (8- and 16-bit, little-endian, its frames
//! one under another); and, by [`sized`], 16-bit samples after a header that tells their size.

use crate::audio::pcm;
use crate::image::{raster, raster16};

/// What the samples of a container are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Pcm(pcm::Format),
    Raster(raster::Format),
    Raster16(raster16::Format),
}

/// Where a container's samples are, and their layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    /// The container's name, as aqr-compress names formats: `wav`, `aiff`, `bmp`, `pnm`, `tiff`,
    /// `dicom` (or `sized`, of [`sized`]).
    pub name: &'static str,
    pub layout: Layout,
    /// The samples are `d[start..start + len]`: whole sample frames for PCM (a partial one
    /// at the end is left to the trailer), whole rows for an image.
    pub start: usize,
    pub len: usize,
}

/// The container `d` is, if it is one of those taken apart here.
pub fn detect(d: &[u8]) -> Option<Found> {
    wav(d).or_else(|| aiff(d)).or_else(|| bmp(d)).or_else(|| pnm(d)).or_else(|| tiff(d)).or_else(|| dicom(d))
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

/// A TIFF file whose first image is uncompressed, of 8-bit samples, 1 to 4 to a pixel,
/// interleaved, in strips one after another in the file. The rest of the file (other images
/// too) is kept as it is.
fn tiff(d: &[u8]) -> Option<Found> {
    let big = match d.get(0..4)? {
        b"II*\0" => false,
        b"MM\0*" => true,
        _ => return None,
    };
    let u16_at = |p| if big { u16be(d, p) } else { u16le(d, p) };
    let u32_at = |p| if big { u32be(d, p) } else { u32le(d, p) };
    let ifd = u32_at(4)?;
    let n = u16_at(ifd)?;
    // (type, count, the value or its offset) of a tag, and its values
    let entry = |tag: usize| {
        (0..n)
            .map(|i| ifd + 2 + 12 * i)
            .find(|&e| u16_at(e) == Some(tag))
            .and_then(|e| Some((u16_at(e + 2)?, u32_at(e + 4)?, e + 8)))
    };
    let values = |tag: usize| -> Option<Vec<usize>> {
        let (ty, count, at) = entry(tag)?;
        let size = match ty {
            3 => 2,
            4 => 4,
            _ => return None,
        };
        let at = if size * count > 4 { u32_at(at)? } else { at };
        (0..count).map(|i| if size == 2 { u16_at(at + 2 * i) } else { u32_at(at + 4 * i) }).collect()
    };
    let one = |tag: usize, default: Option<usize>| match values(tag) {
        Some(v) => (v.len() == 1).then(|| v[0]),
        None if entry(tag).is_none() => default,
        None => None,
    };
    let (w, h) = (one(256, None)?, one(257, None)?);
    let channels = one(277, Some(1))?;
    if one(259, Some(1))? != 1 || one(284, Some(1))? != 1 || !(1..=4).contains(&channels) || w == 0 || h == 0 {
        return None;
    }
    let bits = values(258).unwrap_or_else(|| vec![1]);
    if bits.len() != channels || bits.iter().any(|&b| b != 8) {
        return None;
    }
    let (offsets, counts) = (values(273)?, values(279)?);
    if offsets.len() != counts.len() || offsets.windows(2).zip(&counts).any(|(o, &c)| o[0] + c != o[1]) {
        return None;
    }
    let stride = w.checked_mul(channels)?;
    let f = raster::Format { width: w, height: h, channels, stride };
    (counts.iter().sum::<usize>() == f.len()?).then_some(())?;
    raster_found("tiff", d, f, *offsets.first()?)
}

/// 16-bit raster samples at `start`, if the file holds all of them.
fn raster16_found(name: &'static str, d: &[u8], f: raster16::Format, start: usize) -> Option<Found> {
    let len = f.len()?;
    (f.valid() && start.checked_add(len)? <= d.len()).then_some(Found { name, layout: Layout::Raster16(f), start, len })
}

/// A DICOM file (with its preamble and `DICM`, or without: a data set from its first byte, in
/// implicit VR little-endian, as older files are) whose pixel data are of 8- or 16-bit samples,
/// little-endian, interleaved, of a defined length, its frames one under another. Data elements
/// of undefined length (sequences) before the pixel data are not taken.
fn dicom(d: &[u8]) -> Option<Found> {
    // the data set's start and whether it is of explicit VRs
    let (mut p, explicit) = if d.get(128..132)? == b"DICM" {
        // the file meta information, explicit VR little-endian, its group length first
        if d.get(132..136)? != [2, 0, 0, 0] || d.get(136..138)? != b"UL" {
            return None;
        }
        let end = 144usize.checked_add(u32le(d, 140)?)?;
        let mut q = 144;
        let mut syntax = None;
        while q < end {
            let (group, elem, len, body) = element(d, q, true)?;
            if (group, elem) == (2, 0x10) {
                syntax = Some(d.get(body..body + len)?);
            }
            q = body + len;
        }
        let syntax = syntax?.strip_suffix(b"\0").unwrap_or(syntax?);
        match syntax {
            b"1.2.840.10008.1.2" => (end, false),
            b"1.2.840.10008.1.2.1" => (end, true),
            _ => return None,
        }
    } else if u16le(d, 0)? == 8 && u32le(d, 4)? < 256 {
        (0, false)
    } else {
        return None;
    };
    let (mut rows, mut cols, mut spp, mut bits, mut frames, mut planar) = (0, 0, 1, 0, 1, 0);
    loop {
        let (group, elem, len, body) = element(d, p, explicit)?;
        let v = d.get(body..body.checked_add(len)?)?;
        match (group, elem) {
            (0x28, 2) => spp = u16le(v, 0)?,
            (0x28, 6) => planar = u16le(v, 0)?,
            (0x28, 8) => frames = std::str::from_utf8(v).ok()?.trim_matches([' ', '\0']).parse().ok()?,
            (0x28, 0x10) => rows = u16le(v, 0)?,
            (0x28, 0x11) => cols = u16le(v, 0)?,
            (0x28, 0x100) => bits = u16le(v, 0)?,
            (0x7fe0, 0x10) => {
                let h = rows.checked_mul(frames)?;
                if planar != 0
                    || cols == 0
                    || h == 0
                    || len != h.checked_mul(cols)?.checked_mul(spp)?.checked_mul(bits / 8)?
                {
                    return None;
                }
                return match bits {
                    8 => raster_found(
                        "dicom",
                        d,
                        raster::Format { width: cols, height: h, channels: spp, stride: cols * spp },
                        body,
                    ),
                    16 => {
                        let f = raster16::Format {
                            width: cols,
                            height: h,
                            channels: spp,
                            stride: 2 * cols * spp,
                            big_endian: false,
                        };
                        raster16_found("dicom", d, f, body)
                    }
                    _ => None,
                };
            }
            _ => {}
        }
        p = body + len;
    }
}

/// The data element at `p` of a DICOM data set, little-endian, of explicit VRs or not: its tag,
/// the length of its value and where that is. None where the length is undefined.
fn element(d: &[u8], p: usize, explicit: bool) -> Option<(usize, usize, usize, usize)> {
    let (group, elem) = (u16le(d, p)?, u16le(d, p + 2)?);
    let (len, body) = if explicit {
        match d.get(p + 4..p + 6)? {
            b"OB" | b"OD" | b"OF" | b"OL" | b"OV" | b"OW" | b"SQ" | b"SV" | b"UC" | b"UN" | b"UR" | b"UT" | b"UV" => {
                (u32le(d, p + 8)?, p + 12)
            }
            _ => (u16le(d, p + 6)?, p + 8),
        }
    } else {
        (u32le(d, p + 4)?, p + 8)
    };
    (len != 0xffff_ffff).then_some((group, elem, len, body))
}

/// The most bytes of a header before samples that [`sized`] takes.
const MAX_HEADER: usize = 4096;

/// Data of no format that is 16-bit samples of one channel after a header telling their size:
/// two numbers one after the other in the first KiB (16- or 32-bit, either endianness) whose
/// product, in 16-bit samples, the rest of the data is after a header of at most
/// [`MAX_HEADER`] bytes past them. Which of the two is the width and the samples' endianness
/// are those under which neighbouring samples differ least; they must differ less than
/// samples half a row apart do, as in an image (not in other data).
pub fn sized(d: &[u8]) -> Option<Found> {
    let mut best: Option<(u64, raster16::Format, usize)> = None;
    for w in [2, 4] {
        for p in (0..1024usize.min(d.len().saturating_sub(2 * w))).step_by(2) {
            for big in [false, true] {
                let num = |q| {
                    if w == 2 {
                        if big { u16be(d, q) } else { u16le(d, q) }
                    } else if big {
                        u32be(d, q)
                    } else {
                        u32le(d, q)
                    }
                };
                let (a, b) = (num(p)?, num(p + w)?);
                if !(16..1 << 20).contains(&a) || !(16..1 << 20).contains(&b) {
                    continue;
                }
                let len = 2 * a * b;
                let Some(header) = d.len().checked_sub(len).filter(|&h| h >= p + 2 * w && h <= MAX_HEADER) else {
                    continue;
                };
                for (width, height) in [(a, b), (b, a)] {
                    for big_endian in [false, true] {
                        let f = raster16::Format { width, height, channels: 1, stride: 2 * width, big_endian };
                        let r = roughness(&d[header..], &f);
                        if best.is_none_or(|(b, ..)| r < b) {
                            best = Some((r, f, header));
                        }
                    }
                }
            }
        }
    }
    let (r, f, header) = best?;
    (r < far(&d[header..], &f)).then_some(())?;
    raster16_found("sized", d, f, header)
}

/// How much neighbouring samples of the raster `d` of layout `f` differ, left and upper, on a
/// sample of its rows: the mean difference, in 1/256.
fn roughness(d: &[u8], f: &raster16::Format) -> u64 {
    mean_over_rows(d, f, |up, cur| {
        let left = cur.windows(2).map(|w| w[0].abs_diff(w[1]));
        left.chain(std::iter::zip(up, cur).map(|(a, b)| a.abs_diff(*b))).sum()
    })
}

/// How much samples half a row apart differ, as [`roughness`] counts (over as many pairs).
fn far(d: &[u8], f: &raster16::Format) -> u64 {
    let (w, half) = (f.width, f.width / 2);
    mean_over_rows(d, f, |_, cur| (0..2 * w - 1).map(|i| cur[i % w].abs_diff(cur[(i % w + half) % w])).sum())
}

/// The mean of `pairs`, the sum of the differences of `2 * width - 1` pairs of samples of a row
/// (given it and the row before), over rows sampled through the raster, in 1/256.
fn mean_over_rows(d: &[u8], f: &raster16::Format, pairs: impl Fn(&[i64], &[i64]) -> u64) -> u64 {
    let row = |y: usize| -> Vec<i64> {
        d[y * f.stride..y * f.stride + f.row_bytes()].chunks_exact(2).map(|b| f.sample(b) as i64).collect()
    };
    let rows: Vec<usize> = (1..f.height).step_by((f.height / 64).max(1)).collect();
    let sum: u64 = rows.iter().map(|&y| pairs(&row(y - 1), &row(y))).sum();
    (sum << 8) / (rows.len() as u64 * (2 * f.width as u64 - 1)).max(1)
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
