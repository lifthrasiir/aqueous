//! A tree written as a 7z archive: the data of every file (and the target of every symbolic
//! link) with content in one solid folder, in tree order, of LZMA2 at the xz preset of the level
//! (liblzma's raw encoder; level 0: stored by the Copy coder). The stream is cut into parts of
//! `total / threads` bytes (1 MiB to four times the preset's dictionary), each encoded apart
//! with a dictionary reset on the pool's threads, a wave of parts at a time as the files are
//! read, and the parts' LZMA2 streams joined into one (the end byte of every part but the last
//! dropped); the dictionary size told covers a part. The packed stream goes to the output as it
//! is made, the signature header being written at the end, over its placeholder.
//!
//! - Each file's size and CRC-32 are in the folder's substreams. Names are UTF-16, `/` between
//!   the names as 7-Zip writes them (a name of other bytes than UTF-8 is written lossily, told).
//!   Directories and empty files have no stream (the latter flagged as files).
//! - Times: modification, access and creation (7z's CTime) as FILETIMEs, each told only where
//!   some entry has it. Attributes: Windows' (read-only, hidden, system, archive; 0x10 for a
//!   directory) and, where the Unix mode is known, 7-Zip's extension (0x8000, the mode with its
//!   file type in the high 16 bits). A symbolic link is a file of its target, of type `S_IFLNK`,
//!   as 7-Zip on Unix writes it.
//! - With a passphrase, the folder has 7-Zip's AES-256 coder after LZMA2 (a random IV, the key
//!   of 2^19 rounds derived once, CBC carried over the waves) and the header is encoded too:
//!   LZMA2 then AES in a folder of its own, its CRC-32 telling a wrong passphrase.
//! - Left out, told: FIFOs and devices, extended attributes and forks.

use std::io::{self, SeekFrom};

use aqr_common::{crc32, pool};
use liblzma::stream::{Action, Filters, LzmaOptions, Status, Stream};

use crate::filter::xz_dict;
use crate::{
    Bytes, Contents, Extra, Format, Item, Keeps, Kind, Options, Output, Progress, Tell, Tree, Written, batches, items,
};
use aqr_foreign::attrs::{DOS_DIR, DOS_READ_ONLY, S_IFDIR, S_IFLNK, S_IFREG, UNIX_EXTENSION};
use aqr_foreign::crypt::SEVENZ_AES_ID;
use aqr_foreign::{crypt, time};

use aqr_7z::header::{COPY, LZMA2, MAGIC, SIGNATURE};

/// Property ids, as bytes.
mod id {
    use aqr_7z::header::id;

    pub const END: u8 = id::END as u8;
    pub const HEADER: u8 = id::HEADER as u8;
    pub const MAIN_STREAMS: u8 = id::MAIN_STREAMS as u8;
    pub const FILES: u8 = id::FILES as u8;
    pub const PACK_INFO: u8 = id::PACK_INFO as u8;
    pub const UNPACK_INFO: u8 = id::UNPACK_INFO as u8;
    pub const SUBSTREAMS: u8 = id::SUBSTREAMS as u8;
    pub const SIZE: u8 = id::SIZE as u8;
    pub const CRC: u8 = id::CRC as u8;
    pub const FOLDER: u8 = id::FOLDER as u8;
    pub const UNPACK_SIZE: u8 = id::UNPACK_SIZE as u8;
    pub const NUM_UNPACK_STREAM: u8 = id::NUM_UNPACK_STREAM as u8;
    pub const EMPTY_STREAM: u8 = id::EMPTY_STREAM as u8;
    pub const EMPTY_FILE: u8 = id::EMPTY_FILE as u8;
    pub const NAME: u8 = id::NAME as u8;
    pub const CTIME: u8 = id::CTIME as u8;
    pub const ATIME: u8 = id::ATIME as u8;
    pub const MTIME: u8 = id::MTIME as u8;
    pub const WIN_ATTRIBUTES: u8 = id::WIN_ATTRIBUTES as u8;
    pub const ENCODED_HEADER: u8 = id::ENCODED_HEADER as u8;
}

/// A coder of a folder: its id and properties.
struct Coder {
    id: &'static [u8],
    props: Vec<u8>,
}

/// A NUMBER: the first byte's high 1 bits count the bytes after it, little-endian.
fn number(out: &mut Vec<u8>, v: u64) {
    let n = (0..8).find(|&n| v < 1u64 << (7 * (n + 1))).unwrap_or(8);
    let first = if n == 8 { 0xff } else { (0xff00u16 >> n) as u8 | (v >> (8 * n)) as u8 };
    out.push(first);
    out.extend(&v.to_le_bytes()[..n]);
}

/// The bits of `v`, highest first.
fn bits(v: impl ExactSizeIterator<Item = bool>) -> Vec<u8> {
    let mut b = vec![0u8; v.len().div_ceil(8)];
    v.enumerate().filter(|x| x.1).for_each(|(i, _)| b[i / 8] |= 0x80 >> (i % 8));
    b
}

/// The property byte of an LZMA2 dictionary of at least `size` bytes.
fn dict_prop(size: u64) -> u8 {
    (0..40u8).find(|&p| (2 | (p & 1) as u64) << (p / 2 + 11) >= size).unwrap_or(40)
}

/// The dictionary an LZMA2 stream of parts of up to `part` bytes at `level` has.
fn dictionary(level: u8, part: u64) -> u64 {
    xz_dict(level).min(part).max(4096)
}

/// `data` as a raw LZMA2 stream (ending with its 0x00 byte) of dictionary `dict` at preset `level`.
fn lzma2(data: &[u8], level: u8, dict: u64) -> io::Result<Vec<u8>> {
    let lz = |e: liblzma::stream::Error| io::Error::other(format!("7z: LZMA2: {e}"));
    let mut opts = LzmaOptions::new_preset(level as u32).map_err(lz)?;
    opts.dict_size(dict as u32);
    let mut filters = Filters::new();
    filters.lzma2(&opts);
    let mut s = Stream::new_raw_encoder(&filters).map_err(lz)?;
    let mut out = Vec::with_capacity(data.len() / 2 + 1024);
    loop {
        let at = s.total_in() as usize;
        if out.capacity() - out.len() < 1024 {
            out.reserve(out.capacity() / 2 + 4096);
        }
        if s.process_vec(&data[at..], &mut out, Action::Finish).map_err(lz)? == Status::StreamEnd {
            return Ok(out);
        }
    }
}

/// The data of the folder cut into parts compressed on the pool's threads, a wave at a time,
/// and the packed stream written (encrypted where it is) as it goes.
struct Packer<'w> {
    w: &'w mut dyn Output,
    /// 0: stored.
    level: u8,
    threads: usize,
    part: usize,
    /// Bytes of data compressed at once (whole parts).
    wave: usize,
    dict: u64,
    pending: Vec<u8>,
    aes: Option<crypt::SevenzEncryptor>,
    /// Bytes given, and bytes written.
    unpacked: u64,
    packed: u64,
    /// Bytes of the compressed stream (before encryption).
    stream: u64,
    parts: u64,
}

impl Packer<'_> {
    fn push(&mut self, data: &[u8]) -> io::Result<()> {
        self.unpacked += data.len() as u64;
        let mut data = data;
        // (the wave in the making is filled first; whole waves of `data` are compressed in place)
        if !self.pending.is_empty() {
            let take = (self.wave - self.pending.len()).min(data.len());
            self.pending.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.pending.len() == self.wave {
                let wave = std::mem::take(&mut self.pending);
                self.compress(&wave, false)?;
            }
        }
        while data.len() >= self.wave {
            self.compress(&data[..self.wave], false)?;
            data = &data[self.wave..];
        }
        self.pending.extend_from_slice(data);
        Ok(())
    }

    /// The stream ended: the rest of the data compressed, and the encryption's end.
    fn finish(&mut self) -> io::Result<()> {
        let rest = std::mem::take(&mut self.pending);
        self.compress(&rest, true)?;
        if let Some(a) = &mut self.aes {
            let tail = a.finish();
            self.packed += tail.len() as u64;
            self.w.write_all(&tail)?;
        }
        Ok(())
    }

    fn emit(&mut self, data: &[u8]) -> io::Result<()> {
        self.stream += data.len() as u64;
        match &mut self.aes {
            None => {
                self.packed += data.len() as u64;
                self.w.write_all(data)
            }
            Some(a) => {
                let e = a.feed(data);
                self.packed += e.len() as u64;
                self.w.write_all(&e)
            }
        }
    }

    /// Whole parts of `data` (the last one short where `last`), as one wave.
    fn compress(&mut self, data: &[u8], last: bool) -> io::Result<()> {
        if self.level == 0 {
            return self.emit(data);
        }
        let chunks: Vec<&[u8]> = data.chunks(self.part).collect();
        let (level, dict) = (self.level, self.dict);
        let done = pool::par_map(chunks.len(), self.threads, |i| lzma2(chunks[i], level, dict));
        let n = done.len();
        for (i, d) in done.into_iter().enumerate() {
            let mut d = d?;
            self.parts += 1;
            // (the end byte of the stream is the last part's)
            if !(last && i + 1 == n) {
                d.pop();
            }
            self.emit(&d)?;
        }
        // (a stream ending where a wave does: the end byte alone)
        if last && n == 0 && self.parts > 0 {
            self.emit(&[0])?;
        }
        Ok(())
    }
}

/// What the header tells of a folder's coders: the coders and the sizes of their outputs.
fn folder(coders: &[Coder]) -> Vec<u8> {
    let mut f = Vec::new();
    number(&mut f, coders.len() as u64);
    for c in coders {
        f.push(c.id.len() as u8 | if c.props.is_empty() { 0 } else { 0x20 });
        f.extend_from_slice(c.id);
        if !c.props.is_empty() {
            number(&mut f, c.props.len() as u64);
            f.extend_from_slice(&c.props);
        }
    }
    // (the input of each coder from the output of the next, the last one's the packed stream)
    for i in 0..coders.len() as u64 - 1 {
        number(&mut f, i);
        number(&mut f, i + 1);
    }
    f
}

/// The streams info of one folder of `coders` (the sizes of their outputs given), whose packed
/// stream of `pack` bytes is at `pack_pos`; with `subs` (the sizes and CRC-32s of the files), a
/// substreams info, else the folder's CRC-32 `crc`.
fn streams(
    pack_pos: u64,
    pack: u64,
    coders: &[Coder],
    sizes: &[u64],
    crc: Option<u32>,
    subs: &[(u64, u32)],
) -> Vec<u8> {
    let mut h = vec![id::PACK_INFO];
    number(&mut h, pack_pos);
    number(&mut h, 1);
    h.push(id::SIZE);
    number(&mut h, pack);
    h.push(id::END);
    h.extend([id::UNPACK_INFO, id::FOLDER]);
    number(&mut h, 1);
    h.push(0);
    h.extend(folder(coders));
    h.push(id::UNPACK_SIZE);
    sizes.iter().for_each(|&s| number(&mut h, s));
    if let Some(c) = crc {
        h.extend([id::CRC, 1]);
        h.extend(c.to_le_bytes());
    }
    h.push(id::END);
    if !subs.is_empty() {
        h.extend([id::SUBSTREAMS, id::NUM_UNPACK_STREAM]);
        number(&mut h, subs.len() as u64);
        if subs.len() > 1 {
            h.push(id::SIZE);
            subs[..subs.len() - 1].iter().for_each(|s| number(&mut h, s.0));
        }
        h.extend([id::CRC, 1]);
        subs.iter().for_each(|s| h.extend(s.1.to_le_bytes()));
        h.push(id::END);
    }
    h.push(id::END);
    h
}

/// A property of the files, of its id and body.
fn property(h: &mut Vec<u8>, id: u8, body: &[u8]) {
    h.push(id);
    number(h, body.len() as u64);
    h.extend_from_slice(body);
}

/// A property of values (`width` bytes each) of the files that have them.
fn defined(h: &mut Vec<u8>, id: u8, v: &[Option<u64>], width: usize) {
    if v.iter().all(Option::is_none) {
        return;
    }
    let mut b = Vec::new();
    if v.iter().all(Option::is_some) {
        b.push(1);
    } else {
        b.push(0);
        b.extend(bits(v.iter().map(Option::is_some)));
    }
    b.push(0);
    v.iter().flatten().for_each(|x| b.extend_from_slice(&x.to_le_bytes()[..width]));
    property(h, id, &b);
}

/// The attributes of item `it` of metadata `x`: Windows' kept, the directory bit, and the Unix
/// mode where known (a link's always).
fn attributes(it: &Item, x: &Extra) -> Option<u32> {
    let (dir, link) = (matches!(it.kind, Kind::Dir), matches!(it.kind, Kind::Link(_)));
    let mut a = x.win_attrs.unwrap_or(0) & aqr_formula::aqr::WIN_ATTRS_ALLOWED;
    if dir {
        a |= DOS_DIR;
    }
    if x.mode.is_some() || link {
        let perm = x.mode.unwrap_or(0o777) & 0o7777;
        if x.win_attrs.is_none() && perm & 0o222 == 0 {
            a |= DOS_READ_ONLY;
        }
        let kind = if dir {
            S_IFDIR
        } else if link {
            S_IFLNK
        } else {
            S_IFREG
        };
        a |= UNIX_EXTENSION | (kind | perm) << 16;
    }
    (a != 0).then_some(a)
}

/// The header of `items` (`has`: whether each has data, in order, over the folder in `main`, if any).
fn header(t: &Tree, items: &[Item], has: &[bool], notes: &mut Vec<String>, main: &[u8]) -> Vec<u8> {
    let mut h = vec![id::HEADER];
    if !main.is_empty() {
        h.push(id::MAIN_STREAMS);
        h.extend_from_slice(main);
    }
    if items.is_empty() {
        h.push(id::END);
        return h;
    }
    h.push(id::FILES);
    number(&mut h, items.len() as u64);
    if has.iter().any(|h| !h) {
        let empty: Vec<bool> = has.iter().map(|h| !h).collect();
        property(&mut h, id::EMPTY_STREAM, &bits(empty.iter().copied()));
        let files: Vec<bool> =
            items.iter().zip(has).filter(|(_, h)| !**h).map(|(it, _)| !matches!(it.kind, Kind::Dir)).collect();
        if files.iter().any(|&f| f) {
            property(&mut h, id::EMPTY_FILE, &bits(files.into_iter()));
        }
    }
    let mut names = vec![0u8];
    for it in items {
        if !it.utf8 {
            notes.push(format!("{}: a name not in UTF-8, written lossily in 7z", String::from_utf8_lossy(&it.path)));
        }
        String::from_utf8_lossy(&it.path).encode_utf16().chain([0]).for_each(|u| names.extend(u.to_le_bytes()));
    }
    property(&mut h, id::NAME, &names);
    let extra = |i: &Item| &t.entries[i.entry].extra;
    let times = |f: fn(&Extra) -> Option<crate::Time>| -> Vec<Option<u64>> {
        // (a local time of an unknown time zone as local time here)
        items.iter().map(|i| f(extra(i)).and_then(time::instant).map(time::to_filetime)).collect()
    };
    defined(&mut h, id::CTIME, &times(|x| x.crtime), 8);
    defined(&mut h, id::ATIME, &times(|x| x.atime), 8);
    defined(&mut h, id::MTIME, &times(|x| x.mtime), 8);
    let attrs: Vec<Option<u64>> = items.iter().map(|i| attributes(i, extra(i)).map(u64::from)).collect();
    defined(&mut h, id::WIN_ATTRIBUTES, &attrs, 4);
    h.extend([id::END, id::END]);
    h
}

/// The signature header: the next header's offset, size and CRC-32.
fn signature(offset: u64, len: u64, crc: u32) -> Vec<u8> {
    let mut start = Vec::with_capacity(20);
    start.extend(offset.to_le_bytes());
    start.extend(len.to_le_bytes());
    start.extend(crc.to_le_bytes());
    let mut out = MAGIC.to_vec();
    out.extend([0, 4]);
    out.extend(crc32::hash(&start).to_le_bytes());
    out.extend(start);
    out
}

/// Writes tree `t`, the contents of its files read from `c`, as a 7z archive to `w`, telling
/// `tell` how far it is.
pub fn write(t: &Tree, c: &dyn Contents, o: &Options, w: &mut dyn Output, tell: &Tell) -> io::Result<Written> {
    let mut out = Written::default();
    let keeps = Keeps { special: false, xattrs: false, links: true };
    let items = items(t, "7z", keeps, &mut out.notes);
    let level = o.level_of(Format::SevenZip);
    let threads = o.threads.max(1);
    let size = |it: &Item| match &it.kind {
        Kind::File(k) => c.size(*k),
        Kind::Link(to) => to.len() as u64,
        _ => 0,
    };
    let total: u64 = items.iter().map(size).sum();
    let dict_max = xz_dict(level);
    let part = total.div_ceil(threads as u64).clamp(1 << 20, 4 * dict_max);
    let wave = (part * threads as u64).min(o.batch.max(part)) / part * part;
    // (the key once; the IV of the folder)
    let key = o.passphrase.as_ref().map(|p| crypt::sevenz_key(p, &[], crypt::SEVENZ_POWER));
    let iv = key.as_ref().map(|_| crate::random::<16>()).transpose()?;
    let base = w.stream_position()?;
    w.write_all(&[0; SIGNATURE])?;
    tell(Progress::Done { done: 0, total });
    let mut pk = Packer {
        w: &mut *w,
        level,
        threads,
        part: part as usize,
        wave: wave as usize,
        dict: dictionary(level, part.min(total)),
        pending: Vec::new(),
        aes: key.as_ref().zip(iv.as_ref()).map(|(k, iv)| crypt::SevenzEncryptor::new(k, iv)),
        unpacked: 0,
        packed: 0,
        stream: 0,
        parts: 0,
    };
    // the size and CRC-32 of each item with data, in order
    let mut subs: Vec<(u64, u32)> = Vec::new();
    let mut has = vec![false; items.len()];
    let mut done = 0u64;
    for range in batches(&items, wave, size) {
        let start = range.start;
        let batch = &items[range];
        let read = pool::par_map(batch.len(), threads, |i| -> io::Result<Option<(Bytes, u32)>> {
            match &batch[i].kind {
                Kind::File(k) if c.size(*k) > 0 => {
                    tell(Progress::Started(*k));
                    let d = c.read(*k)?;
                    let crc = crc32::hash(&d);
                    tell(Progress::Ended(*k));
                    Ok(Some((d, crc)))
                }
                Kind::Link(to) if !to.is_empty() => Ok(Some((Bytes::from(to.clone()), crc32::hash(to)))),
                _ => Ok(None),
            }
        });
        for (i, r) in read.into_iter().enumerate() {
            if let Some((d, crc)) = r? {
                if d.is_empty() {
                    continue;
                }
                pk.push(&d)?;
                subs.push((d.len() as u64, crc));
                has[start + i] = true;
                out.read += d.len() as u64;
                done += size(&items[start + i]);
                tell(Progress::Done { done, total });
            }
        }
    }
    pk.finish()?;
    let (unpacked, packed, stream) = (pk.unpacked, pk.packed, pk.stream);
    let main = if subs.is_empty() {
        Vec::new()
    } else {
        let main_coder = if level == 0 {
            Coder { id: COPY, props: Vec::new() }
        } else {
            Coder { id: LZMA2, props: vec![dict_prop(dictionary(level, part.min(total)))] }
        };
        let mut coders = vec![main_coder];
        let mut sizes = vec![unpacked];
        if let Some(iv) = &iv {
            coders.push(Coder { id: SEVENZ_AES_ID, props: crypt::SevenzProps::written(iv) });
            sizes.push(stream);
        }
        streams(0, packed, &coders, &sizes, None, &subs)
    };
    let mut h = header(t, &items, &has, &mut out.notes, &main);
    let mut at = SIGNATURE as u64 + packed;
    if let Some(key) = &key {
        // the header packed apart: LZMA2, then AES
        let iv = crate::random::<16>()?;
        let hd = dictionary(6, (h.len() as u64).next_power_of_two());
        let lz = lzma2(&h, 6, hd)?;
        let enc = crypt::sevenz_aes_encrypt(key, &iv, &lz);
        let coders = [
            Coder { id: LZMA2, props: vec![dict_prop(hd)] },
            Coder { id: SEVENZ_AES_ID, props: crypt::SevenzProps::written(&iv) },
        ];
        let s =
            streams(packed, enc.len() as u64, &coders, &[h.len() as u64, lz.len() as u64], Some(crc32::hash(&h)), &[]);
        w.write_all(&enc)?;
        at += enc.len() as u64;
        h = vec![id::ENCODED_HEADER];
        h.extend(s);
    }
    w.write_all(&h)?;
    let sig = signature(at - SIGNATURE as u64, h.len() as u64, crc32::hash(&h));
    let end = at + h.len() as u64;
    w.seek(SeekFrom::Start(base))?;
    w.write_all(&sig)?;
    w.seek(SeekFrom::Start(base + end))?;
    out.len = end;
    Ok(out)
}
