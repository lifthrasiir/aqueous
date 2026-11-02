//! Compressors of a stream (a tar archive, or a single file), each of its parts compressed on
//! the pool's threads:
//!
//! - gzip: members one after another, each a part of about `total / threads` bytes (1 to 16
//!   MiB) deflated as zlib 1.3.1 does it (aqr-flate's `zlib::compress`, so that each rebuilds
//!   whole as Aqueous recompresses it), stored blocks at level 0. Every gzip reader takes the
//!   members as one stream; the first has the file's name and time (of a single file).
//! - bzip2: one stream whose blocks (cut as libbzip2 cuts them) are compressed side by side by
//!   aqr-bz2's reimplementation of libbzip2: the stream bzip2 writes, bit for bit.
//! - xz: liblzma's encoder of threads, blocks of about `total / threads` bytes (1 MiB to its
//!   own size, 3 times the dictionary), CRC-64.
//! - LZMA (`.lzma`): liblzma's, one thread (a single stream has no parts).
//! - Zstandard: libzstd's workers, jobs of about `total / threads` bytes (512 KiB on), its
//!   checksum.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use aqr_bz2::compress as bz2;
use aqr_common::msb::{BitReader, BitWriter};
use aqr_common::{crc32, pool};
use aqr_flate::zlib;

use crate::{Contents, EntryNode, Filter, Format, Options, Progress, Tell, Time, Tree, Written};

/// A stream's compressor, writing as it is given data; [`Encoder::finish`] ends the stream.
pub trait Encoder: Write {
    fn finish(self: Box<Self>) -> io::Result<()>;
}

/// What a stream is of: its file's name and time (gzip's header tells them), and its bytes
/// where they are known exactly (Zstandard's frame tells them).
#[derive(Debug, Clone, Default)]
pub struct Named {
    pub name: Option<Vec<u8>>,
    pub mtime: Option<Time>,
    pub size: Option<u64>,
}

/// The size of the parts of a stream of `total` bytes on `threads` threads: one each, within
/// `min` and `max`.
fn part(total: u64, threads: usize, min: u64, max: u64) -> u64 {
    total.div_ceil(threads.max(1) as u64).clamp(min, max)
}

/// A compressor of `f` at `level` writing to `w`, of `total` bytes (as far as known; 0 for
/// not), of a file `named` (gzip's header).
pub fn encoder<'w>(
    f: Filter,
    level: u8,
    o: &Options,
    total: u64,
    named: Named,
    w: &'w mut dyn Write,
) -> io::Result<Box<dyn Encoder + 'w>> {
    let threads = o.threads.max(1);
    let wave = |part: u64| (part * threads as u64).min(o.batch.max(part)) as usize;
    Ok(match f {
        Filter::Gzip => {
            let part = part(total, threads, 1 << 20, 16 << 20) as usize;
            Box::new(Gzip { w, level, threads, part, wave: wave(part as u64), pending: Vec::new(), named, members: 0 })
        }
        Filter::Bzip2 => {
            let block = aqr_bz2::block_max(level) as u64;
            // (enough blocks for every thread, a few times over: one may be long by its runs)
            let wave = wave(block * 4).max(block as usize * 4);
            let mut bits = BitWriter::new();
            aqr_bz2::MAGIC.iter().for_each(|&c| bits.put(c as u32, 8));
            bits.put((b'0' + level) as u32, 8);
            Box::new(Bzip2 { w, level, threads, wave, pending: Vec::new(), bits, crc: 0 })
        }
        Filter::Xz => {
            let block = part(total, threads, 1 << 20, 3 * xz_dict(level));
            let mut mt = liblzma::stream::MtStreamBuilder::new();
            mt.threads(threads as u32).block_size(block).preset(level as u32).check(liblzma::stream::Check::Crc64);
            let s = mt.encoder().map_err(io::Error::other)?;
            Box::new(Lzma(liblzma::write::XzEncoder::new_stream(w, s)))
        }
        Filter::Lzma => {
            let opts = liblzma::stream::LzmaOptions::new_preset(level as u32).map_err(io::Error::other)?;
            let s = liblzma::stream::Stream::new_lzma_encoder(&opts).map_err(io::Error::other)?;
            Box::new(Lzma(liblzma::write::XzEncoder::new_stream(w, s)))
        }
        Filter::Zstd => {
            let mut e = zstd::stream::write::Encoder::new(w, level as i32)?;
            e.include_checksum(true)?;
            if threads > 1 {
                e.multithread(threads as u32)?;
                let job = part(total, threads, 512 << 10, 64 << 20);
                // (libzstd's own where that is no larger: 4 windows of the level's)
                if job < 8 << 20 {
                    e.set_parameter(zstd::stream::raw::CParameter::JobSize(job as u32))?;
                }
            }
            if named.size.is_some() {
                e.set_pledged_src_size(named.size)?;
            }
            Box::new(Zstd(e))
        }
    })
}

/// The dictionary of XZ Utils' preset `level` (as its manual lists them).
pub(crate) fn xz_dict(level: u8) -> u64 {
    const KIB: u64 = 1 << 10;
    [256 * KIB, 1 << 20, 2 << 20, 4 << 20, 4 << 20, 8 << 20, 8 << 20, 16 << 20, 32 << 20, 64 << 20]
        [level.min(9) as usize]
}

/// A gzip stream of members of `part` bytes, a wave of them compressed at once.
struct Gzip<'w> {
    w: &'w mut dyn Write,
    level: u8,
    threads: usize,
    part: usize,
    wave: usize,
    pending: Vec<u8>,
    named: Named,
    members: usize,
}

impl Gzip<'_> {
    /// Compresses and writes the pending data's whole parts (all of it, where `last`).
    fn flush_parts(&mut self, last: bool) -> io::Result<()> {
        let n = if last { self.pending.len().div_ceil(self.part).max(1) } else { self.pending.len() / self.part };
        if n == 0 || (last && self.pending.is_empty() && self.members > 0) {
            return Ok(());
        }
        let (d, part, level) = (&self.pending, self.part, self.level);
        let deflated = pool::par_map(n, self.threads, |k| {
            let p = &d[(k * part).min(d.len())..((k + 1) * part).min(d.len())];
            (deflate_raw(p, level), crc32::hash(p), p.len() as u32)
        });
        for (deflated, crc, len) in deflated {
            let first = self.members == 0;
            self.w.write_all(&gzip_header(level, first.then_some(&self.named)))?;
            self.w.write_all(&deflated)?;
            self.w.write_all(&[crc.to_le_bytes(), len.to_le_bytes()].concat())?;
            self.members += 1;
        }
        let used = (n * part).min(d.len());
        self.pending.drain(..used);
        Ok(())
    }
}

impl Write for Gzip<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(b);
        if self.pending.len() >= self.wave {
            self.flush_parts(false)?;
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Encoder for Gzip<'_> {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.flush_parts(true)
    }
}

/// A gzip member's header: of a file named (the first member's), else of none. Its extra flags
/// as gzip writes them (2 at level 9, 4 at level 1), of Unix.
fn gzip_header(level: u8, named: Option<&Named>) -> Vec<u8> {
    let name = named.and_then(|n| n.name.as_ref()).filter(|n| !n.contains(&0));
    let mtime = named.and_then(|n| n.mtime).and_then(|t| u32::try_from(t.secs).ok()).unwrap_or(0);
    let xfl = match level {
        9 => 2,
        1 => 4,
        _ => 0,
    };
    let mut h = vec![0x1f, 0x8b, 8, if name.is_some() { 8 } else { 0 }];
    h.extend(mtime.to_le_bytes());
    h.extend([xfl, 3]);
    if let Some(n) = name {
        h.extend_from_slice(n);
        h.push(0);
    }
    h
}

/// `d` as a raw DEFLATE stream: as zlib 1.3.1 deflates it at `level`, stored blocks at 0.
pub(crate) fn deflate_raw(d: &[u8], level: u8) -> Vec<u8> {
    if level > 0 {
        let p = zlib::Params { level: level.min(9), ..zlib::Params::default() };
        if let Some(v) = zlib::compress(d, p) {
            return v;
        }
    }
    // stored blocks of 65,535 bytes at most (one empty one for no data)
    let mut out = Vec::with_capacity(d.len() + d.len() / 65535 * 5 + 5);
    let n = d.len().div_ceil(65535).max(1);
    for k in 0..n {
        let b = &d[(k * 65535).min(d.len())..((k + 1) * 65535).min(d.len())];
        out.push((k + 1 == n) as u8);
        out.extend((b.len() as u16).to_le_bytes());
        out.extend((!(b.len() as u16)).to_le_bytes());
        out.extend_from_slice(b);
    }
    out
}

/// A bzip2 stream, its blocks compressed a wave at a time; the bits written so far.
struct Bzip2<'w> {
    w: &'w mut dyn Write,
    level: u8,
    threads: usize,
    wave: usize,
    pending: Vec<u8>,
    bits: BitWriter,
    crc: u32,
}

impl Bzip2<'_> {
    /// Compresses and writes the pending data's blocks but the last (all, where `last`): the
    /// last may yet take more.
    fn flush_blocks(&mut self, last: bool) -> io::Result<()> {
        let mut cuts = bz2::boundaries(&self.pending, self.level);
        if !last && cuts.len() > 1 {
            // (the last block is not cut yet)
            cuts.pop();
        } else if !last {
            return Ok(());
        }
        let end = if last { self.pending.len() } else { *cuts.last().unwrap() };
        let n = if last { cuts.len() } else { cuts.len() - 1 };
        cuts.truncate(n);
        cuts.push(end);
        let d = &self.pending;
        let blocks = pool::par_map(n, self.threads, |k| bz2::block(&d[cuts[k]..cuts[k + 1]]));
        for b in &blocks {
            self.bits.copy(&mut BitReader::new(&b.bits, 0), b.nbits);
            self.crc = self.crc.rotate_left(1) ^ b.crc;
        }
        self.pending.drain(..end);
        self.w.write_all(&self.bits.out)?;
        self.bits.out.clear();
        Ok(())
    }
}

impl Write for Bzip2<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(b);
        if self.pending.len() >= self.wave {
            self.flush_blocks(false)?;
            // (a wave of one block, long by its runs: waits for more)
            if self.pending.len() >= self.wave {
                self.wave = self.pending.len() * 2;
            }
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Encoder for Bzip2<'_> {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.flush_blocks(true)?;
        let mut bits = std::mem::take(&mut self.bits);
        bits.put((aqr_bz2::END_MAGIC >> 24) as u32, 24);
        bits.put((aqr_bz2::END_MAGIC & 0xff_ffff) as u32, 24);
        bits.put(self.crc, 32);
        self.w.write_all(&bits.finish())
    }
}

struct Lzma<'w>(liblzma::write::XzEncoder<&'w mut dyn Write>);

impl Write for Lzma<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Encoder for Lzma<'_> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        self.0.finish().map(drop)
    }
}

struct Zstd<'w>(zstd::stream::write::Encoder<'static, &'w mut dyn Write>);

impl Write for Zstd<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Encoder for Zstd<'_> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        self.0.finish().map(drop)
    }
}

/// Writes the single file of tree `t` as a stream of `f` to `w`: its name and time in gzip's
/// header.
pub fn write_single(
    f: Filter,
    t: &Tree,
    c: &dyn Contents,
    o: &Options,
    w: &mut dyn Write,
    tell: &Tell,
) -> io::Result<Written> {
    let [e] = &t.entries[..] else {
        return Err(io::Error::other(format!(
            "{}: a single file alone (tar{} for more)",
            Format::Single(f).ext(),
            Format::Single(f).ext()
        )));
    };
    let EntryNode::File(k) = e.node else {
        return Err(io::Error::other(format!("{}: a single file alone", Format::Single(f).ext())));
    };
    let mut out = Written::default();
    if !e.extra.xattrs.is_empty() || !e.extra.forks.is_empty() {
        out.notes
            .push(format!("the extended attributes and forks of the file, not kept in {}", Format::Single(f).ext()));
    }
    let total = c.size(k);
    let name = Some(aqr_formula::aqr::name_bytes(&e.name).0.to_vec());
    let mut named = Named { name, mtime: e.extra.mtime, size: None };
    let level = o.level_of(Format::Single(f));
    let mut counted = crate::Counted { w, n: 0 };
    tell(Progress::Done { done: 0, total });
    tell(Progress::Started(k));
    let d = c.read(k)?;
    named.size = Some(d.len() as u64);
    let mut enc = encoder(f, level, o, total, named, &mut counted)?;
    let done = AtomicU64::new(0);
    // (given in parts, so that what is done is told)
    for p in d.chunks(16 << 20) {
        enc.write_all(p)?;
        let n = done.fetch_add(p.len() as u64, Ordering::Relaxed) + p.len() as u64;
        tell(Progress::Done { done: n, total });
    }
    enc.finish()?;
    tell(Progress::Ended(k));
    out.read = d.len() as u64;
    out.len = counted.n;
    Ok(out)
}
