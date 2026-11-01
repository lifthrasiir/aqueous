//! The decoders the formats need, giving their data alone: DEFLATE (whole, or in chunks as it
//! is decoded), LZMA and LZMA2 (aqr-lzma's model, without keeping its packets), bzip2
//! (aqr-bz2's), Zstandard (the zstd crate's), and CRC-64 (of `.xz`).

use std::ops::Range;

use aqr_flate::inflate::{self, Sink};
use aqr_lzma::lzma::{self, Kind, Model, Packet, Props};
use aqr_lzma::rc::Decoder;

/// Room kept free past the data for a token: the longest match and its overshoot as copied.
const ROOM: usize = 258 + 32;

/// The data of a DEFLATE stream as it is decoded, in a vector kept as raw parts (as aqr-flate's
/// own collector does, so that its length stays in a register).
struct Data {
    p: *mut u8,
    len: usize,
    cap: usize,
}

impl Data {
    fn new(cap: usize) -> Data {
        let mut v = std::mem::ManuallyDrop::new(Vec::<u8>::with_capacity(cap + ROOM));
        Data { p: v.as_mut_ptr(), len: 0, cap: v.capacity() }
    }

    #[cold]
    #[inline(never)]
    fn grow(&mut self, extra: usize) {
        let mut v = self.take();
        v.reserve(extra.max(v.len() / 2));
        let mut v = std::mem::ManuallyDrop::new(v);
        (self.p, self.len, self.cap) = (v.as_mut_ptr(), v.len(), v.capacity());
    }

    /// The data, this left empty (and not to be used but to be dropped).
    fn take(&mut self) -> Vec<u8> {
        // SAFETY: the parts are of a vector of `cap` bytes, the first `len` written
        let v = unsafe { Vec::from_raw_parts(self.p, self.len, self.cap) };
        (self.p, self.len, self.cap) = (std::ptr::NonNull::dangling().as_ptr(), 0, 0);
        v
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        drop(self.take());
    }
}

impl Sink for Data {
    #[inline(always)]
    fn reserve(&mut self) {
        if self.cap - self.len < ROOM {
            self.grow(ROOM);
        }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        // SAFETY: `reserve` left room for three literals
        unsafe { *self.p.add(self.len) = b };
        self.len += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, _: bool) {
        // SAFETY: `1 <= dist <=` the bytes so far (checked by the parser), and `reserve` left room
        // for the match and its overshoot
        unsafe { inflate::copy_match(self.p, self.len, dist as usize, len as usize) };
        self.len += len as usize;
    }

    fn stored(&mut self, b: &[u8]) {
        if self.cap - self.len < b.len() + ROOM {
            self.grow(b.len() + ROOM);
        }
        // SAFETY: room made just above
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), self.p.add(self.len), b.len()) };
        self.len += b.len();
    }
}

fn flate_error(e: aqr_flate::Error) -> String {
    format!("DEFLATE: {e}")
}

/// The data of the DEFLATE stream at the start of `raw` (`size` bytes, as far as is known),
/// refusing more than `max` bytes; and the bytes of the stream.
pub fn inflate(raw: &[u8], size: usize, max: usize) -> Result<(Vec<u8>, usize), String> {
    // (no more than DEFLATE can hold, whatever a damaged size says)
    let mut d = Data::new(size.min(max).min(raw.len().saturating_mul(1032)));
    let l = inflate::parse_head_max(raw, usize::MAX, max, &mut d).map_err(flate_error)?;
    Ok((d.take(), l.end))
}

/// Bytes of data given at a time by [`inflate_chunks`] (but for the last).
pub const CHUNK: usize = 4 << 20;
/// What DEFLATE may look back.
const WINDOW: usize = 32 << 10;

/// What a sink stops a decoder by, unwinding (not a panic: no hook runs).
struct Stopped;

/// The data of a DEFLATE stream given in chunks as it is decoded: a vector of which the bytes
/// from the second item on are new, those before what DEFLATE may look back at.
struct Chunks<'f> {
    d: Data,
    /// Where the new bytes start in `d`.
    start: usize,
    give: &'f mut dyn FnMut(Vec<u8>, usize) -> bool,
}

impl Chunks<'_> {
    #[cold]
    #[inline(never)]
    fn flush(&mut self, need: usize) {
        let keep = self.d.len.min(WINDOW);
        let mut next = Data::new(CHUNK + keep + need);
        // SAFETY: both of at least `keep` bytes, apart
        unsafe { std::ptr::copy_nonoverlapping(self.d.p.add(self.d.len - keep), next.p, keep) };
        next.len = keep;
        let full = std::mem::replace(&mut self.d, next).take();
        let start = std::mem::replace(&mut self.start, keep);
        if !(self.give)(full, start) {
            std::panic::resume_unwind(Box::new(Stopped));
        }
    }
}

impl Sink for Chunks<'_> {
    #[inline(always)]
    fn reserve(&mut self) {
        if self.d.cap - self.d.len < ROOM {
            self.flush(ROOM);
        }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        self.d.lit(b);
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt: bool) {
        self.d.mat(len, dist, alt);
    }

    fn stored(&mut self, b: &[u8]) {
        if self.d.cap - self.d.len < b.len() + ROOM {
            self.flush(b.len() + ROOM);
        }
        self.d.stored(b);
    }
}

/// Decodes the DEFLATE stream at the start of `raw`, giving its data in chunks to `give` (a
/// vector whose new bytes start at the index given; false: stop, nothing more decoded, and
/// `Ok(None)` returned). Returns the bytes of the stream.
pub fn inflate_chunks(raw: &[u8], give: &mut dyn FnMut(Vec<u8>, usize) -> bool) -> Result<Option<usize>, String> {
    let mut c = Chunks { d: Data::new(CHUNK.min(raw.len().saturating_mul(4)).max(WINDOW)), start: 0, give };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inflate::parse_head(raw, usize::MAX, &mut c)));
    match r {
        Ok(Ok(l)) => {
            let rest = c.d.take();
            if rest.len() > c.start && !(c.give)(rest, c.start) {
                return Ok(None);
            }
            Ok(Some(l.end))
        }
        Ok(Err(e)) => Err(flate_error(e)),
        Err(p) if p.is::<Stopped>() => Ok(None),
        Err(p) => std::panic::resume_unwind(p),
    }
}

/// An empty vector of room for `n` bytes; an error where the system has not so much (of a
/// damaged size, most likely).
pub fn room(n: usize) -> Result<Vec<u8>, String> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| format!("{n} bytes not to be had"))?;
    Ok(v)
}

/// Appends to `out` the data of the LZMA stream at the start of `input` (of properties `props`,
/// of `size` bytes where known, else up to its end marker), as part of what `out` holds from
/// `base` on (what its matches may reach), refusing more than `limit` bytes in all. Returns the
/// bytes of the stream.
fn lzma_into(
    m: &mut Model,
    c: &mut Decoder,
    out: &mut Vec<u8>,
    base: usize,
    end: Option<usize>,
    limit: usize,
) -> Result<bool, String> {
    let corrupt = || "LZMA: damaged data".to_string();
    loop {
        let pos = out.len();
        if end == Some(pos) {
            return Ok(false);
        }
        if pos >= limit {
            return Err("LZMA: more data than can be held".into());
        }
        let pk = m.code(c, &out[base..], pos - base, Packet::LIT);
        if c.overrun > 4 {
            return Err(corrupt());
        }
        match pk.kind {
            Kind::Lit => out.push(pk.dist as u8),
            Kind::ShortRep => {
                let src = (pos - base).checked_sub(pk.dist as usize + 1).ok_or_else(corrupt)?;
                out.push(out[base + src]);
            }
            _ if pk.dist == lzma::END => return Ok(true),
            _ => {
                let (len, dist) = (pk.len as usize, pk.dist as usize + 1);
                let src = base + (pos - base).checked_sub(dist).ok_or_else(corrupt)?;
                if pos + len > end.unwrap_or(limit).min(limit) {
                    return Err(corrupt());
                }
                if dist >= len {
                    out.extend_from_within(src..src + len);
                } else {
                    out.reserve(len);
                    for j in 0..len {
                        out.push(out[src + j]);
                    }
                }
            }
        }
    }
}

fn lzma_props(b: u8) -> Result<Props, String> {
    Props::from_byte(b).ok_or_else(|| "LZMA: bad properties".to_string())
}

/// The data of the LZMA stream at the start of `input` of properties `props` (as 7z and the
/// `.lzma` header give them), of `size` bytes where known (else up to its end marker), at most
/// `limit` bytes; and the bytes of the stream.
pub fn lzma(input: &[u8], props: u8, size: Option<u64>, limit: usize) -> Result<(Vec<u8>, usize), String> {
    let props = lzma_props(props)?;
    let mut c = Decoder::new(input).ok_or("LZMA: damaged data")?;
    let mut m = Model::new(props);
    let size = size.map(|s| usize::try_from(s).unwrap_or(usize::MAX));
    if size.is_some_and(|s| s > limit) {
        return Err("LZMA: more data than can be held".into());
    }
    let mut out = room(size.unwrap_or(input.len().saturating_mul(4)).min(limit))?;
    let marker = lzma_into(&mut m, &mut c, &mut out, 0, size, limit)?;
    // (a stream of known size may still end with a marker)
    if !marker && !c.finished() {
        let pk = m.code(&mut c, &out, out.len(), Packet::LIT);
        if pk.kind != Kind::Match || pk.dist != lzma::END {
            return Err("LZMA: data after the end".into());
        }
    }
    if c.overrun > 0 {
        return Err("LZMA: cut short".into());
    }
    Ok((out, c.pos))
}

/// Appends to `out` the data of the LZMA2 stream at the start of `input`, refusing more than
/// `limit` bytes of it; returns the bytes of the stream.
pub fn lzma2_into(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, String> {
    lzma2_run(input, out, limit, true)
}

/// Segments of an LZMA2 stream: the span of each and the bytes of its data.
pub type Segments = Vec<(Range<usize>, usize)>;

/// The segments of the LZMA2 stream at the start of `input`, found by its chunks' headers alone:
/// from each reset of the dictionary (which needs new properties and a new state of the coder
/// after it, as XZ Utils' and 7-Zip's decoders have it, so that a segment decodes alone), the
/// span of its chunks and the bytes of its data; and the bytes of the stream.
pub fn lzma2_segments(input: &[u8]) -> Result<(Segments, usize), String> {
    let corrupt = || "LZMA2: damaged data".to_string();
    let mut segs: Segments = Vec::new();
    let mut at = 0;
    loop {
        let &first = input.get(at).ok_or_else(corrupt)?;
        if first == 0 {
            return Ok((segs, at + 1));
        }
        let lzma = first >= 0x80;
        let control = if lzma { first & 0xe0 } else { first };
        if !lzma && control > 2 {
            return Err(corrupt());
        }
        let h = input.get(at..at + if lzma { 5 } else { 3 }).ok_or_else(corrupt)?;
        let len = (if lzma { (first as usize & 0x1f) << 16 } else { 0 } | (h[1] as usize) << 8 | h[2] as usize) + 1;
        let size = match lzma {
            true => (if control >= 0xc0 { 6 } else { 5 }) + ((h[3] as usize) << 8 | h[4] as usize) + 1,
            false => 3 + len,
        };
        if control == 1 || control == 0xe0 {
            segs.push((at..at, 0));
        }
        let s = segs.last_mut().ok_or_else(corrupt)?;
        at += size;
        (s.0.end, s.1) = (at, s.1 + len);
    }
}

/// The data of the LZMA2 stream at the start of `input`, at most `limit` bytes, its segments
/// ([`lzma2_segments`]) decoded on up to `threads` threads; and the bytes of the stream.
pub fn lzma2_par(input: &[u8], limit: usize, threads: usize) -> Result<(Vec<u8>, usize), String> {
    let (segs, used) = lzma2_segments(input)?;
    let total = segs.iter().map(|s| s.1).sum::<usize>();
    if total > limit {
        return Err("LZMA2: more data than can be held".into());
    }
    if segs.len() <= 1 || threads <= 1 {
        let mut out = room(total)?;
        lzma2_into(input, &mut out, limit)?;
        return Ok((out, used));
    }
    let parts = aqr_common::pool::par_map(segs.len(), threads, |k| {
        let (span, n) = &segs[k];
        let mut out = room(*n)?;
        lzma2_run(&input[span.clone()], &mut out, *n, false)?;
        Ok::<_, String>(out)
    });
    let mut out = room(total)?;
    for p in parts {
        out.extend_from_slice(&p?);
    }
    Ok((out, used))
}

/// [`lzma2_into`], of a stream to its end byte where `whole`, else (a segment) to the end of
/// `input`.
fn lzma2_run(input: &[u8], out: &mut Vec<u8>, limit: usize, whole: bool) -> Result<usize, String> {
    let corrupt = || "LZMA2: damaged data".to_string();
    let (mut at, start) = (0, out.len());
    // where the dictionary was last reset
    let mut base = None;
    let mut model: Option<Model> = None;
    let mut props = None;
    loop {
        if !whole && at == input.len() {
            return Ok(at);
        }
        let &first = input.get(at).ok_or_else(corrupt)?;
        if first == 0 {
            return Ok(at + 1);
        }
        let lzma = first >= 0x80;
        let control = if lzma { first & 0xe0 } else { first };
        if !lzma && control > 2 {
            return Err(corrupt());
        }
        let head = if !lzma {
            3
        } else if control >= 0xc0 {
            6
        } else {
            5
        };
        let h = input.get(at..at + head).ok_or_else(corrupt)?;
        let len = (if lzma { (first as usize & 0x1f) << 16 } else { 0 } | (h[1] as usize) << 8 | h[2] as usize) + 1;
        if control == 1 || control == 0xe0 {
            base = Some(out.len());
            (model, props) = (None, None);
        }
        let b = base.ok_or_else(corrupt)?;
        if out.len() - start + len > limit {
            return Err("LZMA2: more data than can be held".into());
        }
        if !lzma {
            out.extend_from_slice(input.get(at + 3..at + 3 + len).ok_or_else(corrupt)?);
            at += 3 + len;
            continue;
        }
        let packed = ((h[3] as usize) << 8 | h[4] as usize) + 1;
        if control >= 0xc0 {
            props = Some(lzma_props(h[5]).ok().filter(|p| p.lc + p.lp <= 4).ok_or_else(corrupt)?);
        }
        if control >= 0xa0 {
            model = Some(Model::new(props.ok_or_else(corrupt)?));
        }
        let m = model.as_mut().ok_or_else(corrupt)?;
        let coded = input.get(at + head..at + head + packed).ok_or_else(corrupt)?;
        let mut c = Decoder::new(coded).ok_or_else(corrupt)?;
        let end = out.len() + len;
        if lzma_into(m, &mut c, out, b, Some(end), usize::MAX)? || !c.finished() || c.overrun > 0 || c.pos != packed {
            return Err(corrupt());
        }
        at += head + packed;
    }
}

/// The data of the bzip2 streams at the start of `d`, one after another, at most `limit` bytes.
pub fn bzip2(d: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < d.len() && aqr_bz2::stream::level(&d[at..]).is_some() {
        let s = aqr_bz2::stream::decode(&d[at..], true, limit - out.len()).map_err(|e| format!("bzip2: {e}"))?;
        if out.is_empty() {
            out = s.data;
        } else {
            out.extend_from_slice(&s.data);
        }
        at += s.len;
    }
    if at == 0 {
        return Err("bzip2: not a stream".into());
    }
    Ok(out)
}

/// The data of the Zstandard frames of `d`, given in chunks to `give` (false: stop).
pub fn zstd_chunks(d: &[u8], give: &mut dyn FnMut(Vec<u8>) -> bool) -> Result<(), String> {
    use std::io::Read;
    let mut z = zstd::stream::read::Decoder::with_buffer(d).map_err(|e| format!("Zstandard: {e}"))?;
    z.window_log_max(31).map_err(|e| format!("Zstandard: {e}"))?;
    loop {
        let mut chunk = vec![0u8; CHUNK];
        let mut n = 0;
        while n < chunk.len() {
            match z.read(&mut chunk[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("Zstandard: {e}")),
            }
        }
        chunk.truncate(n);
        if n == 0 || !give(chunk) {
            return Ok(());
        }
    }
}

/// The data of the Zstandard frames of `d`, of `size` bytes as far as is known, at most `limit`.
pub fn zstd(d: &[u8], size: usize, limit: usize) -> Result<Vec<u8>, String> {
    let mut out = room(size.min(limit))?;
    let mut over = false;
    zstd_chunks(d, &mut |c| {
        over = out.len() + c.len() > limit;
        out.extend_from_slice(&c);
        !over
    })?;
    if over {
        return Err("Zstandard: more data than can be held".into());
    }
    Ok(out)
}

/// CRC-64 (ECMA-182, reflected; of `.xz`), 8 bytes at a time.
pub struct Crc64(u64);

fn crc64_tables() -> &'static [[u64; 256]; 8] {
    static T: std::sync::OnceLock<Box<[[u64; 256]; 8]>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = Box::new([[0u64; 256]; 8]);
        for b in 0..256 {
            let mut c = b as u64;
            for _ in 0..8 {
                c = (c >> 1) ^ (0xc96c_5795_d787_0f42 & (c & 1).wrapping_neg());
            }
            t[0][b] = c;
        }
        for k in 1..8 {
            for b in 0..256 {
                let c = t[k - 1][b];
                t[k][b] = (c >> 8) ^ t[0][(c & 0xff) as usize];
            }
        }
        t
    })
}

impl Crc64 {
    pub fn new() -> Crc64 {
        Crc64(!0)
    }

    pub fn update(&mut self, d: &[u8]) {
        let t = crc64_tables();
        let mut c = self.0;
        let (chunks, rest) = d.as_chunks::<8>();
        for w in chunks {
            let v = c ^ u64::from_le_bytes(*w);
            c = t[7][(v & 0xff) as usize]
                ^ t[6][(v >> 8 & 0xff) as usize]
                ^ t[5][(v >> 16 & 0xff) as usize]
                ^ t[4][(v >> 24 & 0xff) as usize]
                ^ t[3][(v >> 32 & 0xff) as usize]
                ^ t[2][(v >> 40 & 0xff) as usize]
                ^ t[1][(v >> 48 & 0xff) as usize]
                ^ t[0][(v >> 56) as usize];
        }
        for &b in rest {
            c = (c >> 8) ^ t[0][((c ^ b as u64) & 0xff) as usize];
        }
        self.0 = c;
    }

    pub fn finish(&self) -> u64 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzma2_split() {
        // two segments: an uncompressed chunk resetting the dictionary, then another; the end
        let mut s = vec![1, 0, 2, b'a', b'b', b'c', 1, 0, 1, b'x', b'y', 0];
        let (segs, used) = lzma2_segments(&s).unwrap();
        assert_eq!(segs, vec![(0..6, 3), (6..11, 2)]);
        assert_eq!(used, 12);
        assert_eq!(lzma2_par(&s, 100, 4).unwrap(), (b"abcxy".to_vec(), 12));
        // (a chunk not resetting the dictionary goes on the segment before it)
        s.splice(11..11, [2, 0, 0, b'z']);
        assert_eq!(lzma2_segments(&s).unwrap().0, vec![(0..6, 3), (6..15, 3)]);
        assert_eq!(lzma2_par(&s, 100, 4).unwrap().0, b"abcxyz");
        assert!(lzma2_par(&s, 5, 4).is_err());
    }

    #[test]
    fn crc64() {
        let mut c = Crc64::new();
        c.update(b"123456789");
        assert_eq!(c.finish(), 0x995d_c9bb_df19_39fa);
        let mut c = Crc64::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0x995d_c9bb_df19_39fa);
    }

    #[test]
    fn inflate_in_chunks() {
        // stored, of more than a chunk
        let data: Vec<u8> = (0..(CHUNK * 2 + 12345)).map(|i| (i * 7 / 3) as u8).collect();
        let mut raw = Vec::new();
        for (k, block) in data.chunks(65535).enumerate() {
            let last = (k + 1) * 65535 >= data.len();
            raw.push(last as u8);
            raw.extend((block.len() as u16).to_le_bytes());
            raw.extend((!(block.len() as u16)).to_le_bytes());
            raw.extend_from_slice(block);
        }
        let mut got = Vec::new();
        let used = inflate_chunks(&raw, &mut |v, s| {
            got.extend_from_slice(&v[s..]);
            true
        })
        .unwrap();
        assert_eq!(used, Some(raw.len()));
        assert!(got == data);
        assert!(inflate(&raw, 0, usize::MAX).unwrap().0 == data);
        // stopped
        let mut n = 0;
        assert_eq!(
            inflate_chunks(&raw, &mut |_, _| {
                n += 1;
                false
            })
            .unwrap(),
            None
        );
        assert_eq!(n, 1);
    }
}
