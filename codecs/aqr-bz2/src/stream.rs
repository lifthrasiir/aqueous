//! bzip2 streams decoded: the data, and what a compressor's output is compared by.

use aqr_common::pool;

use crate::huffman::{self, Decoder};
use crate::{BLOCK_MAGIC, END_MAGIC, Error, MAGIC, Result, corrupt, crc};

/// A stream decoded.
#[derive(Debug, Default)]
pub struct Stream {
    /// The level, 1..9: blocks of up to `100000 * level` bytes of RLE1 output.
    pub level: u8,
    /// The data (empty where it was not asked for).
    pub data: Vec<u8>,
    /// Bytes of data in each block.
    pub sizes: Vec<usize>,
    /// The rank of each block among its rotations (`origPtr`).
    pub orig_ptrs: Vec<u32>,
    /// Bytes of the stream, to its end padded to a byte.
    pub len: usize,
}

/// Groups of symbols coded by one table.
pub const GROUP: usize = 50;
/// Tables of a block, at most.
pub const MAX_GROUPS: usize = 6;
/// Selectors of a block, at most, that libbzip2 decodes (more are an error since 1.0.8).
pub const MAX_SELECTORS: usize = 18002;

/// A reader of bits, MSB first, reading zeros past the end.
struct Bits<'a> {
    d: &'a [u8],
    /// The next byte to load.
    p: usize,
    acc: u64,
    /// Bits in `acc`, its low ones.
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], byte: usize) -> Self {
        Bits { d, p: byte, acc: 0, n: 0 }
    }

    #[inline(always)]
    fn refill(&mut self) {
        while self.n <= 56 {
            let b = self.d.get(self.p).copied().unwrap_or(0);
            self.p += 1;
            self.acc = (self.acc << 8) | b as u64;
            self.n += 8;
        }
    }

    /// The next 32 bits, without reading them.
    #[inline(always)]
    fn peek32(&mut self) -> u32 {
        if self.n < 32 {
            self.refill();
        }
        (self.acc >> (self.n - 32)) as u32
    }

    #[inline(always)]
    fn skip(&mut self, k: u32) {
        self.n -= k;
    }

    /// `k` bits, 1..=32.
    #[inline(always)]
    fn get(&mut self, k: u32) -> u32 {
        let v = self.peek32() >> (32 - k);
        self.n -= k;
        v
    }

    fn bit(&mut self) -> bool {
        self.get(1) != 0
    }

    fn get48(&mut self) -> u64 {
        (self.get(24) as u64) << 24 | self.get(24) as u64
    }

    /// Bits read so far.
    fn pos(&self) -> usize {
        self.p * 8 - self.n as usize
    }

    fn overrun(&self) -> bool {
        self.pos() > self.d.len() * 8
    }
}

/// The level of a stream header at the start of `d`.
pub fn level(d: &[u8]) -> Option<u8> {
    match d {
        [b'B', b'Z', b'h', l @ b'1'..=b'9', ..] if d.starts_with(&MAGIC) => Some(l - b'0'),
        _ => None,
    }
}

/// Decodes the stream at the start of `d`, its data only where `data` is set (else only its
/// extent and blocks are found, and their CRCs are not checked), giving up past `max_data`
/// bytes of data.
pub fn decode(d: &[u8], data: bool, max_data: usize) -> Result<Stream> {
    decode_each(d, data, max_data, &mut |_, _| {})
}

/// [`decode`], with each block's symbols after the BWT and its `origPtr` given to `each`.
pub fn decode_each(d: &[u8], data: bool, max_data: usize, each: &mut dyn FnMut(&[u8], u32)) -> Result<Stream> {
    let mut b = Blocks::new(d)?;
    let mut s = Stream { level: b.level, ..Stream::default() };
    let (mut last, mut links) = (Vec::new(), Vec::new());
    let mut total = 0usize;
    while let Some((orig, want, size)) = b.next(&mut last)? {
        s.orig_ptrs.push(orig);
        each(&last, orig);
        if data {
            let at = s.data.len();
            undo(&last, orig, want, &mut links, &mut s.data, max_data)?;
            s.sizes.push(s.data.len() - at);
        } else {
            s.sizes.push(size);
        }
        total += *s.sizes.last().unwrap();
        if total > max_data {
            return Err(Error("too much data"));
        }
    }
    s.len = b.finish(data)?;
    Ok(s)
}

/// The blocks of a stream read one after another, up to the symbols after the BWT.
struct Blocks<'a> {
    r: Bits<'a>,
    level: u8,
    combined: u32,
}

impl<'a> Blocks<'a> {
    fn new(d: &'a [u8]) -> Result<Self> {
        let level = level(d).ok_or(Error("not a bzip2 stream"))?;
        Ok(Blocks { r: Bits::new(d, 4), level, combined: 0 })
    }

    /// The next block into `last`, with its `origPtr`, CRC and RLE1 symbols; None at the end
    /// of the stream.
    fn next(&mut self, last: &mut Vec<u8>) -> Result<Option<(u32, u32, usize)>> {
        let r = &mut self.r;
        match r.get48() {
            BLOCK_MAGIC => {}
            END_MAGIC => return Ok(None),
            _ => return Err(corrupt()),
        }
        let want = r.get(32);
        if r.bit() {
            return Err(Error("randomised block"));
        }
        let orig = r.get(24);
        let size = block(r, self.level, last)?;
        if orig as usize >= last.len() {
            return Err(corrupt());
        }
        self.combined = self.combined.rotate_left(1) ^ want;
        Ok(Some((orig, want, size)))
    }

    /// Bytes of the stream read so far, to the end of the last block (its last byte with it).
    fn read(&self) -> usize {
        self.r.pos().div_ceil(8)
    }

    /// After the last block: the bytes of the stream, its combined CRC checked where `data`.
    fn finish(&mut self, data: bool) -> Result<usize> {
        let want = self.r.get(32);
        if self.r.overrun() {
            return Err(Error("truncated"));
        }
        if data && want != self.combined {
            return Err(Error("CRC mismatch"));
        }
        Ok(self.r.pos().div_ceil(8))
    }
}

/// A block's data from its symbols after the BWT onto `out`, its CRC checked.
fn undo(last: &[u8], orig: u32, want: u32, links: &mut Vec<u32>, out: &mut Vec<u8>, max_data: usize) -> Result<()> {
    let at = out.len();
    unbwt(last, orig, links);
    unrle(last, links, orig, out, max_data)?;
    if crc::hash(&out[at..]) != want {
        return Err(Error("CRC mismatch"));
    }
    Ok(())
}

/// A stream decoded as far as some data, to be decoded on ([`Partial::finish`]): to tell from
/// its start whether to decode all of it.
pub struct Partial<'a> {
    blocks: Blocks<'a>,
    /// The data so far, of whole blocks.
    pub data: Vec<u8>,
    sizes: Vec<usize>,
    orig_ptrs: Vec<u32>,
    /// The bytes of the stream, where it ended.
    pub ended: Option<usize>,
}

impl<'a> Partial<'a> {
    /// The stream at the start of `d` decoded block by block until there are `want` bytes of
    /// data or it ends; past `max_data` bytes an error.
    pub fn start(d: &'a [u8], want: usize, max_data: usize) -> Result<Self> {
        let mut p = Partial {
            blocks: Blocks::new(d)?,
            data: Vec::new(),
            sizes: Vec::new(),
            orig_ptrs: Vec::new(),
            ended: None,
        };
        let (mut last, mut links) = (Vec::new(), Vec::new());
        while p.data.len() < want {
            let Some((orig, crc, _)) = p.blocks.next(&mut last)? else {
                p.ended = Some(p.blocks.finish(true)?);
                break;
            };
            let at = p.data.len();
            undo(&last, orig, crc, &mut links, &mut p.data, max_data)?;
            p.sizes.push(p.data.len() - at);
            p.orig_ptrs.push(orig);
        }
        Ok(p)
    }

    /// Bytes of the stream that the data so far was coded in.
    pub fn coded(&self) -> usize {
        self.ended.unwrap_or_else(|| self.blocks.read())
    }

    /// The whole stream: the rest of its blocks read in turn, their data made on up to
    /// `threads` threads (a wave of blocks at a time, so that what is too much data is found
    /// before much more of it is made).
    pub fn finish(mut self, threads: usize, max_data: usize) -> Result<Stream> {
        let level = self.blocks.level;
        let mut s = Stream { level, data: self.data, sizes: self.sizes, orig_ptrs: self.orig_ptrs, len: 0 };
        if let Some(len) = self.ended {
            s.len = len;
            return Ok(s);
        }
        let wave = threads.max(1) * 2;
        loop {
            let mut read = Vec::with_capacity(wave);
            let mut last = Vec::new();
            while read.len() < wave {
                match self.blocks.next(&mut last)? {
                    Some((orig, crc, _)) => read.push((std::mem::take(&mut last), orig, crc)),
                    None => break,
                }
            }
            let end = read.len() < wave;
            let left = max_data.saturating_sub(s.data.len());
            let made = pool::par_map(read.len(), threads, |k| {
                let (last, orig, crc) = &read[k];
                let mut out = Vec::new();
                undo(last, *orig, *crc, &mut Vec::new(), &mut out, left).map(|()| out)
            });
            for (out, (_, orig, _)) in made.into_iter().zip(&read) {
                let out = out?;
                if s.data.len() + out.len() > max_data {
                    return Err(Error("too much data"));
                }
                s.data.extend_from_slice(&out);
                s.sizes.push(out.len());
                s.orig_ptrs.push(*orig);
            }
            if end {
                break;
            }
        }
        s.len = self.blocks.finish(true)?;
        Ok(s)
    }
}

/// Reads a block after its header into `last` (the symbols after the BWT, before RLE1 is
/// undone), giving an estimate of its data: its RLE1 symbols.
fn block(r: &mut Bits, level: u8, last: &mut Vec<u8>) -> Result<usize> {
    // the symbols in use
    let used16 = r.get(16);
    let mut seq = Vec::with_capacity(256);
    for i in 0..16 {
        if used16 >> (15 - i) & 1 != 0 {
            let u = r.get(16);
            seq.extend((0..16).filter(|j| u >> (15 - j) & 1 != 0).map(|j| (i * 16 + j) as u8));
        }
    }
    if seq.is_empty() {
        return Err(corrupt());
    }
    let alpha = seq.len() + 2;
    let groups = r.get(3) as usize;
    let nsel = r.get(15) as usize;
    if !(2..=MAX_GROUPS).contains(&groups) || !(1..=MAX_SELECTORS).contains(&nsel) {
        return Err(corrupt());
    }
    let mut mtf: Vec<u8> = (0..groups as u8).collect();
    let mut selectors = Vec::with_capacity(nsel);
    for _ in 0..nsel {
        let mut j = 0;
        while r.bit() {
            j += 1;
            if j >= groups {
                return Err(corrupt());
            }
        }
        let t = mtf[j];
        mtf.copy_within(0..j, 1);
        mtf[0] = t;
        selectors.push(t);
    }
    let mut tables = Vec::with_capacity(groups);
    let mut len = vec![0u8; alpha];
    for _ in 0..groups {
        let mut cur = r.get(5);
        for l in &mut len {
            loop {
                if !(1..=huffman::MAX_LEN).contains(&cur) {
                    return Err(corrupt());
                }
                if !r.bit() {
                    break;
                }
                if r.bit() { cur -= 1 } else { cur += 1 }
            }
            *l = cur as u8;
        }
        tables.push(Decoder::new(&len)?);
    }
    if r.overrun() {
        return Err(Error("truncated"));
    }

    // the symbols: RUNA and RUNB for runs of the front symbol, MTF values, EOB
    let max = 100_000 * level as usize;
    let eob = (alpha - 1) as u16;
    last.clear();
    let mut front: Vec<u8> = (0..seq.len()).map(|i| i as u8).collect();
    let (mut run, mut weight) = (0usize, 1usize);
    let mut rle_syms = 0;
    'groups: for &t in &selectors {
        let tab = &tables[t as usize];
        for _ in 0..GROUP {
            let (sym, l) = tab.decode(r.peek32()).ok_or_else(corrupt)?;
            r.skip(l);
            if sym <= 1 {
                run += weight << sym;
                weight <<= 1;
                if run > max {
                    return Err(corrupt());
                }
                continue;
            }
            if run > 0 {
                if last.len() + run > max {
                    return Err(corrupt());
                }
                last.resize(last.len() + run, seq[front[0] as usize]);
                rle_syms += run;
                run = 0;
                weight = 1;
            }
            if sym == eob {
                break 'groups;
            }
            if last.len() >= max {
                return Err(corrupt());
            }
            let j = sym as usize - 1;
            let c = front[j];
            front.copy_within(0..j, 1);
            front[0] = c;
            last.push(seq[c as usize]);
            rle_syms += 1;
        }
        if r.overrun() {
            return Err(Error("truncated"));
        }
    }
    // (the last group ends with EOB, which the loop breaks at)
    if last.is_empty() || r.overrun() || run > 0 {
        return Err(corrupt());
    }
    Ok(rle_syms)
}

/// The links of the inverse BWT: for each row, the row of the rotation one symbol on.
fn unbwt(last: &[u8], orig: u32, links: &mut Vec<u32>) {
    let _ = orig;
    let mut start = [0u32; 256];
    for &c in last {
        start[c as usize] += 1;
    }
    let mut sum = 0;
    for s in &mut start {
        let c = *s;
        *s = sum;
        sum += c;
    }
    links.clear();
    links.resize(last.len(), 0);
    for (i, &c) in last.iter().enumerate() {
        links[start[c as usize] as usize] = i as u32;
        start[c as usize] += 1;
    }
}

/// The block's data onto `out`: its RLE1 symbols in order, with each run of 4 equal bytes
/// followed by a count of more.
fn unrle(last: &[u8], links: &[u32], orig: u32, out: &mut Vec<u8>, max_data: usize) -> Result<()> {
    let mut p = links[orig as usize];
    let (mut prev, mut same) = (-1i32, 0);
    for _ in 0..last.len() {
        let c = last[p as usize];
        p = links[p as usize];
        if same == 4 {
            out.resize(out.len() + c as usize, prev as u8);
            if out.len() > max_data {
                return Err(Error("too much data"));
            }
            same = 0;
            prev = -1;
            continue;
        }
        if c as i32 == prev {
            same += 1;
        } else {
            prev = c as i32;
            same = 1;
        }
        out.push(c);
    }
    if same == 4 {
        // (a run of 4 without its count, which libbzip2 never writes)
        return Err(corrupt());
    }
    Ok(())
}
