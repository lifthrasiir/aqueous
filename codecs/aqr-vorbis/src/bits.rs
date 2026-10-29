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

    pub fn bit(&mut self) -> Option<u32> {
        let b = *self.data.get(self.pos >> 3)? >> (self.pos & 7) & 1;
        self.pos += 1;
        Some(b as u32)
    }

    /// An `n`-bit value (n ≤ 32), or None where fewer than `n` bits are left (nothing read).
    pub fn bits(&mut self, n: u32) -> Option<u32> {
        if self.pos + n as usize > self.len() {
            return None;
        }
        let mut v = 0u64;
        let (mut got, mut at) = (0, self.pos);
        while got < n {
            let take = (8 - (at & 7) as u32).min(n - got);
            let b = (self.data[at >> 3] >> (at & 7)) as u64 & ((1 << take) - 1);
            v |= b << got;
            got += take;
            at += take as usize;
        }
        self.pos = at;
        Some(v as u32)
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
    pos: usize,
}

impl Writer {
    pub fn new() -> Self {
        Writer::default()
    }

    /// The bits written.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn bit(&mut self, b: u32) {
        if self.pos & 7 == 0 {
            self.data.push(0);
        }
        *self.data.last_mut().expect("a byte") |= ((b & 1) as u8) << (self.pos & 7);
        self.pos += 1;
    }

    /// The low `n` bits of `v` (n ≤ 32).
    pub fn bits(&mut self, v: u32, n: u32) {
        for i in 0..n {
            self.bit(v >> i & 1);
        }
    }

    /// The bytes, the last padded with zero bits.
    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

/// The bits needed for `v`: ilog of Vorbis I §9.2.1.
pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}
