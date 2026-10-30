//! The parts of a PNG file the formula module works on: the skeleton and the records of
//! the images, and the rebuilding of the file from them.
//!
//! A PNG file's pixel data comes in *images*: the run of IDAT chunks (the default image),
//! and in an APNG every run of fdAT chunks (a frame, whose size its fcTL gives; each fdAT
//! starts with a sequence number). An image's *payload* is the concatenation of its chunks'
//! data (past the sequence numbers): one or more zlib streams (several in Apple's iDOT
//! files). Every image is coded on its own, or against the image of the same index and
//! geometry of a similar file (the *reference*, see [`model::Reference`]), or kept as it is
//! if coding does not make it smaller.
//!
//! See FORMAT.md for the full specification.
//!
//! ```text
//! skeleton: varint chunk count, per chunk: kind, varint length, class (bit 7: CRC follows),
//!   data (plain) or sequence number (fdAT); varint trailer length + trailer
//! record:
//!   0, payload                       -- kept as it is
//!   1, varint length + layout        -- streams, blocks, exceptions, model kind
//!      varint length + headers       -- dynamic Huffman headers (aqr-flate's header coder)
//!      model payload                 -- see model::Recorder::finish
//!   2, as 1                          -- coded against the reference's image
//! ```

use crate::model::{self, Geometry, Kind, Span};
use crate::png::{self, Ihdr};
use crate::rans::{BitR, BitW};
use crate::{Error, Result};
use aqr_common::varint;
use aqr_flate::bits::{BitReader, BitWriter};
use aqr_flate::deflate::{Block, Body, Token};
use aqr_flate::header;

pub fn get_varint(src: &[u8], at: &mut usize) -> Result<u64> {
    varint::get(src, at).map_err(|_| crate::corrupt())
}

pub(crate) fn get_bytes<'a>(src: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = src.get(*at..at.checked_add(n).ok_or_else(crate::corrupt)?).ok_or_else(crate::corrupt)?;
    *at += n;
    Ok(s)
}

fn chunk_crc(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut h = aqr_common::crc32::Hasher::new();
    h.update(kind);
    h.update(data);
    h.finalize()
}

/// How a chunk's data is kept in the skeleton.
const PLAIN: u8 = 0;
/// IDAT: all of the data is image payload.
const IDAT: u8 = 1;
/// fdAT: a sequence number (in the skeleton), then image payload.
const FDAT: u8 = 2;

/// The images of a chunk list: runs of chunks of the same image class, with the geometry
/// their payload has (the IHDR's, or the frame's from the last fcTL), if valid.
fn images<'a>(
    classes: &[u8],
    data: impl Fn(usize) -> &'a [u8],
    kinds: impl Fn(usize) -> [u8; 4],
) -> Vec<(std::ops::Range<usize>, Option<Ihdr>)> {
    let ihdr = if classes.first() == Some(&PLAIN) && &kinds(0) == b"IHDR" { Ihdr::parse(data(0)) } else { None };
    let mut frame: Option<Ihdr> = None;
    let mut out = vec![];
    let mut i = 0;
    while i < classes.len() {
        let c = classes[i];
        if c == PLAIN {
            if &kinds(i) == b"fcTL" {
                let d = data(i);
                frame = match (ihdr, d.len()) {
                    (Some(h), 26) => Some(Ihdr {
                        width: u32::from_be_bytes(d[4..8].try_into().unwrap()),
                        height: u32::from_be_bytes(d[8..12].try_into().unwrap()),
                        ..h
                    }),
                    _ => None,
                };
            }
            i += 1;
            continue;
        }
        let start = i;
        while i < classes.len() && classes[i] == c {
            i += 1;
        }
        out.push((start..i, if c == IDAT { ihdr } else { frame }));
    }
    out
}

/// A PNG file in parts: the skeleton and the records of the
/// images, in order (each record's first byte being 0, 1 or 2, see FORMAT.md).
pub(crate) struct Parts {
    pub skeleton: Vec<u8>,
    pub records: Vec<Vec<u8>>,
}

/// The classes of chunks.
fn classes(chunks: &[png::Chunk]) -> Vec<u8> {
    chunks
        .iter()
        .map(|c| match &c.kind {
            b"IDAT" => IDAT,
            b"fdAT" if c.data.len() >= 4 => FDAT,
            _ => PLAIN,
        })
        .collect()
}

/// The payload of the image of `chunks` (of class `class`).
fn payload_of(chunks: &[png::Chunk], class: u8) -> Vec<u8> {
    let skip = if class == FDAT { 4 } else { 0 };
    let mut payload = Vec::with_capacity(chunks.iter().map(|c| c.data.len()).sum());
    for c in chunks {
        payload.extend_from_slice(&c.data[skip..]);
    }
    payload
}

/// The images of a reference file (none if it is not a PNG file), in order: their headers, and
/// their references where their payloads parse.
pub(crate) type References = Vec<(Option<Ihdr>, Option<model::Reference>)>;

/// The [`References`] of `file`.
pub(crate) fn references(file: &[u8]) -> References {
    let Some((chunks, _)) = png::chunks(file) else { return vec![] };
    let classes = classes(&chunks);
    let imgs = images(&classes, |i| chunks[i].data, |i| chunks[i].kind);
    imgs.into_iter()
        .map(|(range, ih)| {
            let payload = payload_of(&chunks[range.clone()], classes[range.start]);
            (ih, ih.and_then(|_| model::Reference::parse(&payload).ok()))
        })
        .collect()
}

/// The reference of an image of header `ih` in a reference's image (`img`, of
/// [`References`]), if both have a header of the same geometry and the reference's payload
/// parses.
fn reference_of<'r>(
    ih: &Option<Ihdr>,
    img: Option<&'r (Option<Ihdr>, Option<model::Reference>)>,
) -> Option<&'r model::Reference> {
    let (Some(a), Some((Some(b), r))) = (ih, img) else { return None };
    let geometry = |h: &Ihdr| (h.width, h.height, h.depth, h.color, h.interlace);
    (geometry(a) == geometry(b)).then_some(r.as_ref()).flatten()
}

/// The parts of `file`, as a PNG file with its images coded where that makes them smaller,
/// against those of `refs` (a reference file's) where that makes them smaller still. With
/// `alone`, the records of the images coded alone, which are not coded again; without, the
/// images coded against a reference are coded alone too only `also_alone` (else only where
/// they cannot be coded against it).
fn parts(
    file: &[u8],
    threads: usize,
    refs: Option<&References>,
    alone: Option<&[&[u8]]>,
    also_alone: bool,
) -> Result<Parts> {
    let (chunks, trailer) = png::chunks(file).ok_or(Error::Unsupported("not a PNG file"))?;
    let classes = classes(&chunks);

    // Skeleton: everything but the image payloads.
    let mut sk = vec![];
    varint::put(&mut sk, chunks.len() as u64);
    for (c, &class) in chunks.iter().zip(&classes) {
        sk.extend_from_slice(&c.kind);
        varint::put(&mut sk, c.data.len() as u64);
        if chunk_crc(&c.kind, c.data) == c.crc {
            sk.push(class);
        } else {
            sk.push(class | 0x80);
            sk.extend_from_slice(&c.crc.to_be_bytes());
        }
        match class {
            PLAIN => sk.extend_from_slice(c.data),
            FDAT => sk.extend_from_slice(&c.data[..4]),
            _ => {}
        }
    }
    varint::put(&mut sk, trailer.len() as u64);
    sk.extend_from_slice(trailer);

    let imgs = images(&classes, |i| chunks[i].data, |i| chunks[i].kind);
    if alone.is_some_and(|a| a.len() != imgs.len()) {
        return Err(Error::Unsupported("records alone of other images"));
    }
    let image = |i: usize, threads: usize| {
        let (range, ih) = &imgs[i];
        let r = reference_of(ih, refs.and_then(|r| r.get(i)));
        image_rec(&chunks[range.clone()], classes[range.start], ih, threads, r, alone.map(|a| a[i]), also_alone)
    };
    // Below this size there is nothing worth a thread to overlap.
    let records = if threads > 1 && file.len() >= PAR_MIN {
        // The images (each into a record of its own) in parallel.
        let workers = threads.min(imgs.len()).max(1);
        // Threads left over from the images go to each image's own parallel steps.
        let inner = (threads / workers).max(1);
        par_map(imgs.len(), workers, |i| image(i, inner))
    } else {
        (0..imgs.len()).map(|i| image(i, 1)).collect()
    };
    Ok(Parts { skeleton: sk, records })
}

/// The record of the image of `chunks` (of class `class`): coded if it has a header and
/// coding makes it smaller (against `reference` if given and smaller still), else the
/// payload as it is. `alone` is the record coded alone, if known; else, against a reference,
/// it is coded alone too only `also_alone`, or where it cannot be coded against it.
fn image_rec(
    chunks: &[png::Chunk],
    class: u8,
    ih: &Option<Ihdr>,
    threads: usize,
    reference: Option<&model::Reference>,
    alone: Option<&[u8]>,
    also_alone: bool,
) -> Vec<u8> {
    let payload = payload_of(chunks, class);
    let code_alone = |rec: &mut Vec<u8>| {
        ih.as_ref().is_some_and(|ih| image_record(ih, &payload, threads, None, rec).is_ok())
            && rec.len() <= payload.len()
    };
    let later = alone.is_none() && !also_alone && ih.is_some() && reference.is_some();
    let mut rec = vec![];
    let mut coded = match alone {
        Some(a) => {
            rec = a.to_vec();
            a.first() == Some(&1)
        }
        None if later => false,
        None => code_alone(&mut rec),
    };
    if let (Some(ih), Some(r)) = (ih, reference) {
        let mut against = vec![];
        if image_record(ih, &payload, threads, Some(r), &mut against).is_ok()
            && against.len() <= payload.len()
            && (!coded || against.len() < rec.len())
        {
            (rec, coded) = (against, true);
        }
    }
    if !coded && later {
        rec.clear();
        coded = code_alone(&mut rec);
    }
    if !coded {
        rec.clear();
        rec.push(0);
        rec.extend_from_slice(&payload);
    }
    if std::env::var_os("PNGR_DEBUG").is_some() {
        eprintln!(
            "image {class} payload {} -> record {} ({})",
            payload.len(),
            rec.len(),
            if coded { "coded" } else { "kept" }
        );
    }
    rec
}

/// The parts of `file` for a formula, if it is a PNG file and any image is coded (see
/// [`crate::formula`]), against `refs` if given, with the records coded `alone` if known
/// (else coded alone `also_alone`, as [`parts`]).
pub(crate) fn formula_parts(
    file: &[u8],
    threads: usize,
    refs: Option<&References>,
    alone: Option<&[&[u8]]>,
    also_alone: bool,
) -> Option<Parts> {
    let parts = parts(file, threads, refs, alone, also_alone).ok()?;
    parts.records.iter().any(|r| r[0] != 0).then_some(parts)
}

/// Files smaller than this are handled on one thread whatever the thread count.
const PAR_MIN: usize = 1 << 16;

/// Runs `f(0..n)` on `workers` threads at most (the caller's among them, the others of
/// the pool), each index once, and returns the results in order.
pub(crate) fn par_map<R: Send>(n: usize, workers: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    aqr_common::pool::par_map(n, workers, f)
}

/// Appends the record of a coded image to `rec`, against `reference` if given.
fn image_record(
    ih: &Ihdr,
    payload: &[u8],
    threads: usize,
    reference: Option<&model::Reference>,
    rec: &mut Vec<u8>,
) -> Result<()> {
    let g = Geometry::new(ih).ok_or(Error::Unsupported("bad image header"))?;
    let kind = if ih.depth == 8 && matches!(ih.color, 0 | 2 | 4 | 6) { Kind::Image } else { Kind::Bytes };
    let enc = model::encode_threads(kind, &g, payload, threads, reference)?;
    // The layout codes the data length in 32 bits.
    if enc.len > u32::MAX as usize {
        return Err(Error::Unsupported("image data of 4 GiB or more"));
    }
    if std::env::var_os("PNGR_DEBUG").is_some() {
        eprintln!("  {} zlib streams, {} bytes of data", enc.streams.len(), enc.len);
    }
    write_record(kind, enc, threads, rec);
    Ok(())
}

/// Appends the coded record of `enc` to `rec` (of kind 2 if it has the tables of coding
/// against a reference, else 1).
fn write_record(kind: Kind, enc: model::Encoded, threads: usize, rec: &mut Vec<u8>) {
    // Layout.
    let mut lw = BitW::default();
    lw.bits(1, (kind == Kind::Bytes) as u32);
    lw.gamma(enc.len as u32);
    lw.gamma(enc.streams.len() as u32 - 1);
    for c in &enc.streams {
        lw.bits(8, c.zhdr[0] as u32);
        lw.bits(8, c.zhdr[1] as u32);
        let blocks = &c.stream.blocks;
        lw.gamma(blocks.len() as u32);
        let std_last = blocks.iter().enumerate().all(|(i, b)| b.last == (i + 1 == blocks.len()));
        lw.bits(1, std_last as u32);
        let mut prev_count = 0usize;
        for (i, b) in blocks.iter().enumerate() {
            if !std_last {
                lw.bits(1, b.last as u32);
            }
            lw.bits(2, b.body.btype() as u32);
            match b.body {
                Body::Stored { pad, len } => {
                    lw.bits(8, pad as u32);
                    lw.bits(16, len as u32);
                }
                _ => {
                    let count = c.starts[i + 1] - c.starts[i];
                    // The same count as the previous block is common (encoders' buffers).
                    if count == prev_count {
                        lw.bits(1, 1);
                    } else {
                        lw.bits(1, 0);
                        lw.gamma(count as u32);
                    }
                    prev_count = count;
                }
            }
        }
        lw.bits(8, c.stream.tail_pad as u32);
        lw.gamma(c.stream.trailing.len() as u32);
        for &b in &c.stream.trailing {
            lw.bits(8, b as u32);
        }
    }
    // Matches of length 258 sent as 284 + 31.
    lw.gamma(enc.alts.len() as u32);
    let mut last = 0;
    for &i in &enc.alts {
        lw.gamma((i - last) as u32);
        last = i;
    }

    // Headers.
    let mut hw = BitWriter::new();
    let mut recipe = header::Recipe::default();
    for c in &enc.streams {
        for (b, (lf, df)) in c.stream.blocks.iter().zip(&c.freqs) {
            if let Body::Dynamic { header: h, .. } = &b.body {
                header::write(&mut hw, lf, df, h, &mut recipe);
            }
        }
    }
    let pad = hw.pad_len();
    hw.bits(pad, 0);
    let headers = hw.finish();
    let layout = lw.finish();

    rec.push(if enc.rec.tables == model::TABLES_REF { 2 } else { 1 });
    for part in [&layout, &headers] {
        varint::put(rec, part.len() as u64);
        rec.extend_from_slice(part);
    }
    enc.rec.finish_threads(rec, threads);
}

/// The chunks of a file without its image payloads, as the skeleton gives them.
pub(crate) struct Skeleton<'a> {
    chunks: Vec<SkChunk<'a>>,
    classes: Vec<u8>,
    trailer: &'a [u8],
    /// The images, as in [`images`].
    pub images: Vec<(std::ops::Range<usize>, Option<Ihdr>)>,
}

struct SkChunk<'a> {
    kind: [u8; 4],
    len: usize,
    crc: Option<u32>,
    /// The whole data (plain chunks) or the sequence number (fdAT).
    data: &'a [u8],
}

impl<'a> Skeleton<'a> {
    pub(crate) fn parse(sk: &'a [u8]) -> Result<Skeleton<'a>> {
        let mut k = 0;
        let nchunks = get_varint(sk, &mut k)? as usize;
        let mut chunks = Vec::with_capacity(nchunks.min(1 << 16));
        let mut classes = Vec::with_capacity(nchunks.min(1 << 16));
        for _ in 0..nchunks {
            let kind: [u8; 4] = get_bytes(sk, &mut k, 4)?.try_into().unwrap();
            let len = get_varint(sk, &mut k)? as usize;
            let flags = *sk.get(k).ok_or_else(crate::corrupt)?;
            k += 1;
            let crc = if flags & 0x80 != 0 {
                Some(u32::from_be_bytes(get_bytes(sk, &mut k, 4)?.try_into().unwrap()))
            } else {
                None
            };
            let class = flags & 0x7f;
            let data = match class {
                PLAIN => get_bytes(sk, &mut k, len)?,
                FDAT if len >= 4 => get_bytes(sk, &mut k, 4)?,
                IDAT => &[][..],
                _ => return Err(crate::corrupt()),
            };
            chunks.push(SkChunk { kind, len, crc, data });
            classes.push(class);
        }
        let tlen = get_varint(sk, &mut k)? as usize;
        let trailer = get_bytes(sk, &mut k, tlen)?;
        if k != sk.len() {
            return Err(crate::corrupt());
        }
        let images = images(&classes, |i| chunks[i].data, |i| chunks[i].kind);
        Ok(Skeleton { chunks, classes, trailer, images })
    }

    /// The file, given the records of its images in order and the reference file's images if
    /// any; and, if `keep`, its own as a reference file (as [`references`] of the file).
    pub(crate) fn rebuild(
        &self,
        recs: &[&[u8]],
        threads: usize,
        refs: Option<&References>,
        keep: bool,
    ) -> Result<(Vec<u8>, Option<References>)> {
        let (cs, classes, imgs) = (&self.chunks, &self.classes, &self.images);
        if recs.len() != imgs.len() {
            return Err(crate::corrupt());
        }
        let img_len = |range: &std::ops::Range<usize>| {
            let skip = if classes[range.start] == FDAT { 4 } else { 0 };
            cs[range.clone()].iter().map(|c| c.len - skip).sum::<usize>()
        };
        let image_payload = |i: usize, threads: usize| {
            let ih = &imgs[i].1;
            let r = if recs[i].first() == Some(&2) { reference_of(ih, refs.and_then(|r| r.get(i))) } else { None };
            image_payload(recs[i], img_len(&imgs[i].0), ih, threads, r, keep)
        };
        let payloads = if threads > 1 && imgs.len() > 1 {
            let workers = threads.min(imgs.len());
            let inner = (threads / workers).max(1);
            // (as tasks on WebAssembly)
            let res = aqr_common::pool::par_map_apart(imgs.len(), workers, |i| image_payload(i, inner));
            res.into_iter().collect::<Result<Vec<_>>>()?
        } else {
            (0..imgs.len()).map(|i| image_payload(i, threads)).collect::<Result<Vec<_>>>()?
        };
        let (payloads, own): (Vec<Vec<u8>>, Vec<Option<model::Reference>>) = payloads.into_iter().unzip();
        let own = keep.then(|| imgs.iter().map(|i| i.1).zip(own).collect());

        // Reassemble the file.
        let total: usize = cs.iter().map(|c| c.len + 12).sum::<usize>() + 8 + self.trailer.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&png::SIGNATURE);
        let mut img = 0;
        let mut pa = 0;
        for (i, c) in cs.iter().enumerate() {
            out.extend_from_slice(&(c.len as u32).to_be_bytes());
            out.extend_from_slice(&c.kind);
            let start = out.len();
            match classes[i] {
                PLAIN => out.extend_from_slice(c.data),
                class => {
                    while img < imgs.len() && imgs[img].0.end <= i {
                        img += 1;
                        pa = 0;
                    }
                    if class == FDAT {
                        out.extend_from_slice(c.data);
                    }
                    let n = c.len - if class == FDAT { 4 } else { 0 };
                    let p = payloads.get(img).ok_or_else(crate::corrupt)?;
                    out.extend_from_slice(p.get(pa..pa + n).ok_or_else(crate::corrupt)?);
                    pa += n;
                }
            }
            let crc = c.crc.unwrap_or_else(|| chunk_crc(&c.kind, &out[start..]));
            out.extend_from_slice(&crc.to_be_bytes());
        }
        out.extend_from_slice(self.trailer);
        Ok((out, own))
    }
}

/// The payload (of `len` bytes) an image's record gives (`reference`: the reference of the
/// image, for a record coded against it); and, if `keep`, the image's own as a reference (as
/// [`references`] makes it).
fn image_payload(
    rec: &[u8],
    len: usize,
    ih: &Option<Ihdr>,
    threads: usize,
    reference: Option<&model::Reference>,
    keep: bool,
) -> Result<(Vec<u8>, Option<model::Reference>)> {
    let (payload, own) = match rec.first() {
        Some(0) if rec.len() == len + 1 => (rec[1..].to_vec(), None),
        Some(&k @ (1 | 2)) => {
            let ih = ih.as_ref().ok_or_else(crate::corrupt)?;
            let g = Geometry::new(ih).ok_or_else(crate::corrupt)?;
            let reference = if k == 2 { Some(reference.ok_or_else(crate::corrupt)?) } else { None };
            rebuild(&g, &rec[1..], len, threads, reference, keep)?
        }
        _ => return Err(crate::corrupt()),
    };
    if payload.len() != len {
        return Err(crate::corrupt());
    }
    // (that of a payload kept as it is parsed, as of a reference file)
    let own = match own {
        Some(r) => Some(r),
        None if keep && ih.is_some() => model::Reference::parse(&payload).ok(),
        None => None,
    };
    Ok((payload, own))
}

/// One stream's structure as the layout gives it.
struct StreamLayout {
    zhdr: [u8; 2],
    blocks: Vec<Block>,
    tail_pad: u8,
    trailing: Vec<u8>,
}

/// Rebuilds a coded image's payload (of `len` bytes, if the record is right) from its record
/// (past the mode byte), against `reference` if it is coded against one; and, if `keep`, the
/// image's own reference (that [`model::Reference::parse`] gives of the payload).
fn rebuild(
    g: &Geometry,
    rec: &[u8],
    len: usize,
    threads: usize,
    reference: Option<&model::Reference>,
    keep: bool,
) -> Result<(Vec<u8>, Option<model::Reference>)> {
    let mut at = 0;
    let part = |at: &mut usize| -> Result<&[u8]> {
        let n = get_varint(rec, at)? as usize;
        get_bytes(rec, at, n)
    };
    let layout = part(&mut at)?;
    let headers = part(&mut at)?;
    let mut r = BitR::new(layout);
    let kind = if r.bits(1) == 1 { Kind::Bytes } else { Kind::Image };
    let n = r.gamma()? as usize;
    let nstreams = r.gamma()? as usize + 1;
    if nstreams > layout.len() {
        return Err(crate::corrupt());
    }
    let mut streams = Vec::with_capacity(nstreams);
    let mut spans = vec![];
    for _ in 0..nstreams {
        let zhdr = [r.bits(8) as u8, r.bits(8) as u8];
        let nblocks = r.gamma()? as usize;
        if nblocks > layout.len() * 8 {
            return Err(crate::corrupt());
        }
        let std_last = r.bits(1) == 1;
        let mut blocks = Vec::with_capacity(nblocks);
        let mut prev_count = 0usize;
        for i in 0..nblocks {
            let last = if std_last { i + 1 == nblocks } else { r.bits(1) == 1 };
            let body = match r.bits(2) {
                0 => {
                    let pad = r.bits(8) as u8;
                    let len = r.bits(16) as u16;
                    spans.push(Span::Stored(len as usize));
                    Body::Stored { pad, len }
                }
                t @ (1 | 2) => {
                    let count = if r.bits(1) == 1 { prev_count } else { r.gamma()? as usize };
                    prev_count = count;
                    spans.push(Span::Tokens(count));
                    if t == 1 {
                        Body::Fixed { tokens: vec![] }
                    } else {
                        Body::Dynamic { header: Default::default(), tokens: vec![] }
                    }
                }
                _ => return Err(crate::corrupt()),
            };
            blocks.push(Block { last, body });
            if r.overrun() {
                return Err(crate::corrupt());
            }
        }
        let tail_pad = r.bits(8) as u8;
        let ntrail = r.gamma()? as usize;
        if ntrail > layout.len() * 8 {
            return Err(crate::corrupt());
        }
        let trailing: Vec<u8> = (0..ntrail).map(|_| r.bits(8) as u8).collect();
        streams.push(StreamLayout { zhdr, blocks, tail_pad, trailing });
    }
    let nalts = r.gamma()? as usize;
    if nalts > layout.len() * 8 {
        return Err(crate::corrupt());
    }
    let mut alts = Vec::with_capacity(nalts);
    let mut last = 0usize;
    for _ in 0..nalts {
        last += r.gamma()? as usize;
        alts.push(last);
    }
    if r.overrun() {
        return Err(crate::corrupt());
    }

    let ntok: usize = spans.iter().map(|s| if let Span::Tokens(c) = s { *c } else { 0 }).sum();
    if ntok > n {
        return Err(crate::corrupt());
    }
    let tables = if reference.is_some() { model::TABLES_REF } else { model::TABLES };
    let player = model::Player::new(rec, &mut at, tables)?;
    if at != rec.len() {
        return Err(crate::corrupt());
    }

    // The streams are written block by block as the model gives them: on this thread, or
    // on another one as this one decodes. (The writer reserves room for the longest codes
    // of a block's tokens ahead.)
    let most = spans.iter().map(|s| if let Span::Tokens(c) = *s { c } else { 0 }).max().unwrap_or(0);
    let mut emit = Payload::new(streams, alts, headers, len + 6 * most + 64)?;
    // (positions as the reference's are 32-bit)
    let mut own = (keep && n <= u32::MAX as usize).then(|| model::Reference::with_capacity(ntok));
    // (the writer on a worker of the pool, if one is free)
    let writer = (threads > 1 && n >= PAR_MIN).then(|| aqr_common::pool::reserve(1)).filter(|w| !w.is_empty());
    if let Some(writer) = writer {
        /// A block's tokens and frequencies, and the bytes of a stored block.
        type Msg = (Vec<Token>, model::Freqs, Vec<u8>);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(PIPE_DEPTH);
        let (back, spare) = std::sync::mpsc::channel::<Vec<Token>>();
        let (dec, written) = aqr_common::pool::scope_in(writer, |sc| {
            let writer = sc.spawn(move || {
                for (toks, freqs, bytes) in rx {
                    let (mut toks, mut freqs) = (toks, freqs);
                    emit.block(&mut toks, &mut freqs, &bytes)?;
                    // The decoder may be done already.
                    let _ = back.send(toks);
                }
                emit.finish()
            });
            // Where the span starts.
            let mut p = 0usize;
            let mut send = |toks: &mut [Token], freqs: &mut model::Freqs, data: &[u8], from: usize| {
                if let Some(r) = &mut own {
                    r.span(toks, from + data.len());
                }
                let mut v = spare.try_recv().unwrap_or_default();
                v.clear();
                v.extend_from_slice(toks);
                // Only a stored block's bytes are wanted (and they are all the new ones).
                let bytes = if toks.is_empty() {
                    p.checked_sub(from).and_then(|i| data.get(i..)).ok_or_else(crate::corrupt)?.to_vec()
                } else {
                    vec![]
                };
                p = from + data.len();
                tx.send((v, *freqs, bytes)).map_err(|_| crate::corrupt())
            };
            let dec = model::decode(&player, kind, g, &spans, n, reference, &mut send);
            drop(tx);
            (dec, writer.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
        });
        // The writer's error first: the decoder's may only be that it stopped.
        let (out, pos) = written?;
        dec?;
        if pos != n {
            return Err(crate::corrupt());
        }
        Ok((out, own))
    } else {
        model::decode(&player, kind, g, &spans, n, reference, &mut |toks, freqs, data, from| {
            if let Some(r) = &mut own {
                r.span(toks, from + data.len());
            }
            // The data from where the block starts (none needed but a stored block's).
            let rest = emit.at().checked_sub(from).and_then(|i| data.get(i..)).unwrap_or_default();
            emit.block(toks, freqs, rest)
        })?;
        let (out, pos) = emit.finish()?;
        if pos != n {
            return Err(crate::corrupt());
        }
        Ok((out, own))
    }
}

/// Blocks in flight between the model and the writer.
const PIPE_DEPTH: usize = 4;

/// Writes an image's streams block by block, as the model decodes them.
struct Payload<'a> {
    streams: Vec<StreamLayout>,
    /// The stream being written, its next block, and its writer (appending to the payload).
    si: usize,
    bi: usize,
    w: Option<aqr_flate::write::Writer>,
    out: Vec<u8>,
    /// Data bytes of the streams written.
    done: usize,
    /// Tokens of the blocks written, and the next of `alts` (token indices of matches of
    /// length 258 sent as 284 + 31).
    t0: usize,
    alts: Vec<usize>,
    ai: usize,
    hr: BitReader<'a>,
    recipe: header::Recipe,
}

impl<'a> Payload<'a> {
    /// `cap`: room for the payload, if the data is right.
    fn new(streams: Vec<StreamLayout>, alts: Vec<usize>, headers: &'a [u8], cap: usize) -> Result<Payload<'a>> {
        let mut e = Payload {
            streams,
            si: 0,
            bi: 0,
            w: None,
            out: Vec::with_capacity(cap),
            done: 0,
            t0: 0,
            alts,
            ai: 0,
            hr: BitReader::new(headers),
            recipe: header::Recipe::default(),
        };
        e.advance()?;
        Ok(e)
    }

    /// Where the next block's data starts.
    fn at(&self) -> usize {
        self.done + self.w.as_ref().map_or(0, |w| w.pos())
    }

    /// Writes the next block: its tokens `toks` and frequencies `freqs` (both corrected for
    /// matches sent as 284 + 31), given the data from [`Payload::at`] on.
    fn block(&mut self, toks: &mut [Token], freqs: &mut model::Freqs, rest: &[u8]) -> Result<()> {
        let t1 = self.t0 + toks.len();
        while let Some(&i) = self.alts.get(self.ai).filter(|&&i| i < t1) {
            match toks.get_mut(i.wrapping_sub(self.t0)) {
                // Listed once only.
                Some(Token::Match { len: 258, alt258: alt258 @ false, .. }) => *alt258 = true,
                _ => return Err(crate::corrupt()),
            }
            // Symbol 284 rather than 285.
            freqs.0[285] -= 1;
            freqs.0[284] += 1;
            self.ai += 1;
        }
        self.t0 = t1;
        let (Some(w), Some(b)) = (&mut self.w, self.streams.get_mut(self.si).and_then(|s| s.blocks.get_mut(self.bi)))
        else {
            return Err(crate::corrupt());
        };
        if let Body::Dynamic { header: h, .. } = &mut b.body {
            *h = header::read_freqs(&mut self.hr, &freqs.0, &freqs.1, &mut self.recipe)?;
        }
        w.block_of(b, toks, rest)?;
        self.bi += 1;
        self.advance()
    }

    /// Finishes the streams whose blocks are all written, and starts the next one.
    fn advance(&mut self) -> Result<()> {
        while let Some(s) = self.streams.get(self.si) {
            if self.w.is_none() {
                self.out.extend_from_slice(&s.zhdr);
                self.w = Some(aqr_flate::write::Writer::append_to(std::mem::take(&mut self.out)));
            }
            if self.bi < s.blocks.len() {
                break;
            }
            let w = self.w.take().unwrap();
            self.done += w.pos();
            self.out = w.finish(s.tail_pad, &s.trailing)?;
            (self.si, self.bi) = (self.si + 1, 0);
        }
        Ok(())
    }

    /// The payload and the data bytes its streams cover.
    fn finish(self) -> Result<(Vec<u8>, usize)> {
        if self.si != self.streams.len() || self.ai != self.alts.len() {
            return Err(crate::corrupt());
        }
        Ok((self.out, self.done))
    }
}

/// The helpers of the integration tests (for [`crate::formula`]'s tests too).
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
pub(crate) mod common;

#[cfg(test)]
mod tests {
    use super::*;

    /// The references a file's rebuild keeps are those parsed of it: of images coded (frames
    /// among them, also on threads) and kept as they are.
    #[test]
    fn kept_references() {
        use super::common::*;
        let mut noise = vec![];
        let mut x = 1u32;
        for _ in 0..40 {
            noise.push(0);
            noise.extend((0..60).map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 16) as u8
            }));
        }
        let big = image_data(400, 300, 8, 6, false, 9);
        let files = [
            apng(&[(64, 48), (32, 20), (64, 48)], 3),
            png(&[(b"IHDR", ihdr(60, 40, 8, 0, 0)), (b"IDAT", zlib(&zlib6(), &noise)), (b"IEND", vec![])]),
            png(&[(b"IHDR", ihdr(400, 300, 8, 6, 0)), (b"IDAT", zlib(&zlib6(), &big)), (b"IEND", vec![])]),
        ];
        for (k, file) in files.iter().enumerate() {
            let parts = parts(file, 1, None, None, true).unwrap();
            let recs: Vec<&[u8]> = parts.records.iter().map(|r| &r[..]).collect();
            for threads in [1, 4] {
                let (out, own) = Skeleton::parse(&parts.skeleton).unwrap().rebuild(&recs, threads, None, true).unwrap();
                assert!(out == *file);
                assert!(own.unwrap() == references(file), "file {k}, {threads} threads");
            }
        }
    }

    /// A token index listed twice among the matches sent as 284 + 31 is corrupt data,
    /// not a counter underflow.
    #[test]
    fn duplicate_alt258() {
        use aqr_flate::deflate::{Stream, Token};
        let ih = Ihdr { width: 300, height: 1, depth: 8, color: 0, compression: 0, filter: 0, interlace: 0 };
        let mut data = vec![0u8, 5];
        data.resize(260, 5);
        data.extend((0..41).map(|i| i as u8));
        let mut tokens = vec![Token::Lit(0), Token::Lit(5), Token::Match { len: 258, dist: 1, alt258: true }];
        tokens.extend(data[260..].iter().map(|&b| Token::Lit(b)));
        let stream = Stream { blocks: vec![Block { last: true, body: Body::Fixed { tokens } }], ..Default::default() };
        let mut payload = vec![0x78, 0x9c];
        payload.extend(aqr_flate::deflate::write(&stream, &data).unwrap());
        payload.extend([0; 4]);
        let g = Geometry::new(&ih).unwrap();
        let rec_of = |dup: bool| {
            let mut enc = model::encode(Kind::Image, &g, &payload).unwrap();
            assert_eq!(enc.alts, [2]);
            if dup {
                enc.alts.push(2);
            }
            let mut rec = vec![];
            write_record(Kind::Image, enc, 1, &mut rec);
            rec
        };
        assert!(rebuild(&g, &rec_of(false)[1..], payload.len(), 1, None, false).unwrap().0 == payload);
        assert!(matches!(rebuild(&g, &rec_of(true)[1..], payload.len(), 1, None, false), Err(Error::Corrupt)));
    }
}
