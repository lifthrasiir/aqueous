//! Picking a recompressor by the magic bytes at the start of the data.

use std::fmt;

/// What [`detect`] recognises, by the recompressor that takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// PE image (exe-recons on its executable sections).
    Pe,
    /// gzip member (flate-recons on its DEFLATE stream).
    Gzip,
    /// zlib stream (flate-recons on its DEFLATE stream).
    Zlib,
    Png,
    Jpeg,
    Mp3,
    /// FLAC, WAV or AIFF (flac-recons).
    Flac,
    /// PSD or PSB.
    Psd,
    Pdf,
    Sqlite,
}

impl Format {
    pub const ALL: [Format; 10] = [
        Format::Pe,
        Format::Gzip,
        Format::Zlib,
        Format::Png,
        Format::Jpeg,
        Format::Mp3,
        Format::Flac,
        Format::Psd,
        Format::Pdf,
        Format::Sqlite,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Format::Pe => "pe",
            Format::Gzip => "gzip",
            Format::Zlib => "zlib",
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
            Format::Psd => "psd",
            Format::Pdf => "pdf",
            Format::Sqlite => "sqlite",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The format `d` starts like, if any. Only the first bytes are looked at (and the PE
/// header, the end of an ID3v2 tag): the recompressor may still turn the data down.
pub fn detect(d: &[u8]) -> Option<Format> {
    let at = |p: usize, m: &[u8]| d.get(p..p + m.len()) == Some(m);
    if at(0, b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if at(0, b"SQLite format 3\0") {
        Some(Format::Sqlite)
    } else if at(0, b"%PDF-") {
        Some(Format::Pdf)
    } else if at(0, b"8BPS") {
        Some(Format::Psd)
    } else if at(0, b"fLaC") || at(0, b"RIFF") && at(8, b"WAVE") || at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC"))
    {
        Some(Format::Flac)
    } else if at(0, b"ID3") && d.len() >= 10 && d[6..10].iter().all(|&b| b < 0x80) {
        // ID3v2 is found before either; what follows the tag tells them apart.
        let size = d[6..10].iter().fold(0usize, |a, &b| a << 7 | b as usize);
        let p = 10 + size + if d[5] & 0x10 != 0 { 10 } else { 0 };
        Some(if at(p, b"fLaC") { Format::Flac } else { Format::Mp3 })
    } else if at(0, &[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if mp3_header(d) {
        Some(Format::Mp3)
    } else if pe(d) {
        Some(Format::Pe)
    } else if at(0, &[0x1f, 0x8b, 8]) {
        Some(Format::Gzip)
    } else if zlib_header(d) {
        Some(Format::Zlib)
    } else {
        None
    }
}

/// An MPEG audio layer III frame header.
fn mp3_header(d: &[u8]) -> bool {
    let &[0xff, b1, b2, ..] = d else { return false };
    let (sync, version, layer) = (b1 >> 5, b1 >> 3 & 3, b1 >> 1 & 3);
    let (bitrate, rate) = (b2 >> 4, b2 >> 2 & 3);
    sync == 7 && version != 1 && layer == 1 && bitrate != 15 && rate != 3
}

/// "MZ" with `e_lfanew` pointing at "PE\0\0".
fn pe(d: &[u8]) -> bool {
    let Some(&[b'M', b'Z']) = d.first_chunk() else { return false };
    let Some(&lfanew) = d.get(0x3c..0x40).and_then(|s| s.first_chunk::<4>()) else { return false };
    let p = u32::from_le_bytes(lfanew) as usize;
    p >= 0x40 && d.get(p..p.saturating_add(4)) == Some(b"PE\0\0")
}

/// A zlib header (RFC 1950) for DEFLATE without a preset dictionary.
pub(crate) fn zlib_header(d: &[u8]) -> bool {
    let Some(&[cmf, flg]) = d.first_chunk() else { return false };
    cmf & 15 == 8 && cmf >> 4 <= 7 && flg & 0x20 == 0 && (cmf as u16 * 256 + flg as u16).is_multiple_of(31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magics() {
        let mut pe = vec![0u8; 0x84];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x80;
        pe[0x80..].copy_from_slice(b"PE\0\0");
        let mut id3_flac = b"ID3\x04\0\0\0\0\0\x02xx".to_vec();
        id3_flac.extend_from_slice(b"fLaC");
        let cases: &[(&[u8], Option<Format>)] = &[
            (b"\x89PNG\r\n\x1a\n....", Some(Format::Png)),
            (b"SQLite format 3\0....", Some(Format::Sqlite)),
            (b"%PDF-1.7\n", Some(Format::Pdf)),
            (b"8BPS\0\x01", Some(Format::Psd)),
            (b"8BPS\0\x02", Some(Format::Psd)),
            (b"fLaC\0\0\0\x22", Some(Format::Flac)),
            (b"RIFF\0\0\0\0WAVEfmt ", Some(Format::Flac)),
            (b"FORM\0\0\0\0AIFCFVER", Some(Format::Flac)),
            (&id3_flac, Some(Format::Flac)),
            (b"ID3\x04\0\0\0\0\0\x02xx\xff\xfb\x90\x00", Some(Format::Mp3)),
            (b"\xff\xfb\x90\x00", Some(Format::Mp3)),
            (b"\xff\xf3\x48\xc4", Some(Format::Mp3)),
            (b"\xff\xfd\x90\x00", None), // layer II
            (b"\xff\xd8\xff\xe0", Some(Format::Jpeg)),
            (&pe, Some(Format::Pe)),
            (b"MZ\x90\0", None),
            (b"\x1f\x8b\x08\0", Some(Format::Gzip)),
            (b"\x78\x9c", Some(Format::Zlib)),
            (b"\x78\xda", Some(Format::Zlib)),
            (b"\x78\x9d", None),
            (b"\x78\xbb", None), // preset dictionary
            (b"hello", None),
            (b"", None),
        ];
        for &(d, f) in cases {
            assert_eq!(detect(d), f, "{d:x?}");
        }
    }
}
