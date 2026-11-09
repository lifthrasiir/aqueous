//! `png-recons` as a part module (see FORMAT.md): the inputs taken and the file put out in order
//! through windows, one image at a time, so that what it holds follows the structure of the file
//! and not its length.
//!
//! The skeleton and the index are read whole (a chunk takes bytes of them, not the image's);
//! the heads of the records, the rANS streams and the raw bits of the models are taken in order
//! in pieces, and the file is put out as it is made: the chunks of the skeleton as they come,
//! an image's payload written block by block by `Payload` and cut into its chunks as it
//! goes, each chunk's CRC taken over its data on the way. The model of an image holds the
//! rows it needs and a block of tokens; the layout of an image's blocks is held (a few
//! bytes a block).

use aqr_api::stream::{In, Pull, Put, Slice, Written};
use aqr_common::crc32::Hasher;
use aqr_common::rans32::{Feed, StreamBits, StreamDecoder};

use crate::codec::{self, Drain, FDAT, Layout, PLAIN, Payload, Skeleton, get_varint};
use crate::model::{self, Geometry, Io, Span};
use crate::png;
use crate::rans::Tables;
use crate::{Result, corrupt};

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 4;

/// Bytes of the pieces the heads of the records and the streams of the models are taken in, and
/// that the file is put out in.
pub const PIECE: usize = 16 << 10;

/// The inputs in order: the skeleton, the index, the heads, the rANS streams and the raw bits.
pub const INPUTS: usize = 5;

/// The output put out in pieces of [`PIECE`] bytes.
struct Out<'a> {
    buf: Vec<u8>,
    sink: &'a mut dyn Put,
}

impl<'a> Out<'a> {
    fn new(sink: &'a mut dyn Put) -> Out<'a> {
        Out { buf: Vec::with_capacity(PIECE), sink }
    }

    fn put(&mut self, d: &[u8]) -> Result<()> {
        if self.buf.len() + d.len() > PIECE {
            self.flush()?;
            if d.len() > PIECE {
                return self.sink.put(d).ok_or_else(corrupt);
            }
        }
        self.buf.extend_from_slice(d);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.sink.put(&self.buf).ok_or_else(corrupt)?;
        self.buf.clear();
        Ok(())
    }
}

/// An input read in order: whole, or in pieces.
struct Cursor<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    at: usize,
    end: usize,
    /// Bytes of the input not pulled yet.
    left: u64,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a mut dyn Pull, len: u64, piece: usize) -> Cursor<'a> {
        Cursor { src, buf: vec![0; piece], at: 0, end: 0, left: len }
    }

    fn fill(&mut self) -> bool {
        self.at = 0;
        self.end = self.src.pull(&mut self.buf);
        self.left = self.left.saturating_sub(self.end as u64);
        self.end > 0
    }

    fn byte(&mut self) -> Result<u8> {
        if self.at == self.end && !self.fill() {
            return Err(corrupt());
        }
        self.at += 1;
        Ok(self.buf[self.at - 1])
    }

    /// The next `n` bytes, handed on in pieces.
    fn take(&mut self, mut n: usize, mut each: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        while n > 0 {
            if self.at == self.end && !self.fill() {
                return Err(corrupt());
            }
            let k = (self.end - self.at).min(n);
            each(&self.buf[self.at..self.at + k])?;
            self.at += k;
            n -= k;
        }
        Ok(())
    }

    fn read(&mut self, n: usize) -> Result<Vec<u8>> {
        // (no more than there is: a length that is damaged does not reserve what is not there)
        if n as u64 > (self.end - self.at) as u64 + self.left {
            return Err(corrupt());
        }
        let mut v = Vec::with_capacity(n);
        self.take(n, |d| {
            v.extend_from_slice(d);
            Ok(())
        })?;
        Ok(v)
    }

    /// Whether every byte was taken.
    fn done(&mut self) -> bool {
        self.at == self.end && !self.fill()
    }
}

/// The whole input of `len` bytes.
fn whole(src: &mut dyn Pull, len: u64) -> Result<Vec<u8>> {
    let n = usize::try_from(len).map_err(|_| corrupt())?;
    // (and a piece to read it by, which the input fills the vector with)
    let mut v = vec![0; n];
    let mut at = 0;
    while at < n {
        match src.pull(&mut v[at..]) {
            0 => return Err(corrupt()),
            k => at += k,
        }
    }
    Ok(v)
}

/// The stream of a model: `left` bytes of an input, taken a piece at a time.
struct Limited<'a> {
    src: &'a mut dyn Pull,
    left: u64,
}

impl Limited<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let k = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let got = if k == 0 { 0 } else { self.src.pull(&mut buf[..k]) };
        self.left -= got as u64;
        got
    }

    /// Takes what is left of the stream, as the whole decoding passes over it.
    fn skip(&mut self) {
        let mut sink = [0u8; 256];
        while self.pull(&mut sink) > 0 {}
    }
}

/// The model's source of symbols: its rANS decoder and its raw bits, read as they come.
struct Live<'a, F: FnMut(&mut [u8]) -> usize, G: FnMut(&mut [u8]) -> usize> {
    tables: &'a Tables,
    dec: StreamDecoder<F>,
    raw: StreamBits<G>,
}

impl<F: FnMut(&mut [u8]) -> usize, G: FnMut(&mut [u8]) -> usize> Io for Live<'_, F, G> {
    #[inline(always)]
    fn sym(&mut self, t: usize, _: u32) -> u32 {
        self.dec.get(self.tables, t)
    }
    #[inline(always)]
    fn bit(&mut self, t: usize, _: u32) -> u32 {
        self.dec.bit(self.tables, t)
    }
    #[inline(always)]
    fn raw(&mut self, k: u32, _: u32) -> u32 {
        self.raw.bits(k)
    }
}

/// The file being put out: the chunks of the skeleton in order, the payload of an image in
/// the chunks it is cut into.
struct Asm<'a> {
    sk: &'a Skeleton<'a>,
    out: Out<'a>,
    /// The next chunk to put out, the bytes of its payload left, and its CRC so far.
    next: usize,
    left: usize,
    crc: Hasher,
}

impl<'a> Asm<'a> {
    fn new(sk: &'a Skeleton<'a>, sink: &'a mut dyn Put) -> Result<Asm<'a>> {
        let mut out = Out::new(sink);
        out.put(&png::SIGNATURE)?;
        Ok(Asm { sk, out, next: 0, left: 0, crc: Hasher::new() })
    }

    /// The chunk's CRC as the skeleton gives it, else the one taken.
    fn end_chunk(&mut self) -> Result<()> {
        let crc = std::mem::replace(&mut self.crc, Hasher::new()).finalize();
        self.out.put(&self.sk.chunks[self.next].crc.unwrap_or(crc).to_be_bytes())?;
        self.next += 1;
        Ok(())
    }

    /// Puts out the plain chunks from the next on, up to the chunk `to`.
    fn plain(&mut self, to: usize) -> Result<()> {
        while self.next < to {
            let c = &self.sk.chunks[self.next];
            if self.sk.classes[self.next] != PLAIN || self.left != 0 {
                return Err(corrupt());
            }
            self.out.put(&(c.len as u32).to_be_bytes())?;
            self.out.put(&c.kind)?;
            self.crc.update(&c.kind);
            self.crc.update(c.data);
            self.out.put(c.data)?;
            self.end_chunk()?;
        }
        Ok(())
    }

    /// Starts the payload chunk that is next: its length, kind and sequence number.
    fn begin(&mut self) -> Result<()> {
        let c = &self.sk.chunks[self.next];
        self.out.put(&(c.len as u32).to_be_bytes())?;
        self.out.put(&c.kind)?;
        self.crc.update(&c.kind);
        if self.sk.classes[self.next] == FDAT {
            self.crc.update(c.data);
            self.out.put(c.data)?;
            self.left = c.len - 4;
        } else {
            self.left = c.len;
        }
        Ok(())
    }

    /// Puts out the payload `d` of the image whose chunks end at `end`.
    fn feed(&mut self, mut d: &[u8], end: usize) -> Result<()> {
        while !d.is_empty() {
            if self.left == 0 {
                if self.next >= end || self.sk.classes[self.next] == PLAIN {
                    return Err(corrupt());
                }
                self.begin()?;
                if self.left == 0 {
                    self.end_chunk()?;
                    continue;
                }
            }
            let k = self.left.min(d.len());
            self.crc.update(&d[..k]);
            self.out.put(&d[..k])?;
            self.left -= k;
            d = &d[k..];
            if self.left == 0 {
                self.end_chunk()?;
            }
        }
        Ok(())
    }

    /// Ends the image whose chunks end at `end`: the empty chunks left, and nothing else.
    fn end_image(&mut self, end: usize) -> Result<()> {
        while self.left == 0 && self.next < end {
            self.begin()?;
            if self.left != 0 {
                return Err(corrupt());
            }
            self.end_chunk()?;
        }
        (self.left == 0 && self.next == end).then_some(()).ok_or_else(corrupt)
    }

    /// The rest of the file: the plain chunks, the trailer.
    fn finish(&mut self) -> Result<()> {
        self.plain(self.sk.chunks.len())?;
        self.out.put(self.sk.trailer)?;
        self.out.flush()
    }
}

/// An image's payload sent on to the file ([`Asm::feed`]) as it is written.
struct ToFile<'x, 'a> {
    asm: &'x mut Asm<'a>,
    end: usize,
}

impl Drain for ToFile<'_, '_> {
    fn put(&mut self, d: &[u8]) -> Result<()> {
        self.asm.feed(d, self.end)
    }
}

/// An image's payload kept, by a task, for the instance to put out in its place.
struct ToVec<'v>(&'v mut Vec<u8>);

impl Drain for ToVec<'_> {
    fn put(&mut self, d: &[u8]) -> Result<()> {
        self.0.extend_from_slice(d);
        Ok(())
    }
}

/// The lengths of the model's streams (the tables', the rANS stream's and the raw bits') that
/// the head `head` (past its mode byte, which is 1) tells.
pub(crate) fn head_lens(head: &[u8]) -> Result<[u64; 3]> {
    let mut at = 0;
    for _ in 0..2 {
        let n = get_varint(head, &mut at)? as usize;
        codec::get_bytes(head, &mut at, n)?;
    }
    let mut lens = [0u64; 3];
    for l in &mut lens {
        *l = get_varint(head, &mut at)?;
    }
    Ok(lens)
}

/// An image whose record has a head `head` (past its mode byte, which is 1): its payload put to
/// `drain`, its model's streams taken from `sts` and `raws`.
fn coded_image<D: Drain>(
    head: &[u8],
    len: usize,
    ih: &Option<png::Ihdr>,
    drain: D,
    sts: &mut dyn Pull,
    raws: &mut dyn Pull,
) -> Result<()> {
    let g = ih.as_ref().and_then(Geometry::new).ok_or_else(corrupt)?;
    let mut at = 0;
    let part = |at: &mut usize| -> Result<&[u8]> {
        let n = get_varint(head, at)? as usize;
        codec::get_bytes(head, at, n)
    };
    let layout = part(&mut at)?;
    let headers = part(&mut at)?;
    let mut lens = [0u64; 3];
    for l in &mut lens {
        *l = get_varint(head, &mut at)?;
    }
    let tb = codec::get_bytes(head, &mut at, lens[0].try_into().map_err(|_| corrupt())?)?;
    if at != head.len() {
        return Err(corrupt());
    }
    let Layout { kind, n, streams, spans, alts } = codec::parse_layout(layout)?;
    // (a deflate stream is 1032 times its length at most)
    if n as u64 > 1032 * len as u64 + 4096 {
        return Err(corrupt());
    }
    if !g.reachable(len) {
        return Err(corrupt());
    }
    let ntok: usize = spans.iter().map(|s| if let Span::Tokens(c) = s { *c } else { 0 }).sum();
    if ntok > n {
        return Err(corrupt());
    }
    // (and a token takes a bit of the stream at least)
    if ntok as u64 > 8 * len as u64 {
        return Err(corrupt());
    }
    let tables = model::tables_of(tb, model::TABLES)?;
    // the room that a block of the longest span takes written (and a stored block's bytes)
    let (most, stored) = spans.iter().fold((0, 0), |(m, s), span| match *span {
        Span::Tokens(c) => (m.max(c), s),
        Span::Stored(len) => (m, s.max(len)),
    });
    let block = (6 * most + 512).max(stored + 16) + aqr_flate::write::TOKEN_SLACK;
    let (mut st, mut raw) = (Limited { src: sts, left: lens[1] }, Limited { src: raws, left: lens[2] });
    {
        let mut live = Live {
            tables: &tables,
            dec: StreamDecoder::new(Feed::new(|b| st.pull(b), PIECE), lens[1]),
            raw: StreamBits::new(Feed::new(|b| raw.pull(b), PIECE)),
        };
        let mut payload = Payload::draining(streams, alts, headers, block, drain)?;
        model::decode_io(&mut live, kind, &g, &spans, n, &mut |toks, freqs, data, from| {
            // The data from where the block starts (none needed but a stored block's).
            let rest = payload.at().checked_sub(from).and_then(|i| data.get(i..)).unwrap_or_default();
            payload.block(toks, freqs, rest)
        })?;
        let (_, pos) = payload.finish()?;
        if pos != n || !live.dec.finished() || live.raw.overrun() {
            return Err(corrupt());
        }
    }
    // (the streams are taken to their ends, as the whole decoding takes them)
    st.skip();
    raw.skip();
    // (and all of them were there)
    if st.left != 0 || raw.left != 0 {
        return Err(corrupt());
    }
    Ok(())
}

/// Threads the images of a file of small ones are decoded on: the tasks' slots.
pub const THREADS: usize = 8;

/// A file whose images all have a payload of this many bytes at most, and that has several, has
/// them decoded as tasks, a batch at a time; any other has them decoded one at a time in the
/// instance (a task is given a copy of the instance's memory, which a large image would have
/// made large).
pub(crate) const SMALL: usize = 8 << 20;

/// Bytes of records that a batch of images holds before it is run (and at most [`THREADS`] of
/// them).
pub(crate) const BATCH: usize = 8 << 20;

/// The payload bytes of each image of `skel`.
pub(crate) fn payload_lens(skel: &Skeleton) -> Vec<usize> {
    skel.images
        .iter()
        .map(|(range, _)| {
            let skip = if skel.classes[range.start] == FDAT { 4 } else { 0 };
            skel.chunks[range.clone()].iter().map(|c| c.len - skip).sum()
        })
        .collect()
}

/// Whether the images with these payload lengths are decoded as tasks.
pub(crate) fn tasks_used(lens: &[usize]) -> bool {
    lens.len() >= 2 && lens.iter().all(|&l| l <= SMALL)
}

/// An image of a batch read: its payload kept as it is, or its record (the head past its mode
/// byte, the model's rANS stream and raw bits).
enum Item {
    Kept(Vec<u8>),
    Coded { head: Vec<u8>, st: Vec<u8>, raw: Vec<u8> },
}

/// The images `first` on of `skel` that `batch` holds, put out in order: those coded decoded
/// as tasks (their payloads given back, the first error stopping the rest).
fn run_batch(skel: &Skeleton, lens: &[usize], asm: &mut Asm, first: usize, batch: &[Item]) -> Result<()> {
    let mut coded: Vec<usize> = Vec::with_capacity(batch.len());
    coded.extend((0..batch.len()).filter(|&j| matches!(batch[j], Item::Coded { .. })));
    let task = |c: usize| -> Vec<u8> {
        let j = coded[c];
        let Item::Coded { head, st, raw } = &batch[j] else { unreachable!() };
        let len = lens[first + j];
        // (and a byte after it: 0, else a 1 alone for a failure)
        let mut out = Vec::with_capacity(len + 1);
        match coded_image(head, len, &skel.images[first + j].1, ToVec(&mut out), &mut Slice(st), &mut Slice(raw)) {
            Ok(()) => {
                out.push(0);
                out
            }
            Err(_) => vec![1],
        }
    };
    let put = |asm: &mut Asm, j: usize, payload: &[u8]| -> Result<()> {
        let end = skel.images[first + j].0.end;
        asm.plain(skel.images[first + j].0.start)?;
        asm.feed(payload, end)?;
        asm.end_image(end)
    };
    let mut next = 0;
    let kept = |asm: &mut Asm, next: &mut usize, to: usize| -> Result<()> {
        while *next < to {
            if let Item::Kept(v) = &batch[*next] {
                put(asm, *next, v)?;
            }
            *next += 1;
        }
        Ok(())
    };
    aqr_common::pool::par_each_apart(
        coded.len(),
        THREADS,
        2,
        task,
        || {},
        |c, bytes| {
            let j = coded[c];
            kept(asm, &mut next, j)?;
            let len = lens[first + j];
            if bytes.last() != Some(&0) || bytes.len() != len + 1 {
                return Err(corrupt());
            }
            put(asm, j, &bytes[..len])?;
            next = j + 1;
            Ok(())
        },
    )?;
    kept(asm, &mut next, batch.len())
}

/// The module on the five inputs `srcs` (the skeleton, the index, the heads of the records, the
/// rANS streams and the raw bits), put to `sink`; the length is for the caller to check.
pub fn run(srcs: [(&mut dyn Pull, u64); INPUTS], sink: &mut dyn Put) -> Result<()> {
    let [(sk, sk_len), (index, index_len), (heads, heads_len), (sts, sts_len), (raws, raws_len)] = srcs;
    aqr_common::crc32::hash(&[]);
    let sk = whole(sk, sk_len)?;
    let index = whole(index, index_len)?;
    let skel = Skeleton::parse(&sk)?;
    let lens = payload_lens(&skel);
    let mut asm = Asm::new(&skel, sink)?;
    let mut heads = Cursor::new(heads, heads_len, PIECE);
    let mut at = 0;
    if tasks_used(&lens) {
        let (mut sts, mut raws) = (Cursor::new(sts, sts_len, PIECE), Cursor::new(raws, raws_len, PIECE));
        let mut k = 0;
        while k < skel.images.len() {
            let mut batch: Vec<Item> = Vec::with_capacity(THREADS);
            let (first, mut bytes) = (k, 0);
            while k < skel.images.len() && batch.len() < THREADS && bytes < BATCH {
                let n = get_varint(&index, &mut at)? as usize;
                let item = match heads.byte()? {
                    0 if n == lens[k] + 1 => Item::Kept(heads.read(lens[k])?),
                    1 => {
                        let head = heads.read(n.checked_sub(1).ok_or_else(corrupt)?)?;
                        let l = head_lens(&head)?;
                        let st = sts.read(l[1].try_into().map_err(|_| corrupt())?)?;
                        let raw = raws.read(l[2].try_into().map_err(|_| corrupt())?)?;
                        Item::Coded { head, st, raw }
                    }
                    _ => return Err(corrupt()),
                };
                bytes += match &item {
                    Item::Kept(v) => v.len(),
                    Item::Coded { head, st, raw } => head.len() + st.len() + raw.len(),
                };
                batch.push(item);
                k += 1;
            }
            run_batch(&skel, &lens, &mut asm, first, &batch)?;
        }
        asm.finish()?;
        if at != index.len() || !heads.done() || !sts.done() || !raws.done() {
            return Err(corrupt());
        }
        return Ok(());
    }
    for (k, (range, ih)) in skel.images.iter().enumerate() {
        let n = get_varint(&index, &mut at)? as usize;
        let len = lens[k];
        asm.plain(range.start)?;
        let end = range.end;
        match heads.byte()? {
            0 if n == len + 1 => heads.take(len, |d| asm.feed(d, end))?,
            1 => {
                let head = heads.read(n.checked_sub(1).ok_or_else(corrupt)?)?;
                coded_image(&head, len, ih, ToFile { asm: &mut asm, end }, sts, raws)?;
            }
            _ => return Err(corrupt()),
        }
        asm.end_image(end)?;
    }
    asm.finish()?;
    let mut one = [0u8; 1];
    if at != index.len() || !heads.done() || sts.pull(&mut one) != 0 || raws.pull(&mut one) != 0 {
        return Err(corrupt());
    }
    Ok(())
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): its inputs taken and its
/// file put out in order through windows ([`WINDOW`]).
pub fn decode_part(params_len: u64, lens: [u64; INPUTS], size: u64) {
    aqr_api::finish(part(params_len, lens, size));
}

fn part(params_len: u64, lens: [u64; INPUTS], size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let (mut i0, mut i1, mut i2, mut i3, mut i4) = (
        In::<0>::new(lens[0]),
        In::<1>::new(lens[1]),
        In::<2>::new(lens[2]),
        In::<3>::new(lens[3]),
        In::<4>::new(lens[4]),
    );
    let mut out = Written::new(size);
    let srcs: [(&mut dyn Pull, u64); INPUTS] =
        [(&mut i0, lens[0]), (&mut i1, lens[1]), (&mut i2, lens[2]), (&mut i3, lens[3]), (&mut i4, lens[4])];
    run(srcs, &mut out)?;
    (out.total() == size).then_some(()).ok_or_else(corrupt)
}

/// [`decode_part`] on whole inputs, natively: what a part module gives.
pub fn decode_streamed(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[], &[a, b, c, d, e]) = (params, inputs) else { return Err(corrupt()) };
    let (mut s0, mut s1, mut s2, mut s3, mut s4) = (Slice(a), Slice(b), Slice(c), Slice(d), Slice(e));
    let srcs: [(&mut dyn Pull, u64); INPUTS] = [
        (&mut s0, a.len() as u64),
        (&mut s1, b.len() as u64),
        (&mut s2, c.len() as u64),
        (&mut s3, d.len() as u64),
        (&mut s4, e.len() as u64),
    ];
    let mut out = Vec::new();
    run(srcs, &mut out)?;
    (out.len() as u64 == len).then_some(out).ok_or_else(corrupt)
}

// (what the plan counts them, `plan::wasm`)
#[cfg(target_family = "wasm")]
const _: () = {
    use crate::plan::wasm;
    assert!(size_of::<Item>() == wasm::ITEM as usize);
    assert!(size_of::<usize>() == wasm::LEN as usize);
};
