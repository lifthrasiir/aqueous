//! LZMA (LZMA1) coding: the packets of a stream, and the stream of packets.
//!
//! The model is written once, over [`Bits`], so that decoding and encoding cannot disagree.

use crate::rc::{Bits, Decoder, Encoder, HALF};

pub const MIN_LEN: u32 = 2;
pub const MAX_LEN: u32 = 273;
const STATES: usize = 12;
const POS_MAX: usize = 16;
/// The distance of the end marker.
pub const END: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Props {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
}

impl Props {
    pub fn from_byte(b: u8) -> Option<Self> {
        (b < 9 * 5 * 5).then_some(Props { lc: b as u32 % 9, lp: b as u32 / 9 % 5, pb: b as u32 / 45 })
    }

    pub fn byte(self) -> u8 {
        ((self.pb * 5 + self.lp) * 9 + self.lc) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Lit,
    Match,
    Rep0,
    Rep1,
    Rep2,
    Rep3,
    ShortRep,
}

/// A packet: a literal, a match (`dist` is the distance less one, as coded), a repeated match
/// of the distance `kind` names, or a single byte at the last distance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub kind: Kind,
    pub len: u16,
    pub dist: u32,
}

impl Packet {
    pub const LIT: Packet = Packet { kind: Kind::Lit, len: 1, dist: 0 };
    pub const SHORT_REP: Packet = Packet { kind: Kind::ShortRep, len: 1, dist: 0 };
}

struct LenProbs {
    choice: [u16; 2],
    low: [[u16; 8]; POS_MAX],
    mid: [[u16; 8]; POS_MAX],
    high: [u16; 256],
}

impl LenProbs {
    fn new() -> Self {
        LenProbs { choice: [HALF; 2], low: [[HALF; 8]; POS_MAX], mid: [[HALF; 8]; POS_MAX], high: [HALF; 256] }
    }
}

/// The probabilities, the state and the last four distances.
pub struct Model {
    props: Props,
    lit: Vec<u16>,
    is_match: [[u16; POS_MAX]; STATES],
    is_rep: [u16; STATES],
    is_rep_g0: [u16; STATES],
    is_rep_g1: [u16; STATES],
    is_rep_g2: [u16; STATES],
    is_rep0_long: [[u16; POS_MAX]; STATES],
    slot: [[u16; 64]; 4],
    spec: [u16; 115],
    align: [u16; 16],
    len: LenProbs,
    rep_len: LenProbs,
    pub state: usize,
    pub reps: [u32; 4],
}

#[inline]
fn tree<B: Bits>(c: &mut B, p: &mut [u16], n: u32, v: u32) -> u32 {
    let mut m = 1;
    for i in (0..n).rev() {
        m = m << 1 | c.bit(&mut p[m as usize], v >> i & 1);
    }
    m - (1 << n)
}

#[inline]
fn rtree<B: Bits>(c: &mut B, p: &mut [u16], n: u32, v: u32) -> u32 {
    let (mut m, mut s) = (1, 0);
    for i in 0..n {
        let b = c.bit(&mut p[m as usize], v >> i & 1);
        m = m << 1 | b;
        s |= b << i;
    }
    s
}

fn code_len<B: Bits>(c: &mut B, p: &mut LenProbs, ps: usize, v: u32) -> u32 {
    if c.bit(&mut p.choice[0], (v >= 8) as u32) == 0 {
        tree(c, &mut p.low[ps], 3, v)
    } else if c.bit(&mut p.choice[1], (v >= 16) as u32) == 0 {
        8 + tree(c, &mut p.mid[ps], 3, v.wrapping_sub(8))
    } else {
        16 + tree(c, &mut p.high, 8, v.wrapping_sub(16))
    }
}

pub fn slot_of(d: u32) -> u32 {
    if d < 4 {
        d
    } else {
        let n = 31 - d.leading_zeros();
        n << 1 | (d >> (n - 1) & 1)
    }
}

impl Model {
    pub fn new(props: Props) -> Self {
        Model {
            props,
            lit: vec![HALF; 0x300 << (props.lc + props.lp)],
            is_match: [[HALF; POS_MAX]; STATES],
            is_rep: [HALF; STATES],
            is_rep_g0: [HALF; STATES],
            is_rep_g1: [HALF; STATES],
            is_rep_g2: [HALF; STATES],
            is_rep0_long: [[HALF; POS_MAX]; STATES],
            slot: [[HALF; 64]; 4],
            spec: [HALF; 115],
            align: [HALF; 16],
            len: LenProbs::new(),
            rep_len: LenProbs::new(),
            state: 0,
            reps: [0; 4],
        }
    }

    fn code_dist<B: Bits>(&mut self, c: &mut B, len: u32, d: u32) -> u32 {
        let ls = (len - MIN_LEN).min(3) as usize;
        let slot = tree(c, &mut self.slot[ls], 6, slot_of(d));
        if slot < 4 {
            return slot;
        }
        let nb = (slot >> 1) - 1;
        let base = (2 | (slot & 1)) << nb;
        let rest = d.wrapping_sub(base);
        if slot < 14 {
            base + rtree(c, &mut self.spec[(base - slot) as usize..], nb, rest)
        } else {
            base + (c.direct(nb - 4, rest >> 4) << 4) + rtree(c, &mut self.align, 4, rest & 15)
        }
    }

    /// Codes the packet at `pos` of `out` (which holds everything before `pos`, and the
    /// packet's bytes too when encoding). Returns the packet, with `dist` the distance used.
    pub fn code<B: Bits>(&mut self, c: &mut B, out: &[u8], pos: usize, pk: Packet) -> Packet {
        let ps = pos & ((1 << self.props.pb) - 1);
        let st = self.state;
        if c.bit(&mut self.is_match[st][ps], (pk.kind != Kind::Lit) as u32) == 0 {
            let prev = if pos > 0 { out[pos - 1] as usize } else { 0 };
            let ls = ((pos & ((1 << self.props.lp) - 1)) << self.props.lc) + (prev >> (8 - self.props.lc));
            let p = &mut self.lit[0x300 * ls..0x300 * (ls + 1)];
            let byte = out.get(pos).copied().unwrap_or(0) as u32;
            let mut s = 1u32;
            if st >= 7 {
                let mut mb = pos.checked_sub(self.reps[0] as usize + 1).map_or(0, |i| out[i]) as u32;
                while s < 0x100 {
                    let mbit = mb >> 7 & 1;
                    mb <<= 1;
                    let i = 7 - (31 - s.leading_zeros());
                    let b = c.bit(&mut p[(((1 + mbit) << 8) + s) as usize], byte >> i & 1);
                    s = s << 1 | b;
                    if mbit != b {
                        break;
                    }
                }
            }
            while s < 0x100 {
                let i = 7 - (31 - s.leading_zeros());
                s = s << 1 | c.bit(&mut p[s as usize], byte >> i & 1);
            }
            self.state = if st < 4 {
                0
            } else if st < 10 {
                st - 3
            } else {
                st - 6
            };
            return Packet { kind: Kind::Lit, len: 1, dist: s - 0x100 };
        }
        let len;
        let kind;
        if c.bit(&mut self.is_rep[st], (pk.kind != Kind::Match) as u32) != 0 {
            let k = match pk.kind {
                Kind::Rep1 => 1,
                Kind::Rep2 => 2,
                Kind::Rep3 => 3,
                _ => 0,
            };
            if c.bit(&mut self.is_rep_g0[st], (k != 0) as u32) == 0 {
                if c.bit(&mut self.is_rep0_long[st][ps], (pk.kind != Kind::ShortRep) as u32) == 0 {
                    self.state = if st < 7 { 9 } else { 11 };
                    return Packet { kind: Kind::ShortRep, len: 1, dist: self.reps[0] };
                }
                kind = Kind::Rep0;
            } else {
                let k = if c.bit(&mut self.is_rep_g1[st], (k != 1) as u32) == 0 {
                    1
                } else if c.bit(&mut self.is_rep_g2[st], (k == 3) as u32) == 0 {
                    2
                } else {
                    3
                };
                kind = [Kind::Rep0, Kind::Rep1, Kind::Rep2, Kind::Rep3][k];
                let d = self.reps[k];
                self.reps.copy_within(0..k, 1);
                self.reps[0] = d;
            }
            len = code_len(c, &mut self.rep_len, ps, (pk.len as u32).wrapping_sub(MIN_LEN)) + MIN_LEN;
            self.state = if st < 7 { 8 } else { 11 };
        } else {
            len = code_len(c, &mut self.len, ps, (pk.len as u32).wrapping_sub(MIN_LEN)) + MIN_LEN;
            self.state = if st < 7 { 7 } else { 10 };
            let d = self.code_dist(c, len, pk.dist);
            kind = Kind::Match;
            if d == END {
                return Packet { kind, len: len as u16, dist: END };
            }
            self.reps.copy_within(0..3, 1);
            self.reps[0] = d;
        }
        Packet { kind, len: len as u16, dist: self.reps[0] }
    }
}

#[derive(Debug)]
pub enum Error {
    /// Not an LZMA stream (or a damaged one).
    Corrupt,
    /// A stream whose packets encode otherwise (its range coder ended otherwise).
    Encoded,
}

/// A decoded stream: its data, its packets (the end marker left out) and how it ended.
pub struct Decoded {
    pub data: Vec<u8>,
    pub packets: Vec<Packet>,
    /// Whether the stream ends with an end marker.
    pub marker: bool,
    /// The bytes of the stream the decoder read.
    pub used: usize,
}

/// Decodes a stream of `size` bytes (None: up to the end marker), up to `limit` bytes.
pub fn decode(input: &[u8], props: Props, size: Option<u64>, limit: usize) -> Result<Decoded, Error> {
    let mut c = Decoder::new(input).ok_or(Error::Corrupt)?;
    let mut m = Model::new(props);
    let mut data = Vec::with_capacity(size.map_or(input.len() * 4, |s| s as usize).min(limit));
    let mut packets = vec![];
    let mut marker = false;
    loop {
        let pos = data.len();
        if size == Some(pos as u64) && c.finished() {
            break;
        }
        let pk = m.code(&mut c, &data, pos, Packet::LIT);
        if c.overrun > 4 {
            return Err(Error::Corrupt);
        }
        match pk.kind {
            Kind::Lit => {
                data.push(pk.dist as u8);
                packets.push(Packet::LIT);
            }
            _ if pk.dist == END => {
                marker = true;
                break;
            }
            _ => {
                let src = pos.checked_sub(pk.dist as usize + 1).ok_or(Error::Corrupt)?;
                if pos + pk.len as usize > limit || size.is_some_and(|s| (pos + pk.len as usize) as u64 > s) {
                    return Err(Error::Corrupt);
                }
                for j in 0..pk.len as usize {
                    data.push(data[src + j]);
                }
                packets.push(if pk.kind == Kind::ShortRep { Packet::SHORT_REP } else { pk });
            }
        }
        if data.len() > limit {
            return Err(Error::Corrupt);
        }
    }
    if !c.finished() || c.overrun > 0 {
        return Err(Error::Corrupt);
    }
    Ok(Decoded { data, packets, marker, used: c.pos })
}

/// An encoder taking packets one by one.
pub struct StreamEncoder {
    c: Encoder,
    m: Model,
    pos: usize,
}

impl StreamEncoder {
    pub fn new(props: Props, capacity: usize) -> Self {
        StreamEncoder { c: Encoder::new(Vec::with_capacity(capacity)), m: Model::new(props), pos: 0 }
    }

    /// Codes the next packet of `data`.
    pub fn push(&mut self, data: &[u8], pk: Packet) {
        self.m.code(&mut self.c, data, self.pos, pk);
        self.pos += pk.len as usize;
    }

    /// Ends the stream, with the end marker if `marker`.
    pub fn finish(mut self, data: &[u8], marker: bool) -> Vec<u8> {
        if marker {
            self.m.code(&mut self.c, data, self.pos, Packet { kind: Kind::Match, len: MIN_LEN as u16, dist: END });
        }
        self.c.finish()
    }
}

/// Encodes the packets of `data`, and the end marker if `marker`.
pub fn encode(data: &[u8], props: Props, packets: &[Packet], marker: bool) -> Vec<u8> {
    let mut e = StreamEncoder::new(props, data.len() / 4);
    for &pk in packets {
        e.push(data, pk);
    }
    e.finish(data, marker)
}
