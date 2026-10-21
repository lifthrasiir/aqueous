//! Where a file found by [`detect`](crate::detect) ends: what follows it in the data is not
//! given to its recompressor.

use crate::detect::{Format, id3_len, mp3_header};
use crate::flate::{gzip_deflate, zlib_deflate};
use aqr_flac::flac;

/// Bytes of the file of format `f` at the start of `d`, as far as its structure tells, or
/// `None` where its structure does not hold. A file whose structure does not tell its end (a
/// FLAC stream, a PDF without `%%EOF`) or runs past `d` ends with `d`.
///
/// The end need not be exact: what is cut off stays in the data around the file, and what is
/// left in is the recompressor's to code or store.
pub fn extent(f: Format, d: &[u8]) -> Option<usize> {
    let end = match f {
        Format::Pe => pe(d),
        Format::Gzip => gzip_deflate(d).map(|r| r.end + 8),
        Format::Zlib => zlib_deflate(d).map(|r| r.end + 4),
        Format::Png => png(d),
        Format::Jpeg => jpeg(d),
        Format::Mp3 => mp3(d),
        Format::Flac => flac(d),
        Format::Psd => psd(d),
        Format::Pdf => Some(pdf(d)),
        Format::Sqlite => sqlite(d),
    }?;
    Some(end.min(d.len()))
}

fn u16le(d: &[u8], p: usize) -> Option<u64> {
    Some(u16::from_le_bytes(*d.get(p..)?.first_chunk()?) as u64)
}

fn u32le(d: &[u8], p: usize) -> Option<u64> {
    Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?) as u64)
}

fn u16be(d: &[u8], p: usize) -> Option<u64> {
    Some(u16::from_be_bytes(*d.get(p..)?.first_chunk()?) as u64)
}

fn u32be(d: &[u8], p: usize) -> Option<u64> {
    Some(u32::from_be_bytes(*d.get(p..)?.first_chunk()?) as u64)
}

fn u64be(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

/// `p`, or the end of `d` where `p` is past it (or does not fit).
fn clamp(d: &[u8], p: u64) -> usize {
    usize::try_from(p).map_or(d.len(), |p| p.min(d.len()))
}

/// The headers and the sections' data, with the certificate table where it is after them: the
/// overlay is left out.
fn pe(d: &[u8]) -> Option<usize> {
    let coff = u32le(d, 0x3c)? as usize + 4;
    let (sections, opt_size) = (u16le(d, coff + 2)? as usize, u16le(d, coff + 16)? as usize);
    let opt = coff + 20;
    let dirs = match u16le(d, opt)? {
        0x10b => opt + 96,
        0x20b => opt + 112,
        _ => return None,
    };
    let table = opt + opt_size;
    let mut end = u32le(d, opt + 60)?.max((table + 40 * sections) as u64);
    for s in (table..).step_by(40).take(sections) {
        let (size, at) = (u32le(d, s + 16)?, u32le(d, s + 20)?);
        if size > 0 {
            end = end.max(at + size);
        }
    }
    // The certificate table (directory 4), the only one given by file offset.
    if u32le(d, dirs - 4)? > 4 {
        let (at, size) = (u32le(d, dirs + 32)?, u32le(d, dirs + 36)?);
        if at > 0 && size > 0 {
            end = end.max(at + size);
        }
    }
    Some(clamp(d, end))
}

/// The chunks up to `IEND`.
fn png(d: &[u8]) -> Option<usize> {
    let mut p = 8u64;
    while let Some(len) = u32be(d, p as usize) {
        let ty = d.get(p as usize + 4..p as usize + 8)?;
        if len > 0x7fff_ffff || !ty.iter().all(u8::is_ascii_alphabetic) {
            return None;
        }
        p += 12 + len;
        if ty == b"IEND" || p >= d.len() as u64 {
            break;
        }
    }
    Some(clamp(d, p))
}

/// The segments and entropy-coded data up to EOI. Broken structure ends the file where it
/// starts, if a scan was found before it.
fn jpeg(d: &[u8]) -> Option<usize> {
    let (mut p, mut scan) = (2, false);
    while p + 1 < d.len() {
        if d[p] != 0xff {
            return scan.then_some(p);
        }
        let m = d[p + 1];
        match m {
            // fill
            0xff => p += 1,
            0xd9 => return Some(p + 2),
            0x01 | 0xd0..=0xd7 => p += 2,
            0x00 | 0xd8 => return scan.then_some(p),
            _ => {
                let Some(len) = u16be(d, p + 2).filter(|&l| l >= 2) else { return scan.then_some(p) };
                p += 2 + len as usize;
                if m == 0xda {
                    scan = true;
                    // to the next marker that is not a restart
                    while p + 1 < d.len() && (d[p] != 0xff || matches!(d[p + 1], 0x00 | 0xd0..=0xd7)) {
                        p += if d[p] == 0xff { 2 } else { 1 };
                    }
                }
            }
        }
    }
    Some(d.len())
}

/// Bytes of the MPEG audio layer III frame at the start of `d` without its padding byte, and
/// that byte; `free` for free format, where the header does not tell.
fn mp3_frame(d: &[u8], free: Option<usize>) -> Option<usize> {
    const KBPS: [[u16; 15]; 2] = [
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    ];
    // by version: 2.5, (reserved), 2, 1
    const RATE: [[u32; 3]; 4] = [[11025, 12000, 8000], [0; 3], [22050, 24000, 16000], [44100, 48000, 32000]];
    if !mp3_header(d) {
        return None;
    }
    let (version, bitrate, rate) = ((d[1] >> 3 & 3) as usize, (d[2] >> 4) as usize, (d[2] >> 2 & 3) as usize);
    let pad = (d[2] >> 1 & 1) as usize;
    let v1 = version == 3;
    let bps = KBPS[v1 as usize][bitrate] as u32 * 1000;
    let len = if bps > 0 { Some(((if v1 { 144 } else { 72 }) * bps / RATE[version][rate]) as usize) } else { free };
    Some(len? + pad)
}

/// The size of the free-format frames at the start of `d` without padding: up to the next
/// header of the stream, within the largest frame (of 640 kbit/s at the lowest rate).
fn mp3_free(d: &[u8]) -> Option<usize> {
    let key = (d[1] & 0xfe, d[2] & 0xfc);
    let end = d.len().min(5762);
    (4..end)
        .find(|&p| d[p] == 0xff && d.get(p + 1..p + 3).is_some_and(|h| (h[0] & 0xfe, h[1] & 0xfc) == key))?
        .checked_sub((d[2] >> 1 & 1) as usize)
}

/// Frames of the same version, layer and rate one after another, after an ID3v2 tag if any,
/// and an ID3v1 tag after them. Without a tag, a single frame is no sign of a file.
fn mp3(d: &[u8]) -> Option<usize> {
    let tagged = id3_len(d);
    let mut p = tagged.unwrap_or(0);
    let first = d.get(p..).filter(|f| mp3_header(f))?;
    let free = if first[2] >> 4 == 0 { Some(mp3_free(first)?) } else { None };
    let (mut frames, mut stream) = (0, None);
    while let Some(f) = d.get(p..) {
        let Some(n) = mp3_frame(f, free) else { break };
        // the CRC and the bitrate may change from frame to frame (not in free format)
        let key = (f[1] & 0xfe, f[2] & if free.is_some() { 0xfc } else { 0x0c });
        if *stream.get_or_insert(key) != key {
            break;
        }
        frames += 1;
        p += n;
    }
    if tagged.is_none() && frames < 2 {
        return None;
    }
    Some(p + d.get(p..).map_or(0, id3v1))
}

/// RIFF and FORM by their sizes; FLAC by its frames ([`flac_frames`]).
fn flac(d: &[u8]) -> Option<usize> {
    let size = match d.first_chunk::<4>()? {
        b"RIFF" => u32le(d, 4)?,
        b"FORM" => u32be(d, 4)?,
        _ => return Some(flac_frames(d)),
    };
    // A size of 0 is that of a stream written without seeking back.
    Some(if size == 0 { d.len() } else { clamp(d, 8 + size + (size & 1)) })
}

/// The longest a FLAC frame is taken to be where the stream info does not tell, and the most
/// junk taken between frames.
const FLAC_FRAME: usize = 1 << 20;
const FLAC_JUNK: usize = 4096;

/// The metadata and the frames of a FLAC stream: a frame ends where its CRC-16 does and is
/// followed by the next, the first header (with its CRC-8) after it (past junk, up to
/// [`FLAC_JUNK`] bytes) that numbers the frame or samples after its own; a frame without an
/// end, damaged, is followed by the next within the largest frame size. The last frame ends at
/// its first end, and an ID3v1 tag may follow it; one without an end is left out. Without the
/// metadata, to the end of `d`.
fn flac_frames(d: &[u8]) -> usize {
    let Some((info, start)) = flac::parse_metadata(d) else { return d.len() };
    let Some(&sync) = d.get(start + 1) else { return start };
    // The stream info is the first block; its maximum frame size is 0 where unknown.
    let si = id3_len(d).unwrap_or(0) + 8;
    let max_frame = u32be(d, si + 7).map(|v| (v >> 8) as usize).filter(|&n| n > 0).unwrap_or(FLAC_FRAME);
    // the header's length without its CRC-8, the frame's number (or its first sample's), and
    // the next frame's
    let header = |q: usize| -> Option<(usize, u64, u64)> {
        let (len, block, ..) = flac::parse_header(d, q, info.bps)?;
        let h = &d[q..];
        if h[1] != sync {
            return None;
        }
        let n = h[4].leading_ones() as usize;
        let first = (h[4] & (0x7f >> n)) as u64;
        let num = h[5..5 + n.saturating_sub(1)].iter().fold(first, |a, &b| a << 6 | (b & 0x3f) as u64);
        Some((len, num, num + if sync == 0xf8 { 1 } else { block as u64 }))
    };
    let Some(mut h) = header(start) else { return start };
    let mut q = start;
    loop {
        // the next frame's header in `from..to`
        let next = |from: usize, to: usize| {
            (from..d.len().min(to).saturating_sub(1))
                .filter(|&s| d[s] == 0xff && d[s + 1] == sync)
                .find_map(|s| header(s).filter(|n| n.1 == h.2).map(|n| (s, n)))
        };
        // A CRC of 0 by chance ends no frame: the next is not after it. A frame without an
        // end (damaged) is junk up to the next.
        let mut ends = crc16_ends(d, q, q + h.0 + 1, max_frame);
        let first = ends.next();
        let found = first
            .into_iter()
            .chain(ends)
            .find_map(|end| next(end, end + FLAC_JUNK))
            .or_else(|| next(q + h.0 + 1, q + max_frame + FLAC_JUNK));
        match (found, first) {
            (Some((s, n)), _) => (q, h) = (s, n),
            (None, Some(end)) => return end + id3v1(&d[end..]),
            (None, None) => return q,
        }
    }
}

/// CRC-16 of FLAC frames (polynomial 0x8005, MSB first), by byte.
const CRC16: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 { c << 1 ^ 0x8005 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// The possible ends of the FLAC frame at `q` whose subframes start at `from`, up to `max`
/// bytes: where the CRC-16 of the frame is 0 (its last two bytes are the CRC of the rest).
fn crc16_ends(d: &[u8], q: usize, from: usize, max: usize) -> impl Iterator<Item = usize> {
    let (mut c, mut i, end) = (0u16, q, d.len().min(q.saturating_add(max)));
    std::iter::from_fn(move || {
        while i < end {
            c = c << 8 ^ CRC16[((c >> 8) as u8 ^ d[i]) as usize];
            i += 1;
            // a byte of subframe and the CRC
            if c == 0 && i >= from + 3 {
                return Some(i);
            }
        }
        None
    })
}

/// Bytes of the ID3v1 tag at the start of `d`, if any.
fn id3v1(d: &[u8]) -> usize {
    if d.starts_with(b"TAG") && d.len() >= 128 { 128 } else { 0 }
}

/// The header, the three sections given by their lengths, and the image data where it is raw
/// or RLE.
fn psd(d: &[u8]) -> Option<usize> {
    let psb = match u16be(d, 4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let (channels, height, width, depth) = (u16be(d, 12)?, u32be(d, 14)?, u32be(d, 18)?, u16be(d, 22)?);
    if !(1..=56).contains(&channels) || !matches!(depth, 1 | 8 | 16 | 32) {
        return None;
    }
    let mut p = 26u64;
    for wide in [false, false, psb] {
        let len = if wide { u64be(d, clamp(d, p)) } else { u32be(d, clamp(d, p)) };
        let Some(len) = len else { return Some(d.len()) };
        p = p.saturating_add(len).saturating_add(if wide { 8 } else { 4 });
    }
    let Some(compression) = u16be(d, clamp(d, p)) else { return Some(d.len()) };
    p += 2;
    let rows = channels * height;
    let end = match compression {
        0 => (width * depth).div_ceil(8).checked_mul(rows).and_then(|n| p.checked_add(n)),
        1 => {
            // the byte counts of the rows, then the rows
            let (w, at) = (if psb { 4 } else { 2 }, clamp(d, p));
            d.get(at..at.checked_add((rows * w) as usize)?).map(|t| {
                let rows: u64 = t.chunks_exact(w as usize).map(|c| c.iter().fold(0, |a, &b| a << 8 | b as u64)).sum();
                p + t.len() as u64 + rows
            })
        }
        _ => None,
    };
    Some(end.map_or(d.len(), |e| clamp(d, e)))
}

/// To the last `%%EOF` of the file: one followed by an object or a cross-reference section
/// (an incremental update, or the first page of a linearized file) is not the last.
fn pdf(d: &[u8]) -> usize {
    let mut end = None;
    let mut p = 0;
    while let Some(i) = d[p..].windows(5).position(|w| w == b"%%EOF") {
        p += i + 5;
        p += d[p..].starts_with(b"\r") as usize;
        p += d[p..].starts_with(b"\n") as usize;
        end = Some(p);
        let next = d[p..].trim_ascii_start();
        if !(next.starts_with(b"xref") || object(next)) {
            break;
        }
    }
    end.unwrap_or(d.len())
}

/// `d` starts with `N G obj`.
fn object(d: &[u8]) -> bool {
    let digits = |d: &[u8]| d.iter().take_while(|b| b.is_ascii_digit()).count();
    let n = digits(d);
    let rest = d[n..].trim_ascii_start();
    let g = digits(rest);
    n > 0
        && g > 0
        && rest.len() > g
        && rest[g].is_ascii_whitespace()
        && rest[g..].trim_ascii_start().starts_with(b"obj")
}

/// The pages the header counts, where the count is valid; else to the end of `d`.
fn sqlite(d: &[u8]) -> Option<usize> {
    let size = match u16be(d, 16)? {
        1 => 65536,
        s if s.is_power_of_two() && (512..=32768).contains(&s) => s,
        _ => return None,
    };
    let pages = u32be(d, 28)?;
    // The count is valid only where the version-valid-for number is the change counter.
    Some(if pages > 0 && u32be(d, 92)? == u32be(d, 24)? { clamp(d, pages * size) } else { d.len() })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn fixture(path: &str) -> Vec<u8> {
        std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// `file` ends where it does, also with other data after it.
    fn ends(f: Format, file: &[u8]) {
        assert_eq!(extent(f, file), Some(file.len()), "{f}");
        let d = [file, b"\0\0\0\0trailing bytes"].concat();
        assert_eq!(extent(f, &d), Some(file.len()), "{f} with trailing bytes");
    }

    #[test]
    fn fixtures() {
        for (f, path) in [
            (Format::Jpeg, "aqr-jpeg/tests/data/gradient.jpg"),
            (Format::Mp3, "aqr-mp3/tests/data/cbr128.mp3"),
            (Format::Mp3, "aqr-mp3/tests/data/vbr.mp3"),
            (Format::Mp3, "aqr-mp3/tests/data/lsf8.mp3"),
            (Format::Mp3, "aqr-mp3/tests/data/tags.mp3"),
            (Format::Flac, "aqr-flac/tests/data/s16.wav"),
            (Format::Flac, "aqr-flac/tests/data/s24.aiff"),
            (Format::Flac, "aqr-flac/tests/data/s16_sowt.aifc"),
            (Format::Flac, "aqr-flac/tests/data/s16_f5.flac"),
            (Format::Flac, "aqr-flac/tests/data/s24_f8.flac"),
            (Format::Flac, "aqr-flac/tests/data/s16_tags.flac"),
            (Format::Flac, "aqr-flac/tests/data/s16_junk.flac"),
            (Format::Flac, "aqr-flac/tests/data/u8_mono.flac"),
        ] {
            ends(f, &fixture(path));
        }
        ends(Format::Mp3, &fixture("aqr-mp3/tests/data/free.mp3"));
        // Without an end of its own (cut short): to the end of the data.
        let d = [&fixture("aqr-jpeg/tests/data/prog_script_recode.jpg")[..], b"trailing"].concat();
        assert_eq!(extent(Format::Jpeg, &d), Some(d.len()));
    }

    /// A FLAC stream cut short ends before its last frame.
    #[test]
    fn flac_cut() {
        let d = fixture("aqr-flac/tests/data/s16_cut.flac");
        let n = extent(Format::Flac, &d).unwrap();
        assert!(n < d.len() && d.len() - n < 16384, "{n} of {}", d.len());
        assert_eq!(extent(Format::Flac, &[&d[..], &[0xff, 0xf8, 0xc9, 0x18, 0, 0x55, 7, 7]].concat()), Some(n));
    }

    #[test]
    fn mp3_frames() {
        let cbr = fixture("aqr-mp3/tests/data/cbr128.mp3");
        let p = id3_len(&cbr).unwrap_or(0);
        let n = mp3_frame(&cbr[p..], None).unwrap();
        // Two frames are a file, one is not; nor is a frame of another rate after it.
        assert_eq!(extent(Format::Mp3, &cbr[p..p + 2 * n]), Some(2 * n));
        assert_eq!(extent(Format::Mp3, &[&cbr[p..p + n], b"junk"].concat()), None);
        let mut d = cbr[p..p + 2 * n].to_vec();
        d[n + 2] ^= 0x04;
        assert_eq!(extent(Format::Mp3, &d), None);
    }

    fn chunk(ty: &[u8], data: &[u8]) -> Vec<u8> {
        [&(data.len() as u32).to_be_bytes()[..], ty, data, &[0; 4]].concat()
    }

    #[test]
    fn png() {
        let file =
            [&b"\x89PNG\r\n\x1a\n"[..], &chunk(b"IHDR", &[0; 13]), &chunk(b"IDAT", b"xyz"), &chunk(b"IEND", b"")]
                .concat();
        ends(Format::Png, &file);
        assert_eq!(extent(Format::Png, &file[..30]), Some(30));
        let mut bad = file.clone();
        bad[12] = b'?';
        assert_eq!(extent(Format::Png, &bad), None);
    }

    fn psd_file(version: u16, compression: u16, image: &[u8]) -> Vec<u8> {
        let mut d = b"8BPS".to_vec();
        d.extend_from_slice(&version.to_be_bytes());
        d.extend_from_slice(&[0; 6]);
        d.extend_from_slice(&2u16.to_be_bytes()); // channels
        d.extend_from_slice(&3u32.to_be_bytes()); // height
        d.extend_from_slice(&5u32.to_be_bytes()); // width
        d.extend_from_slice(&8u16.to_be_bytes());
        d.extend_from_slice(&3u16.to_be_bytes());
        d.extend_from_slice(&[0, 0, 0, 1, 7]); // color mode data
        d.extend_from_slice(&[0, 0, 0, 0]); // image resources
        d.extend_from_slice(if version == 2 { &[0, 0, 0, 0, 0, 0, 0, 2, 7, 7] } else { &[0, 0, 0, 2, 7, 7] });
        d.extend_from_slice(&compression.to_be_bytes());
        d.extend_from_slice(image);
        d
    }

    #[test]
    fn psd() {
        ends(Format::Psd, &psd_file(1, 0, &[1; 30]));
        ends(Format::Psd, &psd_file(2, 0, &[1; 30]));
        let mut rle = Vec::new();
        (0..6).for_each(|i| rle.extend_from_slice(&(i as u16).to_be_bytes()));
        rle.extend_from_slice(&[9; 15]);
        ends(Format::Psd, &psd_file(1, 1, &rle));
        let mut rle = Vec::new();
        (0..6).for_each(|i| rle.extend_from_slice(&(i as u32).to_be_bytes()));
        rle.extend_from_slice(&[9; 15]);
        ends(Format::Psd, &psd_file(2, 1, &rle));
        // ZIP: to the end.
        let d = psd_file(1, 2, &[1; 30]);
        assert_eq!(extent(Format::Psd, &d), Some(d.len()));
        assert_eq!(extent(Format::Psd, &psd_file(3, 0, &[])), None);
    }

    #[test]
    fn pdf() {
        let first = b"%PDF-1.7\n1 0 obj\n<< >>\nendobj\nxref\n0 1\ntrailer\n<< >>\nstartxref\n9\n%%EOF\r\n".to_vec();
        ends(Format::Pdf, &first);
        // An incremental update, and objects after the first page of a linearized file.
        ends(Format::Pdf, &[&first[..], b"xref\n0 1\ntrailer\n<< >>\n%%EOF\n"].concat());
        ends(Format::Pdf, &[&first[..], b"\n2 0 obj\n<< >>\nendobj\n%%EOF"].concat());
        let d = [&first[..], b"%PDF-1.4\n%%EOF\n"].concat();
        assert_eq!(extent(Format::Pdf, &d), Some(first.len()));
        assert_eq!(extent(Format::Pdf, b"%PDF-1.7\n1 0 obj"), Some(16));
    }

    #[test]
    fn sqlite() {
        let mut d = b"SQLite format 3\0".to_vec();
        d.resize(4096 * 3, 0);
        d[16..18].copy_from_slice(&4096u16.to_be_bytes());
        d[28..32].copy_from_slice(&2u32.to_be_bytes());
        ends(Format::Sqlite, &d[..8192]);
        // A stale count is not trusted.
        d[24] = 1;
        assert_eq!(extent(Format::Sqlite, &d), Some(d.len()));
        d[16] = 3;
        assert_eq!(extent(Format::Sqlite, &d), None);
    }

    #[test]
    fn pe() {
        let mut d = vec![0u8; 0x400];
        d[..2].copy_from_slice(b"MZ");
        d[0x3c] = 0x40;
        d[0x40..0x44].copy_from_slice(b"PE\0\0");
        d[0x46] = 2; // sections
        d[0x54] = 0xe0; // optional header size
        let opt = 0x58;
        d[opt..opt + 2].copy_from_slice(&0x10bu16.to_le_bytes());
        d[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
        d[opt + 92] = 16;
        for (i, (at, size)) in [(0x200u32, 0x100u32), (0x300, 0x80)].into_iter().enumerate() {
            let s = opt + 0xe0 + 40 * i;
            d[s + 16..s + 20].copy_from_slice(&size.to_le_bytes());
            d[s + 20..s + 24].copy_from_slice(&at.to_le_bytes());
        }
        ends(Format::Pe, &d[..0x380]);
        // The certificate table, after the overlay.
        d[opt + 96 + 32..opt + 96 + 36].copy_from_slice(&0x3c0u32.to_le_bytes());
        d[opt + 96 + 36..opt + 96 + 40].copy_from_slice(&0x40u32.to_le_bytes());
        ends(Format::Pe, &d);
    }

    #[test]
    fn flate() {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(&[7; 10000]).unwrap();
        ends(Format::Zlib, &e.finish().unwrap());
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(&[7; 10000]).unwrap();
        ends(Format::Gzip, &e.finish().unwrap());
    }
}
