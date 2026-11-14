//! Lossless JPEG (SOF3, ITU T.81 annex H): parsing, decoding to samples and bit-exact
//! re-emission of the entropy-coded data from them.
//!
//! What is supported is what cameras write (Canon's CR2, Adobe DNG, Nikon, ...): one scan of
//! all the components, interleaved; predictors 1 to 7; any point transform; restart intervals
//! of whole rows of MCUs. The samples of a component are kept in a plane of the size of its
//! MCUs (`mcux * h` by `mcuy * v`), padding included, as the encoder coded them.
//!
//! The entropy-coded data is reproduced exactly where an encoder has no choices left beyond the
//! samples: the Huffman code of each difference, padding of 1 bits at the end of an interval,
//! stuffed bytes. [`encode`] gives the bytes the decoder read for it (the caller compares).

use crate::jpeg::Error;
use crate::jpeg::{HuffSpec, err, scan_end};

/// A component of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Comp {
    pub id: u8,
    pub h: usize,
    pub v: usize,
}

/// Up to four values held in place, so that a header takes no blocks of its own: a list of
/// them is one block of headers, which a module's allocator gives back at once.
#[derive(Clone, Copy)]
pub struct Small<T: Copy> {
    n: u8,
    a: [T; 4],
}

impl<T: Copy> Small<T> {
    /// The values `v` (1 to 4 of them: a frame of more is refused before this).
    pub fn of(v: &[T]) -> Small<T> {
        let mut a = [v[0]; 4];
        a[..v.len()].copy_from_slice(v);
        Small { n: v.len() as u8, a }
    }
}

impl<T: Copy> std::ops::Deref for Small<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.a[..self.n as usize]
    }
}

/// The headers of a lossless JPEG and where its entropy-coded data lies.
#[derive(Clone)]
pub struct Header {
    /// bits of a sample (2..=16)
    pub precision: u32,
    pub height: usize,
    pub width: usize,
    pub comps: Small<Comp>,
    /// the components of the scan in its order: (index in `comps`, Huffman table 0..4)
    pub scan: Small<(usize, usize)>,
    /// 1..=7
    pub predictor: u32,
    /// the point transform, 0..precision
    pub point: u32,
    /// restart interval in MCUs (0: none)
    pub restart: usize,
    /// Which code an encoder writes for a symbol that the table lists twice: the last one, not the first.
    pub last_code: bool,
    pub tables: [Option<HuffSpec>; 4],
    /// the entropy-coded data `start..end` of the buffer parsed (RST markers inside)
    pub start: usize,
    pub end: usize,
}

impl Header {
    /// MCUs across and down.
    pub fn mcus(&self) -> (usize, usize) {
        let hmax = self.comps.iter().map(|c| c.h).max().unwrap_or(1);
        let vmax = self.comps.iter().map(|c| c.v).max().unwrap_or(1);
        (self.width.div_ceil(hmax), self.height.div_ceil(vmax))
    }

    /// Width and height of the plane of component `i`.
    pub fn plane(&self, i: usize) -> (usize, usize) {
        let (mx, my) = self.mcus();
        (mx * self.comps[i].h, my * self.comps[i].v)
    }

    /// Samples of plane `i`.
    pub fn samples_of(&self, i: usize) -> usize {
        let (w, h) = self.plane(i);
        w * h
    }

    /// Samples of all planes.
    pub fn samples(&self) -> usize {
        (0..self.comps.len()).map(|i| self.plane(i)).map(|(w, h)| w * h).sum()
    }
}

fn be16(d: &[u8], i: usize) -> Result<usize, Error> {
    match d.get(i..i + 2) {
        Some(b) => Ok(((b[0] as usize) << 8) | b[1] as usize),
        None => err("truncated"),
    }
}

/// Whether a Huffman table of `counts` is a prefix code (and not overfull).
fn prefix_code(counts: &[u8; 16]) -> bool {
    let mut code = 0u32;
    for (l, &n) in counts.iter().enumerate() {
        code += n as u32;
        if code > 1 << (l + 1) {
            return false;
        }
        code <<= 1;
    }
    true
}

/// Parses the headers of the lossless JPEG with its SOI at `at`. Err where it is not one (or
/// not of the kind supported here).
pub fn parse(d: &[u8], at: usize) -> Result<Header, Error> {
    if d.get(at..at + 2) != Some(&[0xFF, 0xD8]) {
        return err("no SOI");
    }
    let mut p = at + 2;
    let mut frame: Option<(u32, usize, usize, Vec<Comp>)> = None;
    let mut tables: [Option<HuffSpec>; 4] = [None; 4];
    let mut restart = 0;
    loop {
        if d.get(p) != Some(&0xFF) {
            return err("marker expected");
        }
        while d.get(p + 1) == Some(&0xFF) {
            p += 1;
        }
        let m = *d.get(p + 1).ok_or(Error("truncated".into()))?;
        p += 2;
        match m {
            0x01 => continue,
            0xD8 | 0xD0..=0xD7 | 0xD9 | 0x00 => return err("unexpected marker"),
            _ => {}
        }
        let len = be16(d, p)?;
        let seg = d.get(p + 2..p + len.max(2)).ok_or(Error("truncated".into()))?;
        p += len;
        match m {
            0xC4 => {
                let mut s = seg;
                while !s.is_empty() {
                    let (tc_th, rest) = s.split_first().unwrap();
                    let (tc, th) = (tc_th >> 4, (tc_th & 15) as usize);
                    if tc != 0 || th >= 4 {
                        return err("not a DC table");
                    }
                    let counts: [u8; 16] = rest.get(..16).ok_or(Error("truncated".into()))?.try_into().unwrap();
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = rest.get(16..16 + n).ok_or(Error("truncated".into()))?;
                    let vals =
                        crate::jpeg::Vals::of(vals).filter(|v| prefix_code(&counts) && v.iter().all(|&v| v <= 16));
                    tables[th] = Some(HuffSpec { counts, vals: vals.ok_or(Error("bad table".into()))? });
                    s = &rest[16 + n..];
                }
            }
            0xC3 => {
                if frame.is_some() {
                    return err("second frame");
                }
                if seg.len() < 6 {
                    return err("truncated SOF");
                }
                let (precision, height, width, nf) = (seg[0] as u32, be16(seg, 1)?, be16(seg, 3)?, seg[5] as usize);
                if !(2..=16).contains(&precision) || height == 0 || width == 0 || !(1..=4).contains(&nf) {
                    return err("unsupported frame");
                }
                if seg.len() != 6 + 3 * nf {
                    return err("bad SOF length");
                }
                let mut comps = Vec::with_capacity(nf);
                for c in seg[6..].chunks(3) {
                    let (h, v) = ((c[1] >> 4) as usize, (c[1] & 15) as usize);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                        return err("bad sampling factors");
                    }
                    if comps.iter().any(|x: &Comp| x.id == c[0]) {
                        return err("repeated component");
                    }
                    comps.push(Comp { id: c[0], h, v });
                }
                frame = Some((precision, height, width, comps));
            }
            0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => return err("not lossless"),
            0xDD => {
                if seg.len() != 2 {
                    return err("bad DRI");
                }
                restart = be16(seg, 0)?;
            }
            0xDA => {
                let Some((precision, height, width, comps)) = frame else { return err("SOS before SOF") };
                let ns = *seg.first().ok_or(Error("truncated".into()))? as usize;
                if ns != comps.len() || seg.len() != 1 + 2 * ns + 3 {
                    return err("scan is not of all the components");
                }
                let mut scan = Vec::with_capacity(ns);
                for c in seg[1..1 + 2 * ns].chunks(2) {
                    let ci = comps.iter().position(|x| x.id == c[0]).ok_or(Error("unknown component".into()))?;
                    if scan.iter().any(|&(i, _)| i == ci) {
                        return err("repeated component in scan");
                    }
                    let td = (c[1] >> 4) as usize;
                    if td >= 4 || tables[td].is_none() {
                        return err("missing table");
                    }
                    scan.push((ci, td));
                }
                let t = &seg[1 + 2 * ns..];
                let (predictor, se, ah, al) = (t[0] as u32, t[1], t[2] >> 4, (t[2] & 15) as u32);
                if !(1..=7).contains(&predictor) || se != 0 || ah != 0 || al >= precision {
                    return err("unsupported scan parameters");
                }
                let start = p;
                let end = scan_end(d, start);
                if d.get(end..end + 2) != Some(&[0xFF, 0xD9]) {
                    return err("no EOI after the scan");
                }
                let h = Header {
                    precision,
                    height,
                    width,
                    comps: Small::of(&comps),
                    scan: Small::of(&scan),
                    predictor,
                    point: al,
                    restart,
                    last_code: false,
                    tables,
                    start,
                    end,
                };
                let (mx, _) = h.mcus();
                if restart != 0 && restart % mx != 0 {
                    return err("restart intervals are not of whole rows");
                }
                return Ok(h);
            }
            // APPn, COM and the like
            _ => {}
        }
    }
}

/// The samples of a frame: a plane for each component, `plane(i)` samples across.
pub struct Samples {
    pub planes: Vec<Vec<u16>>,
}

/// A decoding table: the codes of up to 16 bits, as a lookup of the next `FAST` bits and the
/// canonical decoding for longer codes.
struct Decoder {
    fast: Vec<u16>,
    /// `maxcode[l]`: the largest code of length l + 1 (or -1), `valptr[l]` the index of its first symbol minus
    /// its first code
    maxcode: [i32; 17],
    valptr: [i32; 17],
    vals: Vec<u8>,
}

const FAST: u32 = 10;

impl Decoder {
    fn new(t: &HuffSpec) -> Decoder {
        let mut fast = vec![0u16; 1 << FAST];
        let mut maxcode = [-1i32; 17];
        let mut valptr = [0i32; 17];
        let (mut code, mut k) = (0u32, 0usize);
        for l in 1..=16usize {
            let n = t.counts[l - 1] as usize;
            valptr[l] = k as i32 - code as i32;
            for i in 0..n {
                if l as u32 <= FAST {
                    let first = (code + i as u32) << (FAST - l as u32);
                    for e in &mut fast[first as usize..first as usize + (1 << (FAST - l as u32))] {
                        *e = ((l as u16) << 8) | t.vals[k + i] as u16;
                    }
                }
            }
            code += n as u32;
            k += n;
            maxcode[l] = if n > 0 { code as i32 - 1 } else { -1 };
            code <<= 1;
        }
        Decoder { fast, maxcode, valptr, vals: t.vals.to_vec() }
    }
}

/// An MSB-first bit reader of a destuffed interval; reads past its end give zeros (and are
/// noticed by `over`).
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Bits { d, pos: 0, acc: 0, n: 0 }
    }

    #[inline]
    fn fill(&mut self) {
        while self.n <= 56 {
            let b = self.d.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.acc |= (b as u64) << (56 - self.n);
            self.n += 8;
        }
    }

    #[inline]
    fn peek(&self, k: u32) -> u32 {
        (self.acc >> (64 - k)) as u32
    }

    #[inline]
    fn skip(&mut self, k: u32) {
        self.acc <<= k;
        self.n -= k;
    }

    /// Bits consumed so far.
    fn used(&self) -> usize {
        self.pos * 8 - self.n as usize
    }

    #[inline]
    fn symbol(&mut self, t: &Decoder) -> Result<u32, Error> {
        self.fill();
        let e = t.fast[self.peek(FAST) as usize];
        if e != 0 {
            self.skip((e >> 8) as u32);
            return Ok((e & 255) as u32);
        }
        // (a code of length 0 is no entry: lengths are 1 or more)
        for l in (FAST + 1)..=16 {
            let c = self.peek(l) as i32;
            if c <= t.maxcode[l as usize] {
                self.skip(l);
                let i = (c + t.valptr[l as usize]) as usize;
                return t.vals.get(i).map(|&v| v as u32).ok_or(Error("bad code".into()));
            }
        }
        err("bad code")
    }

    /// The `k` (<= 16) bits as a number.
    #[inline]
    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        self.fill();
        let v = self.peek(k);
        self.skip(k);
        v
    }
}

/// The difference of the category `s` and its `s` bits `v`.
#[inline]
fn extend(v: u32, s: u32) -> i32 {
    if s == 0 {
        0
    } else if s == 16 {
        32768
    } else if v < 1 << (s - 1) {
        v as i32 - (1 << s) + 1
    } else {
        v as i32
    }
}

/// The prediction of the sample at (`x`, `y`) of a plane of width `w`, its rows `first` (the first
/// of an interval) or later.
#[inline]
fn predict(p: &[u16], w: usize, x: usize, y: usize, first_row: bool, predictor: u32, default: i32) -> i32 {
    let prev = if y > 0 { &p[(y - 1) * w..y * w] } else { &[][..] };
    predict_row(&p[y * w..(y + 1) * w], prev, x, first_row, predictor, default)
}

/// [`predict`] of the sample `x` of a line `cur`, the line above it `prev` (not looked at in the
/// first line of an interval).
#[inline]
fn predict_row(cur: &[u16], prev: &[u16], x: usize, first_row: bool, predictor: u32, default: i32) -> i32 {
    if first_row {
        // the first line of an interval: the sample to the left, the default at its start
        return if x == 0 { default } else { cur[x - 1] as i32 };
    }
    if x == 0 {
        return prev[0] as i32;
    }
    let (a, b, c) = (cur[x - 1] as i32, prev[x] as i32, prev[x - 1] as i32);
    match predictor {
        1 => a,
        2 => b,
        3 => c,
        4 => a + b - c,
        5 => a + ((b - c) >> 1),
        6 => b + ((a - c) >> 1),
        _ => (a + b) >> 1,
    }
}

/// The destuffed intervals of the data `d[start..end]`: (bytes, whether a marker closed it).
/// Err where the RST markers are not in order, or a 0xFF is followed by what cannot be.
fn intervals(d: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut out = vec![Vec::with_capacity(d.len())];
    let mut i = 0;
    let mut rst = 0u8;
    while i < d.len() {
        match memchr::memchr(0xFF, &d[i..]) {
            None => {
                out.last_mut().unwrap().extend_from_slice(&d[i..]);
                break;
            }
            Some(o) => {
                out.last_mut().unwrap().extend_from_slice(&d[i..i + o]);
                match d.get(i + o + 1) {
                    Some(0) => {
                        out.last_mut().unwrap().push(0xFF);
                    }
                    Some(&m) if m == 0xD0 + rst => {
                        rst = (rst + 1) & 7;
                        out.push(Vec::new());
                    }
                    _ => return err("unexpected marker in the data"),
                }
                i += o + 2;
            }
        }
    }
    Ok(out)
}

/// Decodes the entropy-coded data of `h` (the bytes of the file from `h.start` to `h.end`).
/// Err where the data is not what [`encode`] gives from the samples (the padding, the order
/// of restart markers) or is cut off.
pub fn decode(h: &Header, data: &[u8]) -> Result<Samples, Error> {
    let (mx, my) = h.mcus();
    let ivs = intervals(data)?;
    let rows_per = if h.restart == 0 { my } else { h.restart / mx };
    if ivs.len() != my.div_ceil(rows_per) {
        return err("the restart intervals do not fit the frame");
    }
    // every sample takes a code of at least one bit
    let samples: usize = (0..h.comps.len()).map(|i| h.plane(i)).map(|(w, hh)| w * hh).sum();
    if samples > data.len().saturating_mul(8) {
        return err("data cut off");
    }
    let decoders: Vec<Option<Decoder>> = h.tables.iter().map(|t| t.as_ref().map(Decoder::new)).collect();
    let mut planes: Vec<Vec<u16>> = (0..h.comps.len()).map(|i| h.plane(i)).map(|(w, hh)| vec![0u16; w * hh]).collect();
    let default = 1i32 << (h.precision - h.point - 1);
    for (k, iv) in ivs.iter().enumerate() {
        let (row0, row1) = (k * rows_per, ((k + 1) * rows_per).min(my));
        let mut b = Bits::new(iv);
        for r in row0..row1 {
            for c in 0..mx {
                for &(ci, td) in h.scan.iter() {
                    let comp = h.comps[ci];
                    let w = mx * comp.h;
                    let t = decoders[td].as_ref().unwrap();
                    for v in 0..comp.v {
                        let y = r * comp.v + v;
                        let first = y == row0 * comp.v;
                        for hh in 0..comp.h {
                            let x = c * comp.h + hh;
                            let s = b.symbol(t)?;
                            let bits = if s == 16 { 0 } else { b.bits(s) };
                            let diff = extend(bits, s);
                            let pred = predict(&planes[ci], w, x, y, first, h.predictor, default);
                            planes[ci][y * w + x] = (pred + diff) as u16;
                        }
                    }
                }
            }
        }
        // the padding: 1 bits to the end of the byte, and no more bytes
        let used = b.used();
        if used > iv.len() * 8 {
            return err("data cut off");
        }
        let end = used.div_ceil(8);
        if end != iv.len() {
            return err("bytes after the interval's data");
        }
        let pad = end * 8 - used;
        if pad > 0 {
            let last = iv[end - 1];
            if last & ((1 << pad) - 1) != (1 << pad) - 1 {
                return err("padding is not of 1 bits");
            }
        }
    }
    Ok(Samples { planes })
}

/// An encoding table: the code and its length of each category.
struct Encoder {
    code: [u16; 17],
    len: [u8; 17],
}

impl Encoder {
    fn new(t: &HuffSpec, last: bool) -> Encoder {
        let mut e = Encoder { code: [0; 17], len: [0; 17] };
        let (mut code, mut k) = (0u32, 0usize);
        for l in 1..=16u8 {
            for _ in 0..t.counts[l as usize - 1] {
                let s = t.vals[k] as usize;
                // (a symbol listed twice: the first code or the last)
                if last || e.len[s] == 0 {
                    e.code[s] = code as u16;
                    e.len[s] = l;
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        e
    }
}

struct Out<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

/// Where the bits of the codes go.
trait Sink {
    fn put(&mut self, v: u32, k: u32);
}

impl Sink for Out<'_> {
    #[inline]
    fn put(&mut self, v: u32, k: u32) {
        self.acc = (self.acc << k) | v as u64;
        self.n += k;
        while self.n >= 8 {
            let b = (self.acc >> (self.n - 8)) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
            self.n -= 8;
        }
    }
}

impl Out<'_> {
    /// Ends the byte with 1 bits.
    fn pad(&mut self) {
        if self.n > 0 {
            let k = 8 - self.n;
            self.put((1 << k) - 1, k);
        }
        self.acc = 0;
    }
}

/// The bits counted.
struct Count(u64);

impl Sink for Count {
    #[inline]
    fn put(&mut self, _: u32, k: u32) {
        self.0 += k as u64;
    }
}

/// The bits as they come, packed from the high end of each byte, not stuffed: the last byte is
/// padded with 0 bits ([`Raw::finish`]).
pub struct Raw<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
    /// bits put
    pub bits: u64,
}

impl<'a> Raw<'a> {
    pub fn new(out: &'a mut Vec<u8>) -> Raw<'a> {
        Raw { out, acc: 0, n: 0, bits: 0 }
    }

    /// Puts the bits left in a last byte.
    pub fn finish(&mut self) {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
            self.n = 0;
        }
    }
}

impl Sink for Raw<'_> {
    #[inline]
    fn put(&mut self, v: u32, k: u32) {
        self.acc = (self.acc << k) | v as u64;
        self.n += k;
        self.bits += k as u64;
        while self.n >= 8 {
            self.out.push((self.acc >> (self.n - 8)) as u8);
            self.n -= 8;
        }
    }
}

/// The entropy-coded data of `s` under `h`, as [`decode`] reads it. Err where a difference
/// has no code in the tables.
pub fn encode(h: &Header, s: &Samples) -> Result<Vec<u8>, Error> {
    let planes: Vec<&[u16]> = s.planes.iter().map(|p| &p[..]).collect();
    let mut out = Vec::new();
    encode_into(h, &planes, &mut out)?;
    Ok(out)
}

/// The rows of the planes of a frame, by the component and the line.
pub trait Rows {
    fn row(&self, ci: usize, y: usize) -> &[u16];
}

/// The whole planes of a frame as [`Rows`].
struct Full<'a>(&'a [&'a [u16]], &'a Header);

impl Rows for Full<'_> {
    fn row(&self, ci: usize, y: usize) -> &[u16] {
        let w = self.1.plane(ci).0;
        &self.0[ci][y * w..(y + 1) * w]
    }
}

/// The entropy-coded data of a frame made a row of MCUs at a time ([`Emitter::mcu_row`]), so that
/// what is held is the rows the prediction looks at, and the bits not yet a byte.
pub struct Emitter<'h> {
    h: &'h Header,
    encoders: Vec<Option<Encoder>>,
    /// bits not yet put in bytes
    acc: u64,
    n: u32,
    /// rows of MCUs to an interval
    rows_per: usize,
}

impl<'h> Emitter<'h> {
    pub fn new(h: &'h Header) -> Emitter<'h> {
        let (mx, my) = h.mcus();
        Emitter {
            h,
            encoders: h.tables.iter().map(|t| t.as_ref().map(|t| Encoder::new(t, h.last_code))).collect(),
            acc: 0,
            n: 0,
            rows_per: if h.restart == 0 { my } else { h.restart / mx },
        }
    }

    /// Bytes of the blocks an emitter makes (its tables), rounded up as the stack does.
    pub fn bytes() -> u64 {
        aqr_api::stack::block(4 * size_of::<Option<Encoder>>() as u64)
    }

    /// Bytes the data of a row of MCUs takes at most: each sample a code of 16 bits and the 16 bits
    /// after it, stuffed, and a restart marker and the bits left.
    pub fn row_room(h: &Header) -> usize {
        let samples: usize = (0..h.comps.len()).map(|i| h.plane(i).0 * h.comps[i].v).sum();
        samples * 8 + 16
    }

    /// Appends to `out` the data of the row `r` of MCUs (in order), the rows of the planes by
    /// `rows`: those of the MCU row and the one above.
    pub fn mcu_row(&mut self, rows: &impl Rows, r: usize, out: &mut Vec<u8>) -> Result<(), Error> {
        let my = self.h.mcus().1;
        if r.is_multiple_of(self.rows_per) && r > 0 {
            out.extend_from_slice(&[0xFF, 0xD0 + ((r / self.rows_per - 1) & 7) as u8]);
        }
        let mut o = Out { out, acc: self.acc, n: self.n };
        self.row_bits(rows, r, &mut o)?;
        if (r + 1).is_multiple_of(self.rows_per) || r + 1 == my {
            o.pad();
        }
        (self.acc, self.n) = (o.acc, o.n);
        Ok(())
    }

    /// Puts the bits of the codes of the row `r` of MCUs to `o`, whatever precedes them
    /// (`rows` as [`Emitter::mcu_row`]'s).
    fn row_bits(&self, rows: &impl Rows, r: usize, o: &mut impl Sink) -> Result<(), Error> {
        let h = self.h;
        let mx = h.mcus().0;
        let default = 1i32 << (h.precision - h.point - 1);
        let row0 = r / self.rows_per * self.rows_per;
        for c in 0..mx {
            for &(ci, td) in h.scan.iter() {
                let comp = h.comps[ci];
                let e = self.encoders[td].as_ref().unwrap();
                for v in 0..comp.v {
                    let y = r * comp.v + v;
                    let first = y == row0 * comp.v;
                    let cur = rows.row(ci, y);
                    let prev = if first { &[][..] } else { rows.row(ci, y - 1) };
                    for hh in 0..comp.h {
                        let x = c * comp.h + hh;
                        let pred = predict_row(cur, prev, x, first, h.predictor, default);
                        // the difference in -32768..32768
                        let d = ((cur[x] as i32 - pred + 32768) & 0xFFFF) - 32768;
                        let cat = 32 - d.unsigned_abs().leading_zeros();
                        if e.len[cat as usize] == 0 {
                            return err("difference without a code");
                        }
                        o.put(e.code[cat as usize] as u32, e.len[cat as usize] as u32);
                        if cat > 0 && cat < 16 {
                            let bits = if d > 0 { d } else { d + (1 << cat) - 1 };
                            o.put(bits as u32, cat);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The codes of the row `r` of MCUs as raw bits ([`Raw`]), for a frame without restart
    /// intervals, a row that is not its first.
    pub fn bits(&self, rows: &impl Rows, r: usize, raw: &mut Raw) -> Result<(), Error> {
        self.row_bits(rows, r, raw)
    }

    /// Appends to `out` the `bits` bits of `data` (packed from the high end, [`Raw`]) after what
    /// the rows put, `chunk` bytes of them at a time to `put`; `last` ends the byte with 1 bits
    /// (the end of the frame).
    pub fn append(
        &mut self,
        data: &[u8],
        bits: u64,
        last: bool,
        out: &mut Vec<u8>,
        chunk: usize,
        mut put: impl FnMut(&[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut o = Out { out, acc: self.acc, n: self.n };
        let (whole, tail) = ((bits / 8) as usize, (bits % 8) as u32);
        if data.len() != whole + (tail > 0) as usize {
            return err("bits do not fit their bytes");
        }
        for piece in data[..whole].chunks(chunk.max(1)) {
            o.out.clear();
            for &b in piece {
                o.put(b as u32, 8);
            }
            put(o.out)?;
        }
        o.out.clear();
        if tail > 0 {
            o.put((data[whole] >> (8 - tail)) as u32, tail);
        }
        if last {
            o.pad();
        }
        put(o.out)?;
        (self.acc, self.n) = (o.acc, o.n);
        Ok(())
    }
}

/// The bits of the rows `r0 + 1..r1` of MCUs of the frame of `h` in `planes` (a frame without
/// restart intervals), as the data of the rows after the first of a band.
pub fn band_bits(h: &Header, planes: &[&[u16]], r0: usize, r1: usize) -> Result<u64, Error> {
    let full = Full(planes, h);
    let emitter = Emitter::new(h);
    let mut count = Count(0);
    for r in r0 + 1..r1 {
        emitter.row_bits(&full, r, &mut count)?;
    }
    Ok(count.0)
}

/// [`encode`] of the `planes` of a frame, appended to `out`.
pub fn encode_into(h: &Header, planes: &[&[u16]], out: &mut Vec<u8>) -> Result<(), Error> {
    let full = Full(planes, h);
    let mut emitter = Emitter::new(h);
    for r in 0..h.mcus().1 {
        emitter.mcu_row(&full, r, out)?;
    }
    Ok(())
}

/// Whether `d` holds a lossless JPEG ([`find`] stopping at the first).
pub fn any(d: &[u8]) -> bool {
    let mut i = 0;
    while let Some(o) = memchr::memmem::find(&d[i..], &[0xFF, 0xD8, 0xFF]) {
        if parse(d, i + o).is_ok() {
            return true;
        }
        i += o + 2;
    }
    false
}

/// The lossless JPEGs in `d`: the headers of each, found by their SOI (a JPEG inside the data
/// of another is not looked for).
pub fn find(d: &[u8]) -> Vec<(usize, Header)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(o) = memchr::memmem::find(&d[i..], &[0xFF, 0xD8, 0xFF]) {
        let at = i + o;
        match parse(d, at) {
            Ok(h) => {
                i = h.end;
                out.push((at, h));
            }
            Err(_) => i = at + 2,
        }
    }
    out
}

/// A DC table with the categories 0..=16 as 5-bit codes (the all-ones code is left out).
fn table() -> Vec<u8> {
    let mut t = vec![0x00];
    let mut counts = [0u8; 16];
    counts[4] = 17;
    t.extend_from_slice(&counts);
    t.extend(0..=16u8);
    t
}

fn seg(out: &mut Vec<u8>, m: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, m]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

/// A lossless JPEG for tests, here and in other crates: the headers with a table of 17 codes of 5
/// bits, of `comps` (h, v) components of `precision` bits and `w` by `h` samples (a gradient with noise
/// from `seed`), predictor `pred`, point transform `pt` and restart interval `restart` MCUs (0: none).
#[allow(clippy::too_many_arguments)]
pub fn synthetic(
    precision: u8,
    w: u16,
    h: u16,
    comps: &[(u8, u8)],
    pred: u8,
    pt: u8,
    restart: u16,
    seed: u32,
) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    seg(&mut d, 0xC4, &table());
    let mut sof = vec![precision];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.push(comps.len() as u8);
    for (i, &(hh, vv)) in comps.iter().enumerate() {
        sof.extend_from_slice(&[i as u8 + 1, hh << 4 | vv, 0]);
    }
    seg(&mut d, 0xC3, &sof);
    if restart > 0 {
        seg(&mut d, 0xDD, &restart.to_be_bytes());
    }
    let mut sos = vec![comps.len() as u8];
    for i in 0..comps.len() {
        sos.extend_from_slice(&[i as u8 + 1, 0]);
    }
    sos.extend_from_slice(&[pred, 0, pt]);
    seg(&mut d, 0xDA, &sos);
    let start = d.len();
    d.extend_from_slice(&[0xFF, 0xD9]);
    // the samples: a gradient with noise, coded by the encoder itself
    let hd = parse(&d, 0).unwrap();
    let mut x = seed;
    let planes = (0..comps.len())
        .map(|i| {
            let (pw, ph) = hd.plane(i);
            (0..pw * ph)
                .map(|k| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    let mask = (1u32 << (precision - pt)) - 1;
                    (((k % pw) as u32 * 7 + (k / pw) as u32 * 3 + (x >> 24) % 9) & mask) as u16
                })
                .collect()
        })
        .collect();
    let data = encode(&hd, &Samples { planes }).unwrap();
    d.splice(start..start, data);
    d
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn roundtrip(d: &[u8]) {
        let h = parse(d, 0).unwrap();
        let s = decode(&h, &d[h.start..h.end]).unwrap();
        assert_eq!(encode(&h, &s).unwrap(), &d[h.start..h.end]);
    }

    #[test]
    fn predictors_and_shapes() {
        for pred in 1..=7 {
            roundtrip(&synthetic(14, 37, 19, &[(1, 1), (1, 1)], pred, 0, 0, pred as u32));
        }
        roundtrip(&synthetic(16, 20, 11, &[(1, 1)], 1, 0, 0, 1));
        roundtrip(&synthetic(12, 33, 17, &[(2, 1), (1, 1), (1, 1)], 4, 0, 0, 2));
        roundtrip(&synthetic(12, 33, 17, &[(2, 2), (1, 1), (1, 1)], 6, 2, 0, 3));
        roundtrip(&synthetic(10, 16, 9, &[(1, 1), (1, 1), (1, 1), (1, 1)], 7, 0, 0, 4));
    }

    /// A prefix code of 510 symbols is an error, not a panic.
    #[test]
    fn table_of_too_many_symbols() {
        let mut d = vec![0xFF, 0xD8];
        let mut t = vec![0x00];
        let mut counts = [0u8; 16];
        counts[8] = 255;
        counts[9] = 255;
        t.extend_from_slice(&counts);
        t.extend(std::iter::repeat_n(0u8, 510));
        seg(&mut d, 0xC4, &t);
        seg(&mut d, 0xC3, &[8, 0, 4, 0, 6, 1, 1, 0x11, 0]);
        seg(&mut d, 0xDA, &[1, 1, 0, 1, 0, 0]);
        d.extend_from_slice(&[0xFF, 0xD9]);
        assert!(parse(&d, 0).is_err());
    }

    /// A big frame with no data is cut off, without a plane of its size made first.
    #[test]
    fn big_frame_without_data() {
        let d = synthetic(8, 16, 16, &[(1, 1)], 1, 0, 0, 1);
        let mut h = parse(&d, 0).unwrap();
        (h.height, h.width) = (60000, 60000);
        assert!(decode(&h, &[]).is_err());
    }

    /// A table listing the symbol 0 twice (5 and 7 bits), as in an Adobe DNG: its encoder writes
    /// the later code for every 0.
    #[test]
    fn symbols_listed_twice() {
        let mut d = vec![0xFF, 0xD8];
        let mut t = vec![0x00];
        let mut counts = [0u8; 16];
        counts[4] = 16;
        counts[6] = 1;
        t.extend_from_slice(&counts);
        t.extend((0..16u8).chain([0]));
        seg(&mut d, 0xC4, &t);
        seg(&mut d, 0xC3, &[8, 0, 4, 0, 6, 1, 1, 0x11, 0]);
        seg(&mut d, 0xDA, &[1, 1, 0, 1, 0, 0]);
        d.extend_from_slice(&[0xFF, 0xD9]);
        let mut h = parse(&d, 0).unwrap();
        let planes = vec![(0..24u16).map(|k| (k / 3) % 4).collect::<Vec<_>>()];
        let s = Samples { planes };
        // a constant row has zero differences: the first code and the last differ
        let first = encode(&h, &s).unwrap();
        h.last_code = true;
        let last = encode(&h, &s).unwrap();
        assert_ne!(first, last);
        for data in [first, last] {
            let mut g = h.clone();
            let back = decode(&g, &data).unwrap();
            assert_eq!(back.planes, s.planes);
            g.last_code = data == encode(&h, &s).unwrap();
            assert_eq!(encode(&g, &back).unwrap(), data);
        }
    }

    #[test]
    fn restarts() {
        roundtrip(&synthetic(14, 16, 12, &[(1, 1), (1, 1)], 1, 0, 32, 5));
        roundtrip(&synthetic(14, 16, 12, &[(1, 1), (1, 1)], 6, 0, 16, 6));
    }

    #[test]
    fn found_in_other_data() {
        let j = synthetic(14, 16, 12, &[(1, 1), (1, 1)], 1, 0, 0, 7);
        let mut d = b"II*\0 some container ".to_vec();
        d.extend_from_slice(&j);
        d.extend_from_slice(b"and more");
        let f = find(&d);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, 20);
    }

    #[test]
    fn damaged_data_is_refused() {
        let d = synthetic(14, 16, 12, &[(1, 1), (1, 1)], 1, 0, 0, 8);
        let h = parse(&d, 0).unwrap();
        let mut cut = d[h.start..h.end - 3].to_vec();
        assert!(decode(&h, &cut).is_err());
        // a 0 among the padding bits (the last byte of the data has some, or none)
        cut = d[h.start..h.end].to_vec();
        cut.push(0);
        assert!(decode(&h, &cut).is_err());
    }
}
