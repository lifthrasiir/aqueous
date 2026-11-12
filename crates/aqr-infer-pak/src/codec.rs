//! Codecs of a directory's members: a member is coded where a decoder takes it whole (up to
//! 16 bytes of zeros after its stream).

use flate2::{Decompress, FlushDecompress, Status};

/// Decoded output at most (a member's probe).
pub const LIMIT: usize = 256 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Zstd,
    Zlib,
    Deflate,
    Lzo1x,
    Lz4Block,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Zstd => "zstd",
            Codec::Zlib => "zlib",
            Codec::Deflate => "deflate",
            Codec::Lzo1x => "lzo1x",
            Codec::Lz4Block => "lz4b",
        }
    }
}

fn whole(src: &[u8], used: usize) -> bool {
    used + 16 >= src.len() && used <= src.len() && src[used..].iter().all(|&x| x == 0)
}

fn zstd_frames(src: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while src.len() - i >= 4 && src[i..i + 4] == [0x28, 0xb5, 0x2f, 0xfd] {
        let n = zstd::zstd_safe::find_frame_compressed_size(&src[i..]).ok()?;
        let cap = match zstd::zstd_safe::get_frame_content_size(&src[i..]) {
            Ok(Some(c)) if (c as usize) <= LIMIT => c as usize,
            Ok(Some(_)) => return None,
            _ => LIMIT,
        };
        out.extend(zstd::bulk::decompress(&src[i..i + n], cap).ok()?);
        i += n;
    }
    if i == 0 { None } else { Some((out, i)) }
}

fn inflate(src: &[u8], zlib: bool) -> Option<(Vec<u8>, usize)> {
    let mut d = Decompress::new(zlib);
    let mut out = Vec::with_capacity((src.len() * 4).min(LIMIT));
    loop {
        if out.capacity() == out.len() {
            if out.len() >= LIMIT {
                return None;
            }
            out.reserve(out.len().max(4096));
        }
        let (at, had) = (d.total_in() as usize, out.len());
        if d.decompress_vec(&src[at..], &mut out, FlushDecompress::Finish).ok()? == Status::StreamEnd {
            return Some((out, d.total_in() as usize));
        }
        if d.total_in() as usize == at && out.len() == had {
            return None; // no progress: the input ended within the stream
        }
    }
}

/// The codec decoding `src` whole and its data. Headerless codecs only for 32 bytes or
/// more; an LZ4 block (which ends where its input does) only where its decoded size is among
/// `hints` (the values of its row's fields).
pub fn probe(src: &[u8], hints: &[u64]) -> Option<(Codec, Vec<u8>)> {
    let zlib = src.len() >= 2
        && src[0] & 0x0f == 8
        && src[0] >> 4 <= 7
        && (src[0] as u32 * 256 + src[1] as u32).is_multiple_of(31);
    if src.len() >= 4
        && src[..4] == [0x28, 0xb5, 0x2f, 0xfd]
        && let Some((d, used)) = zstd_frames(src)
        && whole(src, used)
    {
        return Some((Codec::Zstd, d));
    }
    if zlib
        && let Some((d, used)) = inflate(src, true)
        && whole(src, used)
    {
        return Some((Codec::Zlib, d));
    }
    if src.len() < 32 {
        return None;
    }
    if let Ok(d) = aqr_lzo::lzo1x::decode(src, LIMIT)
        && whole(src, d.used)
    {
        return Some((Codec::Lzo1x, d.data));
    }
    if let Some((d, used)) = inflate(src, false)
        && whole(src, used)
        && d.len() >= src.len() / 2
    {
        return Some((Codec::Deflate, d));
    }
    let mut out = Vec::new();
    if aqr_lz4::block::decode(src, &mut out, 0, LIMIT).is_ok() && hints.contains(&(out.len() as u64)) {
        return Some((Codec::Lz4Block, out));
    }
    None
}

/// A member's decoded size without decoding it, where its codec's header tells.
pub fn decoded_size(src: &[u8]) -> Option<u64> {
    if src.len() >= 4 && src[..4] == [0x28, 0xb5, 0x2f, 0xfd] {
        let n = zstd::zstd_safe::find_frame_compressed_size(src).ok()?;
        if n + 16 >= src.len() {
            return zstd::zstd_safe::get_frame_content_size(src).ok().flatten();
        }
    }
    None
}

/// Decodes a member known to be of `codec`.
pub fn decode(codec: Codec, src: &[u8]) -> Option<Vec<u8>> {
    match codec {
        Codec::Zstd => zstd_frames(src).map(|x| x.0),
        Codec::Zlib => inflate(src, true).map(|x| x.0),
        Codec::Deflate => inflate(src, false).map(|x| x.0),
        Codec::Lzo1x => aqr_lzo::lzo1x::decode(src, LIMIT).ok().map(|d| d.data),
        Codec::Lz4Block => {
            let mut out = Vec::new();
            aqr_lz4::block::decode(src, &mut out, 0, LIMIT).ok().map(|_| out)
        }
    }
}
