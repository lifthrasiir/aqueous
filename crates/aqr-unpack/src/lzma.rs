//! An LZMA decoder for decoding alone: aqr-lzma's model ([`aqr_lzma::lzma::Model`]) codes
//! packets both ways, through a trait, and gives them back one at a time; this one keeps the
//! coder in registers through a loop over the packets, writes the data where it goes and copies
//! matches in chunks, and reads the probabilities of a tree's next bit before its bit is known.
//! (1.8 times as fast; the model stays the reference, and the tests check one against the
//! other.)

use aqr_lzma::lzma::Props;

const TOP: u32 = 1 << 24;
const BITS: u32 = 11;
const ONE: u16 = 1 << BITS;
const HALF: u16 = ONE / 2;
const MOVE: u32 = 5;
const STATES: usize = 12;
const POS_MAX: usize = 16;
const MAX_LEN: usize = 273;
/// Bytes past a match that its copy may write (see [`aqr_flate::inflate::copy_match`]).
const OVERSHOOT: usize = 16;

/// The range decoder of a stream, between runs.
pub struct Rc<'a> {
    range: u32,
    code: u32,
    input: &'a [u8],
    /// Bytes read, those past the end of the input (read as zeros) too.
    pub pos: usize,
}

impl<'a> Rc<'a> {
    /// None where the stream does not start with the zero byte every encoder writes first.
    pub fn new(input: &'a [u8]) -> Option<Self> {
        if input.len() < 5 || input[0] != 0 {
            return None;
        }
        let code = u32::from_be_bytes(input[1..5].try_into().unwrap());
        Some(Rc { range: u32::MAX, code, input, pos: 5 })
    }

    /// Whether the coder ended where an encoder's flush would have left it.
    pub fn finished(&self) -> bool {
        self.code == 0
    }

    /// Bytes read past the end of the input.
    pub fn overrun(&self) -> usize {
        self.pos.saturating_sub(self.input.len())
    }
}

struct LenProbs {
    choice: [u16; 2],
    low: [[u16; 8]; POS_MAX],
    mid: [[u16; 8]; POS_MAX],
    high: [u16; 256],
}

const LEN_PROBS: LenProbs =
    LenProbs { choice: [HALF; 2], low: [[HALF; 8]; POS_MAX], mid: [[HALF; 8]; POS_MAX], high: [HALF; 256] };

/// The probabilities, the state and the last four distances.
pub struct Dec {
    lc: u32,
    lp_mask: usize,
    pb_mask: usize,
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
    state: usize,
    reps: [u32; 4],
}

/// Why a run stopped: at `stop`, or at the end marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Stop,
    Marker,
}

/// The data does not decode (a distance before the start, data past the end, or the input
/// read far past its end).
#[derive(Debug, PartialEq, Eq)]
pub struct Corrupt;

impl Dec {
    pub fn new(props: Props) -> Dec {
        let mut d = Dec {
            lc: 0,
            lp_mask: 0,
            pb_mask: 0,
            lit: Vec::new(),
            is_match: [[HALF; POS_MAX]; STATES],
            is_rep: [HALF; STATES],
            is_rep_g0: [HALF; STATES],
            is_rep_g1: [HALF; STATES],
            is_rep_g2: [HALF; STATES],
            is_rep0_long: [[HALF; POS_MAX]; STATES],
            slot: [[HALF; 64]; 4],
            spec: [HALF; 115],
            align: [HALF; 16],
            len: LEN_PROBS,
            rep_len: LEN_PROBS,
            state: 0,
            reps: [0; 4],
        };
        d.reset(props);
        d
    }

    /// The state of a new stream of properties `props` (LZMA2's reset of the state).
    pub fn reset(&mut self, props: Props) {
        (self.lc, self.lp_mask, self.pb_mask) = (props.lc, (1 << props.lp) - 1, (1 << props.pb) - 1);
        self.lit.clear();
        self.lit.resize(0x300 << (props.lc + props.lp), HALF);
        self.is_match = [[HALF; POS_MAX]; STATES];
        self.is_rep = [HALF; STATES];
        self.is_rep_g0 = [HALF; STATES];
        self.is_rep_g1 = [HALF; STATES];
        self.is_rep_g2 = [HALF; STATES];
        self.is_rep0_long = [[HALF; POS_MAX]; STATES];
        self.slot = [[HALF; 64]; 4];
        self.spec = [HALF; 115];
        self.align = [HALF; 16];
        self.len = LEN_PROBS;
        self.rep_len = LEN_PROBS;
        self.state = 0;
        self.reps = [0; 4];
    }

    /// Decodes into `out` past its length, as part of what it holds from `base` on (what
    /// matches may reach, and where positions count from), until it holds `stop` bytes or more
    /// (or the end marker), never more than `end` (`stop <= end`). Room is made for `end`
    /// bytes (and matches copied faster where there is a little more).
    pub fn run_vec(
        &mut self,
        rc: &mut Rc,
        out: &mut Vec<u8>,
        base: usize,
        stop: usize,
        end: usize,
    ) -> Result<Ended, Corrupt> {
        assert!(base <= out.len() && out.len() <= stop && stop <= end);
        // (no more: a match's copy writes past its end only where there is room)
        out.reserve(end - out.len());
        let mut pos = out.len();
        let room = out.capacity();
        // SAFETY: `out` holds `pos` bytes and has room for `room`
        let r = unsafe { self.run(rc, out.as_mut_ptr(), base, &mut pos, stop, end, room) };
        // SAFETY: `run` wrote the bytes up to `pos`
        unsafe { out.set_len(pos) };
        r
    }

    /// Decodes into `buf` from `*pos` (left where it stopped) as part of what it holds from
    /// `base` on, until `*pos >= stop` (or the end marker), never past `end`.
    ///
    /// # Safety
    /// `buf[base..*pos]` is initialized, `buf[*pos..room]` is allocated and nobody else's while
    /// this runs, and `base <= *pos <= stop <= end <= room`. Only `buf[*pos..room]` is written.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn run(
        &mut self,
        rc: &mut Rc,
        buf: *mut u8,
        base: usize,
        pos: &mut usize,
        stop: usize,
        end: usize,
        room: usize,
    ) -> Result<Ended, Corrupt> {
        debug_assert!(base <= *pos && *pos <= stop && stop <= end && end <= room);
        // (positions from `base` on)
        let buf = unsafe { buf.add(base) };
        let (mut p, stop, end, room) = (*pos - base, stop - base, end - base, room - base);
        let input = rc.input;
        let far = input.len() + 4;
        let (mut range, mut code, mut ip) = (rc.range, rc.code, rc.pos);
        // The input is read without a check while a packet's bytes are surely there (`ip <=
        // guard`), then from a copy of what is left, padded with zeros: `src + ip` is the byte at
        // `ip` either way. (A packet takes at most 48 bits to code, a byte each at most.)
        const SLACK: usize = 64;
        let mut tail = [0u8; 4 * SLACK];
        let mut src = input.as_ptr();
        let mut guard = input.len().saturating_sub(SLACK);
        let mut st = self.state;
        let [mut rep0, mut rep1, mut rep2, mut rep3] = self.reps;
        let (lc, lp_mask, pb_mask) = (self.lc, self.lp_mask, self.pb_mask);

        macro_rules! normalize {
            () => {
                if range < TOP {
                    range <<= 8;
                    // SAFETY: `ip < guard + SLACK`, below the input's length or the tail's
                    code = code << 8 | (unsafe { *src.wrapping_add(ip) }) as u32;
                    ip += 1;
                }
            };
        }
        macro_rules! bit {
            ($p:expr) => {{
                let pr: &mut u16 = $p;
                let bound = (range >> BITS) * *pr as u32;
                let b = if code < bound {
                    range = bound;
                    *pr += (ONE - *pr) >> MOVE;
                    0
                } else {
                    range -= bound;
                    code -= bound;
                    *pr -= *pr >> MOVE;
                    1
                };
                normalize!();
                b
            }};
        }
        // A bit hard to predict, of probability `v` (read already) at `pr`: chosen without a
        // branch. (Bits of a tree read the probabilities of both of the next bit's before this
        // one is known, which takes the read off the path from bit to bit, as 7-Zip's decoder in
        // assembly does.)
        macro_rules! bitv {
            ($pr:expr, $v:expr) => {{
                let v: u32 = $v;
                let bound = (range >> BITS) * v;
                let b = code >= bound;
                range = std::hint::select_unpredictable(b, range - bound, bound);
                code -= std::hint::select_unpredictable(b, bound, 0);
                // SAFETY: as the caller's
                unsafe {
                    *$pr = std::hint::select_unpredictable(b, v - (v >> MOVE), v + ((ONE as u32 - v) >> MOVE)) as u16
                };
                normalize!();
                b as usize
            }};
        }
        // a tree of `n` bits over `t` (of `1 << n` probabilities)
        macro_rules! tree {
            ($t:expr, $n:expr) => {{
                let t: &mut [u16] = &mut $t[..];
                assert!(t.len() >= 1 << $n);
                let t = t.as_mut_ptr();
                let mut m = 1usize;
                // SAFETY: `m < 1 << n` here and below
                let mut v = unsafe { *t.add(1) } as u32;
                for i in 0..$n {
                    let (c0, c1) = if i + 1 < $n { unsafe { (*t.add(2 * m), *t.add(2 * m + 1)) } } else { (0, 0) };
                    let b = bitv!(t.add(m), v);
                    m = 2 * m + b;
                    v = std::hint::select_unpredictable(b != 0, c1, c0) as u32;
                }
                m - (1 << $n)
            }};
        }
        // a reversed tree of `n` bits over `t` (of `1 << n` probabilities)
        macro_rules! rtree {
            ($t:expr, $n:expr) => {{
                let t: &mut [u16] = $t;
                let n: u32 = $n;
                assert!(t.len() >= 1 << n);
                let t = t.as_mut_ptr();
                let (mut m, mut r) = (1usize, 0u32);
                // SAFETY: `m < 1 << n` here and below
                let mut v = unsafe { *t.add(1) } as u32;
                for i in 0..n {
                    let (c0, c1) = if i + 1 < n { unsafe { (*t.add(2 * m), *t.add(2 * m + 1)) } } else { (0, 0) };
                    let b = bitv!(t.add(m), v);
                    m = 2 * m + b;
                    r |= (b as u32) << i;
                    v = std::hint::select_unpredictable(b != 0, c1, c0) as u32;
                }
                r
            }};
        }
        macro_rules! len {
            ($l:expr, $ps:expr) => {{
                let l: &mut LenProbs = $l;
                if bit!(&mut l.choice[0]) == 0 {
                    tree!(l.low[$ps], 3)
                } else if bit!(&mut l.choice[1]) == 0 {
                    8 + tree!(l.mid[$ps], 3)
                } else {
                    16 + tree!(l.high, 8)
                }
            }};
        }

        let r = 'run: loop {
            if p >= stop {
                break Ok(Ended::Stop);
            }
            if ip > guard {
                if guard == far {
                    break Err(Corrupt);
                }
                let at = ip.min(input.len());
                tail[..input.len() - at].copy_from_slice(&input[at..]);
                (src, guard) = (tail.as_ptr().wrapping_sub(at), far);
            }
            let ps = p & pb_mask;
            if bit!(&mut self.is_match[st][ps]) == 0 {
                // SAFETY: before `p`, initialized
                let prev = if p > 0 { (unsafe { *buf.add(p - 1) }) as usize } else { 0 };
                let ls = ((p & lp_mask) << lc) + (prev >> (8 - lc));
                let probs = self.lit[0x300 * ls..0x300 * ls + 0x300].as_mut_ptr();
                let mut s = 1usize;
                // (indices below 0x300 here and below)
                if st >= 7 {
                    let back = rep0 as usize + 1;
                    // SAFETY: as `prev`
                    let mut mb = if p >= back { (unsafe { *buf.add(p - back) }) as usize } else { 0 };
                    // (the bits of the byte at the last distance as context while the bits so far
                    // are its: `offs` is 0x100 until they differ, then 0; `mb` shifted a bit ahead)
                    let mut offs = 0x100usize;
                    mb <<= 1;
                    let mut at = offs + (mb & offs) + s;
                    // SAFETY: `offs <= 0x100`, `mb & offs <= 0x100`, `s < 0x100`
                    let mut v = unsafe { *probs.add(at) } as u32;
                    for i in 0..8 {
                        let next = mb << 1;
                        let (o0, o1) = (offs & !mb, offs & mb);
                        let (a0, a1) = (o0 + (next & o0) + 2 * s, o1 + (next & o1) + 2 * s + 1);
                        let (c0, c1) = if i < 7 { unsafe { (*probs.add(a0), *probs.add(a1)) } } else { (0, 0) };
                        let b = bitv!(probs.add(at), v);
                        s = 2 * s + b;
                        (offs, at) = std::hint::select_unpredictable(b != 0, (o1, a1), (o0, a0));
                        v = std::hint::select_unpredictable(b != 0, c1, c0) as u32;
                        mb = next;
                    }
                } else {
                    let mut v = unsafe { *probs.add(1) } as u32;
                    for i in 0..8 {
                        let (c0, c1) =
                            if i < 7 { unsafe { (*probs.add(2 * s), *probs.add(2 * s + 1)) } } else { (0, 0) };
                        let b = bitv!(probs.add(s), v);
                        s = 2 * s + b;
                        v = std::hint::select_unpredictable(b != 0, c1, c0) as u32;
                    }
                }
                // SAFETY: `p < stop <= room`
                unsafe { *buf.add(p) = s as u8 };
                p += 1;
                st = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5][st];
                continue;
            }
            let len;
            if bit!(&mut self.is_rep[st]) == 0 {
                len = len!(&mut self.len, ps) + 2;
                st = if st < 7 { 7 } else { 10 };
                let slot = tree!(self.slot[len.min(5) - 2], 6) as u32;
                let dist = if slot < 4 {
                    slot
                } else {
                    let nb = (slot >> 1) - 1;
                    let d = (2 | (slot & 1)) << nb;
                    if slot < 14 {
                        d + rtree!(&mut self.spec[(d - slot) as usize..], nb)
                    } else {
                        let mut v = 0u32;
                        for _ in 0..nb - 4 {
                            range >>= 1;
                            let b = (code >= range) as u32;
                            code -= range & b.wrapping_neg();
                            v = v << 1 | b;
                            normalize!();
                        }
                        d.wrapping_add(v << 4).wrapping_add(rtree!(&mut self.align, 4))
                    }
                };
                if dist == u32::MAX {
                    break Ok(Ended::Marker);
                }
                (rep3, rep2, rep1, rep0) = (rep2, rep1, rep0, dist);
            } else {
                if bit!(&mut self.is_rep_g0[st]) == 0 {
                    if bit!(&mut self.is_rep0_long[st][ps]) == 0 {
                        // a single byte at the last distance
                        let back = rep0 as usize + 1;
                        if p < back {
                            break Err(Corrupt);
                        }
                        st = if st < 7 { 9 } else { 11 };
                        // SAFETY: `p - back` initialized, `p < stop`
                        unsafe { *buf.add(p) = *buf.add(p - back) };
                        p += 1;
                        continue;
                    }
                } else {
                    let d;
                    if bit!(&mut self.is_rep_g1[st]) == 0 {
                        d = rep1;
                    } else {
                        if bit!(&mut self.is_rep_g2[st]) == 0 {
                            d = rep2;
                        } else {
                            d = rep3;
                            rep3 = rep2;
                        }
                        rep2 = rep1;
                    }
                    rep1 = rep0;
                    rep0 = d;
                }
                len = len!(&mut self.rep_len, ps) + 2;
                st = if st < 7 { 8 } else { 11 };
            }
            let back = rep0 as usize + 1;
            if p < back || end - p < len {
                break 'run Err(Corrupt);
            }
            debug_assert!(len <= MAX_LEN);
            // SAFETY: `p - back >= base`, initialized; the match ends by `end <= room`, and its
            // copy writes up to OVERSHOOT bytes more only where there is room for them
            unsafe {
                if room - p >= len + OVERSHOOT {
                    aqr_flate::inflate::copy_match(buf, p, back, len);
                } else {
                    for i in 0..len {
                        *buf.add(p + i) = *buf.add(p + i - back);
                    }
                }
            }
            p += len;
        };
        *pos = base + p;
        (rc.range, rc.code, rc.pos) = (range, code, ip);
        self.state = st;
        self.reps = [rep0, rep1, rep2, rep3];
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_lzma::lzma::{self, Kind, Packet};

    /// Packets of every kind (literals, matches near and far, overlapping, the four repeats,
    /// short repeats) and their data.
    fn sample(n: usize) -> (Vec<u8>, Vec<Packet>) {
        let mut s = 0x1234_5678u32;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let (mut d, mut pks) = (Vec::new(), Vec::new());
        let mut reps = [0u32; 4];
        while d.len() < n {
            let len = 2 + (rnd() % if rnd() % 4 == 0 { 272 } else { 10 }) as usize;
            let pk = match rnd() % 8 {
                _ if d.is_empty() => Packet::LIT,
                0..=2 => Packet { kind: Kind::Lit, len: 1, dist: 0 },
                3 => {
                    let far = if rnd() % 3 == 0 { d.len() } else { d.len().min(64) };
                    let dist = rnd() % far as u32;
                    reps = [dist, reps[0], reps[1], reps[2]];
                    Packet { kind: Kind::Match, len: len as u16, dist }
                }
                4 => Packet::SHORT_REP,
                k => {
                    let r = (k as usize - 5 + (rnd() % 2) as usize) % 4;
                    let dist = reps[r];
                    reps[..=r].rotate_right(1);
                    Packet { kind: [Kind::Rep0, Kind::Rep1, Kind::Rep2, Kind::Rep3][r], len: len as u16, dist }
                }
            };
            let back = reps[0] as usize + 1;
            match pk.kind {
                Kind::Lit => d.push(if rnd() % 2 == 0 { b"abcde"[rnd() as usize % 5] } else { rnd() as u8 }),
                // (repeats before the start: taken back, the packet a literal instead)
                _ if back > d.len() => {
                    reps = [0; 4];
                    pks.push(Packet::LIT);
                    d.push(0);
                    continue;
                }
                _ => {
                    for _ in 0..pk.len {
                        d.push(d[d.len() - back]);
                    }
                }
            }
            pks.push(pk);
        }
        (d, pks)
    }

    #[test]
    fn as_the_model() {
        let (data, pks) = sample(1 << 20);
        for (lc, lp, pb) in [(3, 0, 2), (0, 2, 0), (8, 0, 4), (4, 4, 4)] {
            let props = Props { lc, lp, pb };
            let packed = lzma::encode(&data, props, &pks, true);
            let mut rc = Rc::new(&packed).unwrap();
            let mut out = Vec::new();
            let r = Dec::new(props).run_vec(&mut rc, &mut out, 0, data.len() + 1, data.len() + MAX_LEN).unwrap();
            assert_eq!(r, Ended::Marker);
            assert!(out == data);
            assert_eq!(rc.pos, packed.len());
            assert!(rc.finished());
            // in pieces, of a known size, as part of more
            let mut rc = Rc::new(&packed).unwrap();
            let mut out = b"before".to_vec();
            let mut dec = Dec::new(props);
            for stop in (1..).map(|k| 6 + k * 77777) {
                let stop = stop.min(6 + data.len());
                assert_eq!(dec.run_vec(&mut rc, &mut out, 6, stop, 6 + data.len()).unwrap(), Ended::Stop);
                if stop == 6 + data.len() {
                    break;
                }
            }
            assert!(out[6..] == data);
            // the marker past the size; nothing more than the size
            let n = 6 + data.len();
            assert_eq!(dec.run_vec(&mut rc, &mut out, 6, n + 1, n + MAX_LEN).unwrap(), Ended::Marker);
            let mut rc = Rc::new(&packed).unwrap();
            let mut out = Vec::new();
            let n = data.len() - 1;
            assert_ne!(Dec::new(props).run_vec(&mut rc, &mut out, 0, n, n), Ok(Ended::Marker));
            // cut short: zeros read past the end, then damage
            let mut rc = Rc::new(&packed[..packed.len() / 2]).unwrap();
            let mut out = Vec::new();
            assert!(Dec::new(props).run_vec(&mut rc, &mut out, 0, data.len(), data.len()).is_err());
            assert!(rc.overrun() > 0);
        }
    }
}
