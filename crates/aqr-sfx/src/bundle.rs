//! The bundle of blobs that a self-extracting program carries after its stub and before its
//! archive (`aqr --format sfx`): the blobs the archive's modules need (native module blobs, the
//! shared libraries they link against, the interpreter blob), each by its name in the program
//! ([`crate::native_name`], [`crate::LIB`], [`crate::INTERP`]), recompressed as Aqueous would
//! recompress code: each blob's code (from its first byte to the end of the last function of its
//! exception table) through aqr-exe's x86 disfilter (`exe-recons/x86`), the streams of every
//! blob's code one after another, by stream, and the rest of the blobs apart, each a zstd frame
//! of its own. The stub ([`unpack`]) places them in memory at run time (`aqr_blob::load`).
//!
//! With the feature `xz`, the same bundle can be made with its frames of raw LZMA2 instead
//! ([`pack_xz`], of the magic `AQRSFXX1`; preset 9, a dictionary no larger than the frame nor than
//! 8 MiB): the pack that aqr carries (`sfx.pack`, `src/bin/aqr-sfx-pack.rs`), which aqr alone reads.
//! A self-extracting program's bundle is of zstd: its stub links no liblzma.
//!
//! ```text
//! bundle = "AQRSFXB2" le64:len frame:meta frame:x86[13] frame:rest
//! frame  = le32:raw le32:n zstd[n]                            (AQRSFXX1: lzma2[n])
//! meta   = count blob*                                         (varints)
//! blob   = str:entry str:name apply_rva info_rva pdata_rva pdata_count import_dir_rva first
//!          image_base count str:lib* count (slot lib rva)* count reloc-delta*
//!          count (rva len flags)* image_len code_len sentinel x86-len[13]
//! str    = len byte[len]
//! ```
//!
//! `len` is the bundle's length, its magic included; the archive comes right after. `rest` is,
//! for each blob in order, its image after its code. The x86 streams of a blob are at its origin
//! `image_base + first`. Every number of `meta` is a varint (aqr-common's), a relocation the
//! difference from the one before it (from 0). The sections are the blob's ([`Section`]), by
//! which the stub protects its pages.

use aqr_blob::{Bind, Blob, Section};
use aqr_common::varint;
use aqr_exe::arch::x86;

pub const MAGIC: &[u8; 8] = b"AQRSFXB2";

/// The magic of a bundle of raw LZMA2 frames ([`pack_xz`]).
pub const MAGIC_XZ: &[u8; 8] = b"AQRSFXX1";

/// What a bundle's frames are compressed by.
#[derive(Clone, Copy)]
enum Codec {
    Zstd,
    #[cfg(feature = "xz")]
    Xz,
}

/// The frames of a bundle: `meta`, the x86 streams, `rest`.
const FRAMES: usize = x86::NUM_STREAMS + 2;

/// The length of the bundle at the start of `d`, if one is there (else `None`).
pub fn len(d: &[u8]) -> Option<usize> {
    len_of(d, MAGIC)
}

fn len_of(d: &[u8], magic: &[u8; 8]) -> Option<usize> {
    let n = d.strip_prefix(magic)?.first_chunk::<8>()?;
    usize::try_from(u64::from_le_bytes(*n)).ok().filter(|&n| (MAGIC.len() + 8..=d.len()).contains(&n))
}

/// The end of the code of `b`, as an offset in its image: the end of the last function of its
/// exception table (code comes first in the image, `.text` being the first section); 0 where it
/// has none.
#[cfg(feature = "encode")]
fn code_len(b: &Blob) -> usize {
    let Some(at) = (b.pdata_rva.checked_sub(b.first)).map(|x| x as usize) else { return 0 };
    let table = b.image.get(at..at + 12 * b.pdata_count as usize).unwrap_or_default();
    let end = table.as_chunks::<12>().0.iter().map(|f| u32::from_le_bytes(f[4..8].try_into().unwrap())).max();
    end.and_then(|e| e.checked_sub(b.first)).map_or(0, |e| (e as usize).min(b.image.len()))
}

/// The bundle of `blobs` (each its name in the program and its bytes, `Blob::to_bytes`).
#[cfg(feature = "encode")]
pub fn pack(blobs: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    pack_with(blobs, Codec::Zstd)
}

/// [`pack`] with frames of raw LZMA2 ([`MAGIC_XZ`]).
#[cfg(all(feature = "encode", feature = "xz"))]
pub fn pack_xz(blobs: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    pack_with(blobs, Codec::Xz)
}

#[cfg(feature = "encode")]
fn pack_with(blobs: &[(String, Vec<u8>)], codec: Codec) -> Result<Vec<u8>, String> {
    let mut meta = Vec::new();
    let mut streams: [Vec<u8>; x86::NUM_STREAMS] = std::array::from_fn(|_| Vec::new());
    let mut rest = Vec::new();
    let put = |m: &mut Vec<u8>, v: u64| varint::put(m, v);
    let put_str = |m: &mut Vec<u8>, s: &str| {
        varint::put(m, s.len() as u64);
        m.extend_from_slice(s.as_bytes());
    };
    put(&mut meta, blobs.len() as u64);
    for (entry, bytes) in blobs {
        let b = Blob::from_bytes(bytes).map_err(|e| format!("{entry}: {e}"))?;
        put_str(&mut meta, entry);
        put_str(&mut meta, &b.name);
        for v in [b.apply_rva, b.info_rva, b.pdata_rva, b.pdata_count, b.import_dir_rva, b.first] {
            put(&mut meta, v.into());
        }
        put(&mut meta, b.image_base);
        let mut libs: Vec<&str> = Vec::new();
        for bind in &b.binds {
            if !libs.contains(&&bind.lib[..]) {
                libs.push(&bind.lib);
            }
        }
        put(&mut meta, libs.len() as u64);
        libs.iter().for_each(|l| put_str(&mut meta, l));
        put(&mut meta, b.binds.len() as u64);
        for bind in &b.binds {
            put(&mut meta, bind.slot.into());
            put(&mut meta, libs.iter().position(|l| *l == bind.lib).unwrap() as u64);
            put(&mut meta, bind.rva.into());
        }
        put(&mut meta, b.relocs.len() as u64);
        let mut last = 0u32;
        for &r in &b.relocs {
            put(&mut meta, u64::from(r.wrapping_sub(last)));
            last = r;
        }
        put(&mut meta, b.sections.len() as u64);
        for s in &b.sections {
            [s.rva, s.len, s.flags].into_iter().for_each(|v| put(&mut meta, v.into()));
        }
        // the code through the disfilter (else none of it: the image as it is)
        let mut n = code_len(&b);
        let origin = b.image_base.wrapping_add(b.first.into());
        let split = (n > 0).then(|| x86::encode(&b.image[..n], origin, true)).flatten();
        let split = split.filter(|st| {
            let back = x86::decode_with(&st.into(), true, &x86::Opcodes::BUILTIN, n);
            back.as_deref() == Some(&b.image[..n])
        });
        if split.is_none() {
            n = 0;
        }
        put(&mut meta, b.image.len() as u64);
        put(&mut meta, n as u64);
        put(&mut meta, split.as_ref().map_or(0, |st| st.sentinel_size()).into());
        for (i, s) in streams.iter_mut().enumerate() {
            let st = split.as_ref().map_or(&[][..], |st| &st.streams()[i][..]);
            put(&mut meta, st.len() as u64);
            s.extend_from_slice(st);
        }
        rest.extend_from_slice(&b.image[n..]);
    }
    let mut frames: Vec<&[u8]> = vec![&meta];
    frames.extend(streams.iter().map(|s| &s[..]));
    frames.push(&rest);
    // (the frames compressed at once, each on a thread of its own)
    let frames: Vec<Vec<u8>> = std::thread::scope(|s| {
        let hs: Vec<_> = frames.iter().map(|&f| s.spawn(move || frame(f, codec))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;
    let mut out = match codec {
        Codec::Zstd => MAGIC.to_vec(),
        #[cfg(feature = "xz")]
        Codec::Xz => MAGIC_XZ.to_vec(),
    };
    let len = MAGIC.len() + 8 + frames.iter().map(Vec::len).sum::<usize>();
    out.extend_from_slice(&(len as u64).to_le_bytes());
    frames.iter().for_each(|f| out.extend_from_slice(f));
    Ok(out)
}

/// zstd's level for the frames (the bundle is made once a program).
#[cfg(feature = "encode")]
const LEVEL: i32 = 19;

/// `d` as a frame: its length, and compressed by `codec` (with its length before it).
#[cfg(feature = "encode")]
fn frame(d: &[u8], codec: Codec) -> Result<Vec<u8>, String> {
    let z = match codec {
        Codec::Zstd => zstd_frame(d)?,
        #[cfg(feature = "xz")]
        Codec::Xz => xz::compress(d)?,
    };
    let mut f = Vec::with_capacity(8 + z.len());
    f.extend_from_slice(&(d.len() as u32).to_le_bytes());
    f.extend_from_slice(&(z.len() as u32).to_le_bytes());
    f.extend(z);
    Ok(f)
}

#[cfg(feature = "encode")]
fn zstd_frame(d: &[u8]) -> Result<Vec<u8>, String> {
    use zstd::zstd_safe::CParameter;
    let mut z = zstd::bulk::Compressor::new(LEVEL).map_err(|e| e.to_string())?;
    // (a window as large as the data, and no larger than the decoder takes by default)
    let log = (usize::BITS - d.len().max(1).leading_zeros()).clamp(10, 27);
    for p in [CParameter::WindowLog(log), CParameter::ContentSizeFlag(false), CParameter::ChecksumFlag(false)] {
        z.set_parameter(p).map_err(|e| e.to_string())?;
    }
    z.compress(d).map_err(|e| e.to_string())
}

/// Raw LZMA2 frames (the bundles of [`MAGIC_XZ`]).
#[cfg(feature = "xz")]
mod xz {
    use liblzma::stream::{Action, Error, Filters, LzmaOptions, Status, Stream};

    /// The dictionary of a frame of `raw` bytes: as large as the frame (a power of two), 8 MiB
    /// at most, so that decoding takes little memory.
    fn dict(raw: usize) -> u32 {
        raw.max(1).next_power_of_two().clamp(1 << 12, 8 << 20) as u32
    }

    fn filters(raw: usize) -> Result<Filters, Error> {
        let mut opts = LzmaOptions::new_preset(9)?;
        opts.dict_size(dict(raw));
        let mut f = Filters::new();
        f.lzma2(&opts);
        Ok(f)
    }

    #[cfg(feature = "encode")]
    pub fn compress(d: &[u8]) -> Result<Vec<u8>, String> {
        let e = |e: Error| format!("LZMA2: {e}");
        let mut s = Stream::new_raw_encoder(&filters(d.len()).map_err(e)?).map_err(e)?;
        let mut out = Vec::with_capacity(d.len() / 4 + 4096);
        loop {
            let at = s.total_in() as usize;
            if out.capacity() - out.len() < 4096 {
                out.reserve(out.capacity() / 2 + 4096);
            }
            if s.process_vec(&d[at..], &mut out, Action::Finish).map_err(e)? == Status::StreamEnd {
                return Ok(out);
            }
        }
    }

    /// The `raw` bytes of frame `z` (more or fewer: the caller tells).
    pub fn decompress(z: &[u8], raw: usize) -> Result<Vec<u8>, String> {
        let e = |e: Error| format!("the bundle of blobs: LZMA2: {e}");
        let mut s = Stream::new_raw_decoder(&filters(raw).map_err(e)?).map_err(e)?;
        // (a byte more than it should be, so that a frame of more is told)
        let mut out = Vec::with_capacity(raw + 1);
        loop {
            let (at, had) = (s.total_in() as usize, out.len());
            match s.process_vec(&z[at..], &mut out, Action::Finish).map_err(e)? {
                Status::StreamEnd => return Ok(out),
                _ if s.total_in() as usize == at && out.len() == had => {
                    return Err("the bundle of blobs: a frame cut short".into());
                }
                _ => {}
            }
        }
    }
}

/// The blobs of bundle `d` (as [`pack`] took them: each its name in the program and its bytes).
///
/// A bundle of [`MAGIC_XZ`] is taken with the feature `xz` alone.
pub fn unpack(d: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let cut = || "the bundle of blobs is cut short".to_string();
    let (n, codec) = match (len(d), len_of(d, MAGIC_XZ)) {
        (Some(n), _) => (n, Codec::Zstd),
        #[cfg(feature = "xz")]
        (None, Some(n)) => (n, Codec::Xz),
        #[cfg(not(feature = "xz"))]
        (None, Some(_)) => return Err("a bundle of blobs of LZMA2, which this program does not read".into()),
        (None, None) => return Err("not a bundle of blobs".into()),
    };
    let mut d = &d[MAGIC.len() + 8..n];
    let mut zs: Vec<(usize, &[u8])> = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let (head, r) = d.split_first_chunk::<8>().ok_or_else(cut)?;
        let raw = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
        let (z, r) = r.split_at_checked(u32::from_le_bytes(head[4..].try_into().unwrap()) as usize).ok_or_else(cut)?;
        zs.push((raw, z));
        d = r;
    }
    // (the frames decompressed at once, each on a thread of its own)
    let frames: Vec<Vec<u8>> = std::thread::scope(|s| {
        let hs: Vec<_> = zs
            .iter()
            .map(|&(raw, z)| {
                s.spawn(move || {
                    let f = match codec {
                        Codec::Zstd => zstd::bulk::decompress(z, raw).map_err(|e| format!("the bundle of blobs: {e}")),
                        #[cfg(feature = "xz")]
                        Codec::Xz => xz::decompress(z, raw),
                    }?;
                    if f.len() == raw { Ok(f) } else { Err("a frame of the bundle of another length".to_string()) }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;
    let (meta, streams, rest) = (&frames[0], &frames[1..FRAMES - 1], &frames[FRAMES - 1]);
    let bad = || "the bundle of blobs is not valid".to_string();
    let mut r = Meta { d: meta, at: 0 };
    let count = r.u64()? as usize;
    let (mut st_at, mut rest_at) = ([0usize; x86::NUM_STREAMS], 0usize);
    let mut out = Vec::with_capacity(count.min(1 << 12));
    for _ in 0..count {
        let entry = r.str()?;
        let name = r.str()?;
        let [apply_rva, info_rva, pdata_rva, pdata_count, import_dir_rva, first] =
            [(); 6].map(|_| r.u32()).into_iter().collect::<Result<Vec<_>, _>>()?.try_into().unwrap();
        let image_base = r.u64()?;
        let libs = (0..r.u64()?).map(|_| r.str()).collect::<Result<Vec<_>, _>>()?;
        let mut binds = Vec::new();
        for _ in 0..r.u64()? {
            let (slot, lib, rva) = (r.u32()?, r.u64()? as usize, r.u32()?);
            binds.push(Bind { slot, lib: libs.get(lib).ok_or_else(bad)?.clone(), rva });
        }
        let mut relocs = Vec::new();
        let mut last = 0u32;
        for _ in 0..r.u64()? {
            last = last.wrapping_add(r.u32()?);
            relocs.push(last);
        }
        let mut sections = Vec::new();
        for _ in 0..r.u64()? {
            sections.push(Section { rva: r.u32()?, len: r.u32()?, flags: r.u32()? });
        }
        let (image_len, code_len, sentinel) = (r.u64()? as usize, r.u64()? as usize, r.u64()?);
        let mut parts: [&[u8]; x86::NUM_STREAMS] = [&[]; x86::NUM_STREAMS];
        for (i, p) in parts.iter_mut().enumerate() {
            let n = r.u64()? as usize;
            *p = streams[i].get(st_at[i]..st_at[i].checked_add(n).ok_or_else(bad)?).ok_or_else(bad)?;
            st_at[i] += n;
        }
        let mut image = Vec::with_capacity(image_len);
        if code_len > 0 {
            let origin = image_base.wrapping_add(first.into());
            let st = x86::DecodedStreams::new(origin, u8::try_from(sentinel).map_err(|_| bad())?, parts);
            let code = x86::decode_with(&st, true, &x86::Opcodes::BUILTIN, code_len);
            image = code.filter(|c| c.len() == code_len).ok_or_else(|| format!("{entry}: its code not decoded"))?;
            image.reserve(image_len.saturating_sub(code_len));
        }
        let left = image_len.checked_sub(code_len).ok_or_else(bad)?;
        image.extend_from_slice(rest.get(rest_at..rest_at + left).ok_or_else(bad)?);
        rest_at += left;
        let b = Blob {
            apply_rva,
            info_rva,
            pdata_rva,
            pdata_count,
            import_dir_rva,
            first,
            image_base,
            name,
            binds,
            relocs,
            sections,
            image,
        };
        out.push((entry, b.to_bytes()));
    }
    if r.at != meta.len() || rest_at != rest.len() || st_at.iter().zip(streams).any(|(&a, s)| a != s.len()) {
        return Err(bad());
    }
    Ok(out)
}

/// The varints and strings of a bundle's `meta`, read in order.
struct Meta<'a> {
    d: &'a [u8],
    at: usize,
}

impl Meta<'_> {
    fn u64(&mut self) -> Result<u64, String> {
        varint::get(self.d, &mut self.at).map_err(|_| "the bundle of blobs is not valid".to_string())
    }

    fn u32(&mut self) -> Result<u32, String> {
        u32::try_from(self.u64()?).map_err(|_| "the bundle of blobs is not valid".to_string())
    }

    fn str(&mut self) -> Result<String, String> {
        let n = self.u64()? as usize;
        let s = self.d.get(self.at..self.at.saturating_add(n)).ok_or("the bundle of blobs is not valid")?;
        self.at += n;
        String::from_utf8(s.to_vec()).map_err(|_| "a name of the bundle not of UTF-8".to_string())
    }
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use aqr_blob::{Bind, Blob, SCN_EXECUTE, SCN_READ, SCN_WRITE, Section};

    /// Blobs of code (an exception table naming its end), of none, and with binds and
    /// relocations, bundled and back as they were; a bundle cut short is not taken.
    #[test]
    fn blobs_round_trip() {
        // (a few functions: push rbp; mov rbp, rsp; call; lea rax, [rip+...]; pop rbp; ret, padded)
        let f = [0x55, 0x48, 0x89, 0xe5, 0xe8, 0x10, 0, 0, 0, 0x48, 0x8d, 0x05, 0x20, 0, 0, 0, 0x5d, 0xc3];
        let mut code: Vec<u8> = (0..40).flat_map(|_| f.iter().copied().chain([0xcc; 14])).collect();
        let code_len = code.len() as u32;
        let first = 0x1000;
        // the exception table after the code, of one function over all of it
        let pdata_rva = first + code_len;
        code.extend([first, first + code_len, 0].iter().flat_map(|v| v.to_le_bytes()));
        code.extend(b"data after the code".repeat(10));
        let blob = |name: &str, image: Vec<u8>, pdata_count| Blob {
            apply_rva: first + 32,
            info_rva: first,
            pdata_rva,
            pdata_count,
            import_dir_rva: 0,
            first,
            image_base: 0x1_8000_0000,
            name: name.into(),
            binds: vec![Bind { slot: pdata_rva + 8, lib: "aqr_dll_common.dll".into(), rva: 0x1234 }],
            relocs: vec![first + 100, first + 300, first + 200],
            sections: vec![
                Section { rva: first, len: code_len, flags: SCN_EXECUTE | SCN_READ },
                Section { rva: first + 0x1000, len: 16, flags: SCN_READ | SCN_WRITE },
            ],
            image,
        };
        let blobs = vec![
            ("ab/01".to_string(), blob("", code.clone(), 1).to_bytes()),
            ("lib:aqr_dll_common.dll".to_string(), blob("aqr_dll_common.dll", code[..600].to_vec(), 0).to_bytes()),
            ("cd/".to_string(), blob("", code, 1).to_bytes()),
        ];
        let p = super::pack(&blobs).unwrap();
        assert_eq!(super::len(&p), Some(p.len()));
        assert!(super::unpack(&p).unwrap() == blobs);
        assert!(super::unpack(&p[..p.len() - 1]).is_err());
        #[cfg(feature = "xz")]
        {
            let x = super::pack_xz(&blobs).unwrap();
            assert_eq!((super::len(&x), super::len_of(&x, super::MAGIC_XZ)), (None, Some(x.len())));
            assert!(super::unpack(&x).unwrap() == blobs);
            assert!(super::unpack(&x[..x.len() - 1]).is_err());
            // (a frame of LZMA2 cut short: its last byte, the end of the stream, gone)
            let (raw, n) = (&x[16..20], u32::from_le_bytes(x[20..24].try_into().unwrap()) as usize);
            let raw = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
            assert!(super::xz::decompress(&x[24..24 + n - 1], raw).is_err());
        }
    }

    /// The blobs of the pack (`target/aqr-sfx/`, crates/aqr-sfx/pack.py) bundled and back, with
    /// their sizes: as they are, by zstd alone, bundled (and bundled of LZMA2, with `xz`).
    #[test]
    #[ignore]
    fn pack_round_trips() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/aqr-sfx");
        let mut blobs = Vec::new();
        for sub in ["libs", "modules"] {
            let Ok(rd) = std::fs::read_dir(dir.join(sub)) else {
                eprintln!("skipped: no {}", dir.display());
                return;
            };
            for e in rd {
                let p = e.unwrap().path();
                blobs.push((p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()));
            }
        }
        blobs.sort();
        blobs.push(("aqr-interp".into(), std::fs::read(dir.join("aqr-interp.blob")).unwrap()));
        let raw: usize = blobs.iter().map(|b| b.1.len()).sum();
        let whole: Vec<u8> = blobs.iter().flat_map(|b| b.1.iter().copied()).collect();
        let t = std::time::Instant::now();
        let z = super::frame(&whole, super::Codec::Zstd).unwrap().len();
        let zt = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let p = super::pack(&blobs).unwrap();
        let pt = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let back = super::unpack(&p).unwrap();
        let ut = t.elapsed().as_secs_f64();
        assert!(back == blobs);
        eprintln!(
            "{} blobs: {raw} bytes; zstd {z} ({zt:.1} s); bundled {} ({pt:.1} s, unpacked in {ut:.2} s)",
            blobs.len(),
            p.len()
        );
        #[cfg(feature = "xz")]
        {
            let t = std::time::Instant::now();
            let x = super::pack_xz(&blobs).unwrap();
            let xt = t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            assert!(super::unpack(&x).unwrap() == blobs);
            eprintln!("bundled of LZMA2 {} ({xt:.1} s, unpacked in {:.2} s)", x.len(), t.elapsed().as_secs_f64());
        }
    }
}
