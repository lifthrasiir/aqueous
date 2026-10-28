//! MSB-first bit I/O (the bit order of MP3 and FLAC).

/// Bytes of zero padding that [`BitReader`] needs after the data.
pub const PAD: usize = 8;

/// Random-access MSB-first reader. The slice must be followed by [`PAD`] readable bytes
/// (see [`padded`]); bits past `limit` read as whatever is there.
#[derive(Clone, Copy)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pub pos: usize,
}

/// Copies `d` into a buffer with [`PAD`] trailing zero bytes.
pub fn padded(d: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(d.len() + PAD);
    v.extend_from_slice(d);
    v.resize(d.len() + PAD, 0);
    v
}

impl<'a> BitReader<'a> {
    /// `data` includes the padding.
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        debug_assert!(data.len() >= PAD);
        BitReader { data, pos }
    }

    /// The underlying (padded) data.
    #[inline(always)]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// The next 57 or more bits, left aligned.
    #[inline(always)]
    pub fn window(&self) -> u64 {
        // Past the end, the last eight bytes are read again (garbage, but no panic).
        let b = (self.pos >> 3).min(self.data.len() - 8);
        // SAFETY: b + 8 <= data.len().
        let w = u64::from_be(unsafe { (self.data.as_ptr().add(b) as *const u64).read_unaligned() });
        w << (self.pos & 7)
    }

    /// [`BitReader::window`] without the clamp.
    ///
    /// # Safety
    /// `pos / 8 + 8 <= data.len()`.
    #[inline(always)]
    pub unsafe fn window_unchecked(&self) -> u64 {
        debug_assert!(self.pos / 8 + 8 <= self.data.len());
        // SAFETY: by the caller.
        let w = u64::from_be(unsafe { (self.data.as_ptr().add(self.pos >> 3) as *const u64).read_unaligned() });
        w << (self.pos & 7)
    }

    /// Bits that can still be read with [`BitReader::window_unchecked`] after skipping
    /// up to this many.
    #[inline(always)]
    pub fn room(&self) -> usize {
        (self.data.len() - 8) * 8 - self.pos.min((self.data.len() - 8) * 8)
    }

    /// The next `n` bits (1..=32).
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        (self.window() >> (64 - n)) as u32
    }

    #[inline(always)]
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    /// Reads `n` bits (0..=32).
    #[inline(always)]
    pub fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }
}

/// MSB-first writer.
#[derive(Default)]
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        BitWriter { out: Vec::with_capacity(bytes), acc: 0, n: 0 }
    }

    /// Writes the low `n` bits of `v` (0..=32).
    #[inline(always)]
    pub fn put(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32 && (n == 32 || v >> n == 0));
        self.acc = (self.acc << n) | v as u64;
        self.n += n;
        if self.n >= 32 {
            self.n -= 32;
            self.out.extend_from_slice(&((self.acc >> self.n) as u32).to_be_bytes());
        }
    }

    /// Number of bits written so far.
    pub fn len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A position to return to with [`BitWriter::rollback`].
    pub fn mark(&self) -> (usize, u64, u32) {
        (self.out.len(), self.acc, self.n)
    }

    pub fn rollback(&mut self, m: (usize, u64, u32)) {
        self.out.truncate(m.0);
        self.acc = m.1;
        self.n = m.2;
    }

    /// Copies `n` bits from `r`.
    pub fn copy(&mut self, r: &mut BitReader, mut n: usize) {
        while n >= 32 {
            self.put(r.get(32), 32);
            n -= 32;
        }
        self.put(r.get(n as u32), n as u32);
    }

    /// Pads with zero bits to a byte boundary and returns the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let pad = (8 - self.n % 8) % 8;
        self.put(0, pad);
        let mut k = self.n;
        while k > 0 {
            k -= 8;
            self.out.push((self.acc >> k) as u8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_writer() {
        let mut a = BitWriter::new();
        let mut b = FastWriter::with_capacity(4);
        let mut seed = 99u64;
        for _ in 0..10000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (seed >> 58) as u32 % 33;
            let v = if n == 0 { 0 } else { (seed >> 7) as u32 & (u32::MAX >> (32 - n)) };
            a.put(v, n);
            b.reserve(8);
            b.put(v as u64, n);
        }
        b.reserve(8);
        b.put(0x1f_ffff_ffff_ffff, 53);
        a.put(0x1f_ffff, 21);
        a.put(0xffff_ffff, 32);
        assert_eq!(a.len(), b.len());
        assert_eq!(a.finish(), b.finish());
    }

    /// Bytes taken and dropped as they are completed, a window kept: the same bytes.
    #[test]
    fn fast_writer_window() {
        let mut a = BitWriter::new();
        let mut b = FastWriter::with_capacity(4);
        let mut got = Vec::new();
        let mut seed = 3u64;
        for i in 0..10000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (seed >> 58) as u32 % 33;
            let v = if n == 0 { 0 } else { (seed >> 7) as u32 & (u32::MAX >> (32 - n)) };
            a.put(v, n);
            b.reserve(8);
            b.put(v as u64, n);
            if i % 37 == 0 {
                let (base, done) = b.done();
                assert_eq!(base, got.len());
                let keep = done.len() / 3;
                got.extend_from_slice(&done[..done.len() - keep]);
                b.discard(got.len());
            }
        }
        assert_eq!(a.len(), b.len());
        got.extend_from_slice(&b.finish());
        assert_eq!(a.finish(), got);
    }

    #[test]
    fn word_writer() {
        let mut a = BitWriter::new();
        let mut b = WordWriter::with_capacity(4);
        let mut seed = 77u64;
        for _ in 0..10000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (seed >> 58) as u32 % 33;
            let v = if n == 0 { 0 } else { (seed >> 7) as u32 & (u32::MAX >> (32 - n)) };
            a.put(v, n);
            b.reserve(8);
            let mut l = b.local();
            l.put(v as u64, n);
            b.put_back(l);
        }
        assert_eq!(a.len(), b.len());
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn roundtrip() {
        let mut w = BitWriter::new();
        let items: Vec<(u32, u32)> =
            (0..1000u32).map(|i| (i.wrapping_mul(2654435761) >> (32 - i % 33).min(31), i % 33)).collect();
        for &(v, n) in &items {
            let v = if n == 0 {
                0
            } else if n == 32 {
                v
            } else {
                v & ((1 << n) - 1)
            };
            w.put(v, n);
        }
        let total = w.len();
        let buf = padded(&w.finish());
        let mut r = BitReader::new(&buf, 0);
        for &(v, n) in &items {
            let v = if n == 0 {
                0
            } else if n == 32 {
                v
            } else {
                v & ((1 << n) - 1)
            };
            assert_eq!(r.get(n), v);
        }
        assert_eq!(r.pos, total);
    }
}

/// Branchless MSB-first writer into a buffer reserved up front. Each `put` stores eight
/// bytes; [`FastWriter::reserve`] must have made room for the bits before them. Completed
/// bytes may be dropped from the front ([`FastWriter::discard`]): the buffer is then a window,
/// and [`FastWriter::finish`] returns the bytes since.
pub struct FastWriter {
    buf: Vec<u8>,
    /// Bytes dropped before `buf[0]`.
    base: usize,
    /// Bytes completed in `buf`.
    pos: usize,
    /// Pending bits in the low `n < 8` bits (higher bits are stale).
    acc: u64,
    n: u32,
}

impl FastWriter {
    pub fn with_capacity(bytes: usize) -> Self {
        FastWriter { buf: vec![0; bytes + 16], base: 0, pos: 0, acc: 0, n: 0 }
    }

    /// Makes room for `bytes` more bytes.
    #[inline(always)]
    pub fn reserve(&mut self, bytes: usize) {
        if self.pos + bytes + 16 > self.buf.len() {
            self.grow(bytes);
        }
    }

    #[inline(never)]
    fn grow(&mut self, bytes: usize) {
        let want = (self.buf.len() * 2).max(self.pos + bytes + 16);
        self.buf.resize(want, 0);
    }

    /// The completed bytes not dropped, and the number of bytes dropped before them.
    pub fn done(&self) -> (usize, &[u8]) {
        (self.base, &self.buf[..self.pos])
    }

    /// Drops the bytes before byte `at` (counted from the start; completed ones).
    pub fn discard(&mut self, at: usize) {
        let n = at - self.base;
        assert!(n <= self.pos);
        // The byte at `pos` need not be kept: the next `put` stores it again from `acc`.
        self.buf.copy_within(n..self.pos, 0);
        self.pos -= n;
        self.base = at;
    }

    /// Writes the low `n` bits of `v` (n <= 56).
    #[inline(always)]
    pub fn put(&mut self, v: u64, n: u32) {
        let mut l = self.local();
        l.put(v, n);
        self.put_back(l);
    }

    /// Number of bits written so far.
    #[inline(always)]
    pub fn len(&self) -> usize {
        (self.base + self.pos) * 8 + self.n as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The hot state; [`FastWriter::reserve`] must have made room for what is written
    /// through it before [`FastWriter::put_back`].
    #[inline(always)]
    pub fn local(&mut self) -> FastBits {
        FastBits { buf: self.buf.as_mut_ptr(), end: self.buf.len(), pos: self.pos, acc: self.acc, n: self.n }
    }

    #[inline(always)]
    pub fn put_back(&mut self, l: FastBits) {
        (self.pos, self.acc, self.n) = (l.pos, l.acc, l.n);
    }

    /// A position to return to with [`FastWriter::rollback`] (not across a discard).
    pub fn mark(&self) -> (usize, u64, u32) {
        (self.base + self.pos, self.acc, self.n)
    }

    pub fn rollback(&mut self, m: (usize, u64, u32)) {
        (self.pos, self.acc, self.n) = (m.0 - self.base, m.1, m.2);
    }

    /// Copies `n` bits from `r`.
    pub fn copy(&mut self, r: &mut BitReader, mut n: usize) {
        self.reserve(n / 8 + 8);
        while n >= 32 {
            self.put(r.get(32) as u64, 32);
            n -= 32;
        }
        self.put(r.get(n as u32) as u64, n as u32);
    }

    /// Pads with zero bits to a byte boundary and returns the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let len = self.pos + (self.n > 0) as usize;
        self.buf.truncate(len);
        self.buf
    }
}

/// [`FastWriter`] state by value, so that it stays in registers.
#[derive(Clone, Copy)]
pub struct FastBits {
    buf: *mut u8,
    end: usize,
    pos: usize,
    acc: u64,
    n: u32,
}

impl FastBits {
    /// Writes the low `n` bits of `v` (n <= 56).
    #[inline(always)]
    pub fn put(&mut self, v: u64, n: u32) {
        debug_assert!(n <= 56 && (n == 0 || v >> n == 0));
        assert!(self.pos + 8 <= self.end);
        // Separate dependency chains: acc (shift, or) and the bit count (add, and).
        self.acc = (self.acc << n) | v;
        let total = self.n + n;
        let out = (self.acc << (63 - total)) << 1;
        // SAFETY: pos + 8 <= end, the buffer's length.
        unsafe { (self.buf.add(self.pos) as *mut u64).write_unaligned(out.to_be()) };
        self.pos += (total >> 3) as usize;
        self.n = total & 7;
    }
}

/// MSB-first writer that stores whole 32-bit words, for many short writes (where the
/// occasional branch is cheaper than [`FastWriter`]'s store per write).
pub struct WordWriter {
    buf: Vec<u8>,
    /// Bytes completed (a multiple of 4).
    pos: usize,
    /// Pending bits in the low `n < 32` bits of acc (higher bits are stale).
    acc: u64,
    n: u32,
}

impl WordWriter {
    pub fn with_capacity(bytes: usize) -> Self {
        WordWriter { buf: vec![0; bytes + 16], pos: 0, acc: 0, n: 0 }
    }

    /// Makes room for `bytes` more bytes.
    #[inline(always)]
    pub fn reserve(&mut self, bytes: usize) {
        if self.pos + bytes + 16 > self.buf.len() {
            self.grow(bytes);
        }
    }

    #[inline(never)]
    fn grow(&mut self, bytes: usize) {
        let want = (self.buf.len() * 2).max(self.pos + bytes + 16);
        self.buf.resize(want, 0);
    }

    #[inline(always)]
    pub fn local(&mut self) -> WordBits {
        WordBits { buf: self.buf.as_mut_ptr(), end: self.buf.len(), pos: self.pos, acc: self.acc, n: self.n }
    }

    #[inline(always)]
    pub fn put_back(&mut self, l: WordBits) {
        (self.pos, self.acc, self.n) = (l.pos, l.acc, l.n);
    }

    pub fn len(&self) -> usize {
        self.pos * 8 + self.n as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn mark(&self) -> (usize, u64, u32) {
        (self.pos, self.acc, self.n)
    }

    pub fn rollback(&mut self, m: (usize, u64, u32)) {
        (self.pos, self.acc, self.n) = m;
    }

    /// Pads with zero bits to a byte boundary and returns the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let k = self.n as usize;
        let w = (self.acc << (64 - self.n.max(1))).to_be_bytes();
        let nb = k.div_ceil(8);
        self.buf[self.pos..self.pos + nb].copy_from_slice(&w[..nb]);
        self.buf.truncate(self.pos + nb);
        self.buf
    }
}

/// [`WordWriter`] state by value.
#[derive(Clone, Copy)]
pub struct WordBits {
    buf: *mut u8,
    end: usize,
    pos: usize,
    acc: u64,
    n: u32,
}

impl WordBits {
    /// Writes the low `n` bits of `v` (n <= 56).
    #[inline(always)]
    pub fn put(&mut self, v: u64, n: u32) {
        if n > 32 {
            self.put32(v >> 32, n - 32);
            self.put32(v & 0xffff_ffff, 32);
        } else {
            self.put32(v, n);
        }
    }

    /// Writes the low `n` bits of `v` (n <= 32).
    #[inline(always)]
    pub fn put32(&mut self, v: u64, n: u32) {
        debug_assert!(n <= 32 && (n == 0 || v >> n == 0));
        self.acc = (self.acc << n) | v;
        self.n += n;
        if self.n >= 32 {
            assert!(self.pos + 8 <= self.end);
            self.n -= 32;
            let w = (self.acc >> self.n) as u32;
            // SAFETY: pos + 4 <= end, the buffer's length.
            unsafe { (self.buf.add(self.pos) as *mut u32).write_unaligned(w.to_be()) };
            self.pos += 4;
        }
    }
}
