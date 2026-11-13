//! XNA's and MonoGame's compiled content (`.xnb`, `xna-xnb`), as Microsoft's "Compiled (XNB)
//! Content Format" describes it, little endian: `XNB`, the target platform (a byte: `w`
//! Windows, `m` Phone, `x` Xbox 360; MonoGame's others), the format's version (a byte: 5 for
//! XNA 4.0, 4 and 3 before), flags (a byte: 0x01 HiDef, 0x80 compressed by LZX, 0x40 by LZ4) and
//! the file's size (u32). A compressed file is not read here (left for its codec's work).
//!
//! Then the type readers (a count, then each its name and version: a string of a 7-bit encoded
//! length, an i32), the shared resources' count, and the primary object: its type reader's index
//! plus 1 (7-bit encoded), its data by its reader. Read here:
//!
//! - `Texture2DReader`: the surface format (i32: 0 Color, RGBA of 8 bits; 4, 5, 6 DXT1, DXT3,
//!   DXT5), the width, the height and the mipmaps (u32 each), then each mipmap's size (u32) and
//!   bytes;
//! - `SpriteFontReader`: its texture first (its reader's index plus 1, then a texture's data);
//! - `SoundEffectReader`: the format's size (u32) and its `WAVEFORMATEX`, the data's size (u32)
//!   and the samples, then the loop and the duration.
//!
//! A texture's mipmaps (of the sizes their format makes) and a sound's samples (PCM of 8 or 16
//! bits) are files; the rest (the header, the readers, the fields between) is the rest. The
//! formula of its own ([`encode_with`]) rebuilds the mipmaps' pixels (aqr-tex's `bcn-recons`,
//! aqr-raw's `raster-recons`) and the samples (aqr-raw's `pcm-recons`).

use super::unity_assets::{Pixels, Raw, apply_pixels};
use super::{file, u16le, u32le, within};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"XNB")
        && d.get(4).is_some_and(|v| (3..=5).contains(v))
        && u32le(d, 6).is_some_and(|n| n as usize == d.len())
}

/// A 7-bit encoded integer at `*at` of `d`.
fn v7(d: &[u8], at: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let b = *d.get(*at)?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Some(v);
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

/// What the file holds that is taken: a texture's mipmaps (each its bytes and pixels), or a
/// sound's samples (their bytes and format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asset {
    Texture(Vec<(std::ops::Range<usize>, Pixels)>),
    Sound(std::ops::Range<usize>, aqr_raw::audio::pcm::Format),
}

/// A texture's data at `*at` of `d`: its mipmaps, each of the size its format makes.
fn texture(d: &[u8], at: &mut usize) -> Option<Vec<(std::ops::Range<usize>, Pixels)>> {
    use aqr_tex::layout::{Format, Layout};
    let format = u32le(d, *at)?;
    let (w, h, mips) = (u32le(d, *at + 4)?, u32le(d, *at + 8)?, u32le(d, *at + 12)?);
    *at += 16;
    if mips == 0 || mips > 17 {
        return None;
    }
    let mut out = Vec::with_capacity(mips as usize);
    for m in 0..mips {
        let (width, height) = ((w >> m).max(1), (h >> m).max(1));
        let pixels = match format {
            0 => Pixels::Raw(Raw { channels: 4, width, height, mips: 1, layers: 1 }),
            4..=6 => {
                let format = [Format::Bc1, Format::Bc2, Format::Bc3][format as usize - 4];
                Pixels::Blocks(Layout { format, width, height, mips: 1, layers: 1 })
            }
            _ => return None,
        };
        let size = u32le(d, *at)? as u64;
        let r = within(d, *at as u64 + 4, size)?;
        if pixels.len() != size {
            return None;
        }
        *at = r.end;
        out.push((r, pixels));
    }
    Some(out)
}

/// The asset of file `d`; None where it is not one, is compressed, or holds no asset taken.
pub fn asset(d: &[u8]) -> Option<Asset> {
    if !starts(d) || d[5] & 0xc0 != 0 {
        return None;
    }
    let mut at = 10;
    let n = v7(d, &mut at)? as usize;
    if n > 64 {
        return None;
    }
    let mut readers = Vec::with_capacity(n);
    for _ in 0..n {
        let len = v7(d, &mut at)? as usize;
        let name = d.get(at..at.checked_add(len)?)?;
        at += len + 4;
        // (the reader's type without its assembly: `Microsoft.Xna.Framework.Content.Texture2DReader`)
        let name = name.split(|&b| b == b',').next().unwrap_or(name);
        readers.push(name.rsplit(|&b| b == b'.').next().unwrap_or(name).to_vec());
    }
    v7(d, &mut at)?;
    let reader = |at: &mut usize| -> Option<&[u8]> {
        let k = v7(d, at)? as usize;
        Some(&readers.get(k.checked_sub(1)?)?[..])
    };
    match reader(&mut at)? {
        b"Texture2DReader" => texture(d, &mut at).map(Asset::Texture),
        b"SpriteFontReader" if reader(&mut at)? == b"Texture2DReader" => texture(d, &mut at).map(Asset::Texture),
        b"SoundEffectReader" => {
            let fmt = u32le(d, at)? as usize;
            let w = d.get(at + 4..(at + 4).checked_add(fmt)?)?;
            at += 4 + fmt;
            let (tag, channels, bits) = (u16le(w, 0)?, u16le(w, 2)? as usize, u16le(w, 14)? as usize);
            let size = u32le(d, at)? as u64;
            let r = within(d, at as u64 + 4, size)?;
            let pcm = aqr_raw::audio::pcm::Format { channels, bytes: bits / 8, big_endian: false, unsigned: bits == 8 };
            (tag == 1 && (bits == 8 || bits == 16) && pcm.valid() && r.len().is_multiple_of(channels * bits / 8))
                .then_some(Asset::Sound(r, pcm))
        }
        _ => None,
    }
}

/// The files of `d`: a texture's mipmaps, or a sound's samples.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    Some(match asset(d)? {
        Asset::Texture(mips) => {
            mips.into_iter().enumerate().map(|(k, (r, _))| file(format!("mip{k}.pixels").into_bytes(), r)).collect()
        }
        Asset::Sound(r, _) => vec![file(b"samples.pcm".to_vec(), r)],
    })
}

/// The formula of `d`: its mipmaps' pixels or its samples rebuilt, the rest the stream
/// `xna-xnb/rest`. None where it holds no asset taken.
pub fn encode_with(d: &[u8], threads: usize) -> Option<aqr_formula::Formula> {
    let mut p = super::Pieces::new(d, "xna-xnb", |_| aqr_formula::Kind::Unknown);
    match asset(d)? {
        Asset::Texture(mips) => {
            for (r, pixels) in mips {
                if let Some(node) = apply_pixels(&mut p.f, &d[r.clone()], &pixels, threads) {
                    p.node(node, r);
                }
            }
        }
        Asset::Sound(r, pcm) => {
            let node = aqr_raw::formula::apply(&mut p.f, &d[r.clone()], aqr_raw::container::Layout::Pcm(pcm), threads)?;
            p.node(node, r);
        }
    }
    Some(p.finish())
}
