//! Bits as Vorbis packs them: least significant first, an n-bit value bit 0 first (Vorbis I
//! §2.1).

/// Reads a packet, telling where it ends (end-of-packet) rather than reading zeros past it.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    /// The bits read.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// The packet's bits.
    pub fn len(&self) -> usize {
        self.data.len() * 8
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The bits left.
    pub fn left(&self) -> usize {
        self.len() - self.pos
    }

    /// The next 57 bits (zeros past the end), not read.
    #[inline]
    pub fn peek(&self) -> u64 {
        let at = self.pos >> 3;
        let w = match self.data.get(at..at + 8) {
            Some(b) => u64::from_le_bytes(b.try_into().expect("8 bytes")),
            None => {
                let mut b = [0u8; 8];
                let rest = self.data.get(at..).unwrap_or(&[]);
                b[..rest.len()].copy_from_slice(rest);
                u64::from_le_bytes(b)
            }
        };
        w >> (self.pos & 7)
    }

    /// Skips `n` bits (within the packet).
    #[inline]
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }

    pub fn bit(&mut self) -> Option<u32> {
        self.bits(1)
    }

    /// An `n`-bit value (n ≤ 32), or None where fewer than `n` bits are left (nothing read).
    #[inline]
    pub fn bits(&mut self, n: u32) -> Option<u32> {
        if n as usize > self.left() {
            return None;
        }
        let v = if n == 0 { 0 } else { (self.peek() & ((1u64 << n) - 1)) as u32 };
        self.pos += n as usize;
        Some(v)
    }

    /// Back to bit `pos`.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }
}

/// Writes a packet.
#[derive(Debug, Clone, Default)]
pub struct Writer {
    data: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    pub fn new() -> Self {
        Writer::default()
    }

    /// The bits written.
    pub fn pos(&self) -> usize {
        self.data.len() * 8 + self.n as usize
    }

    pub fn bit(&mut self, b: u32) {
        self.bits(b & 1, 1);
    }

    /// The low `n` bits of `v` (n ≤ 32).
    #[inline]
    pub fn bits(&mut self, v: u32, n: u32) {
        let v = if n == 32 { v as u64 } else { (v as u64) & ((1u64 << n) - 1) };
        self.acc |= v << self.n;
        self.n += n;
        while self.n >= 8 {
            self.data.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// The bytes, the last padded with zero bits.
    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.data.push(self.acc as u8);
        }
        self.data
    }
}

/// The bits needed for `v`: ilog of Vorbis I §9.2.1.
pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}
