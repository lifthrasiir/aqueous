//! Emulation of zlib's (1.3.1) match finding and lazy evaluation, used as a token predictor.
//!
//! This is a port of `deflate_fast`, `deflate_slow`, `deflate_rle`, `deflate_huff`,
//! `longest_match` and `fill_window` from zlib's deflate.c, operating on the same 2 x w_size
//! window buffer with the same sliding and zero-filling, so that predictions match zlib's output
//! bit for bit (for a one-shot `deflate(Z_FINISH)` call). It is driven by the actual tokens:
//! [`Emulator::predict`] gives the token zlib would emit next, and [`Emulator::commit`] moves on
//! with the actual one. On a mismatch the state is forced to follow the actual token, so
//! prediction resumes right after.

use crate::deflate::{Block, Body, DynHeader, Token};
use crate::header::zlib_header;
use crate::huffman::{self, RleOp};

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const TOO_FAR: usize = 4096;
const WIN_INIT: usize = MAX_MATCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Strategy {
    Default = 0,
    Filtered = 1,
    HuffmanOnly = 2,
    Rle = 3,
    /// Z_FIXED: tokens as Default, only block types differ.
    Fixed = 4,
}

impl Strategy {
    pub const ALL: [Strategy; 5] = [
        Strategy::Default,
        Strategy::Filtered,
        Strategy::HuffmanOnly,
        Strategy::Rle,
        Strategy::Fixed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Strategy::Default => "default",
            Strategy::Filtered => "filtered",
            Strategy::HuffmanOnly => "huffman",
            Strategy::Rle => "rle",
            Strategy::Fixed => "fixed",
        }
    }
}

/// zlib's `deflateInit2` parameters that affect tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Params {
    /// 1..=9 (level 0 produces only stored blocks and is not emulated).
    pub level: u8,
    pub strategy: Strategy,
    /// 9..=15 (8 is promoted to 9 by zlib).
    pub wbits: u8,
    /// 1..=9.
    pub mem: u8,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            level: 6,
            strategy: Strategy::Default,
            wbits: 15,
            mem: 8,
        }
    }
}

impl std::fmt::Display for Params {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "zlib:level={},strategy={},wbits={},mem={}",
            self.level,
            self.strategy.name(),
            self.wbits,
            self.mem
        )
    }
}

impl Params {
    /// Tokens per block before zlib flushes it (`lit_bufsize - 1`).
    pub fn block_tokens(self) -> usize {
        (1 << (self.mem as usize + 6)) - 1
    }

    /// Whether an emulation with these parameters can be resumed from a [`Checkpoint`]: not
    /// deflate_fast's (levels 1-3), whose hash chains depend on the tokens.
    pub fn resumable(self) -> bool {
        self.level > 3 || matches!(self.strategy, Strategy::HuffmanOnly | Strategy::Rle)
    }

    fn valid(self) -> bool {
        (1..=9).contains(&self.level)
            && (9..=15).contains(&self.wbits)
            && (1..=9).contains(&self.mem)
    }
}

/// good_length, max_lazy, nice_length, max_chain; levels 1-3 use deflate_fast.
const CONFIG: [(usize, usize, usize, usize); 10] = [
    (0, 0, 0, 0),
    (4, 4, 8, 4),
    (4, 5, 16, 8),
    (4, 6, 32, 32),
    (4, 4, 16, 16),
    (8, 16, 32, 32),
    (8, 16, 128, 128),
    (8, 32, 128, 256),
    (32, 128, 258, 1024),
    (32, 258, 258, 4096),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algo {
    Fast,
    Slow,
    Rle,
    Huff,
}

/// Which emission point of which algorithm is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    /// deflate_slow: the match of the previous position (prev_length/prev_match).
    SlowMatch,
    /// deflate_slow: the pending literal at strstart - 1.
    SlowLit,
    /// deflate_slow: the last pending literal after the input ended.
    SlowEnd,
    /// deflate_fast / deflate_rle / deflate_huff at strstart.
    Match,
    Lit,
}

/// What a deflate_slow [`Emulator`] at a token boundary knows beyond its input, enough to
/// resume it there with [`Emulator::resume`] (positions are absolute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub strstart: usize,
    /// Start of the window; a multiple of its size.
    pub base: usize,
    /// End of the input read into the window.
    pub read: usize,
    pub high_water: usize,
    pub match_available: bool,
    /// The search result at `strstart - 1`'s successor, compared at the next position.
    pub match_length: usize,
    /// Where that match is; 0 when `match_length` is not a match.
    pub match_start: usize,
}

#[derive(Clone)]
pub struct Emulator<'a> {
    params: Params,
    input: &'a [u8],
    /// Bytes of `input` consumed into the window so far.
    read: usize,
    /// Absolute position of window[0].
    base: usize,

    algo: Algo,
    filtered: bool,
    good_match: usize,
    max_lazy: usize,
    nice_match: usize,
    max_chain: usize,

    w_size: usize,
    w_mask: usize,
    window: Vec<u8>,
    hash_shift: u32,
    hash_mask: usize,
    head: Vec<u16>,
    prev: Vec<u16>,

    strstart: usize,
    lookahead: usize,
    high_water: usize,
    match_length: usize,
    match_start: usize,
    prev_length: usize,
    prev_match: usize,
    match_available: bool,
    /// Hash head found by the last insertion at strstart (for forced literals).
    last_head: usize,

    pending: Option<(Point, usize, Token)>,
    done: bool,
}

impl<'a> Emulator<'a> {
    pub fn new(input: &'a [u8], p: Params) -> Self {
        assert!(p.valid(), "bad zlib parameters {p:?}");
        let (good, lazy, nice, chain) = CONFIG[p.level as usize];
        let algo = match p.strategy {
            Strategy::HuffmanOnly => Algo::Huff,
            Strategy::Rle => Algo::Rle,
            _ if p.level <= 3 => Algo::Fast,
            _ => Algo::Slow,
        };
        let w_size = 1usize << p.wbits;
        let hash_bits = p.mem as u32 + 7;
        Emulator {
            params: p,
            input,
            read: 0,
            base: 0,
            algo,
            filtered: p.strategy == Strategy::Filtered,
            good_match: good,
            max_lazy: lazy,
            nice_match: nice,
            max_chain: chain,
            w_size,
            w_mask: w_size - 1,
            window: vec![0; 2 * w_size],
            hash_shift: hash_bits.div_ceil(3),
            hash_mask: (1 << hash_bits) - 1,
            head: vec![0; 1 << hash_bits],
            prev: vec![0; w_size],
            strstart: 0,
            lookahead: 0,
            high_water: 0,
            match_length: MIN_MATCH - 1,
            match_start: 0,
            prev_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            last_head: 0,
            pending: None,
            done: false,
        }
    }

    /// The state at this token boundary, or `None` where resuming is not supported: not in
    /// deflate_fast (whose hash chains depend on the tokens), and in deflate_slow only while
    /// input remains to be read (so that every position before strstart has been inserted).
    /// deflate_rle and deflate_huff keep no state beyond the window.
    pub fn checkpoint(&self) -> Option<Checkpoint> {
        if self.algo == Algo::Fast
            || self.pending.is_some()
            || self.done
            || (self.algo == Algo::Slow && self.read == self.input.len())
        {
            return None;
        }
        let strstart = self.base + self.strstart;
        if self.algo != Algo::Slow {
            return Some(Checkpoint {
                strstart,
                base: self.base,
                read: self.read,
                high_water: self.high_water,
                match_available: false,
                match_length: MIN_MATCH - 1,
                match_start: 0,
            });
        }
        if self.match_length < MIN_MATCH - 1 {
            return None;
        }
        let match_start = if self.match_length >= MIN_MATCH {
            let m = self.base.wrapping_add(self.match_start);
            if m < self.base || m >= strstart {
                return None;
            }
            m
        } else {
            // Not a match: overwritten by the next search before it is used.
            0
        };
        Some(Checkpoint {
            strstart,
            base: self.base,
            read: self.read,
            high_water: self.high_water,
            match_available: self.match_available,
            match_length: self.match_length,
            match_start,
        })
    }

    /// An emulator in the state of [`Emulator::checkpoint`], rebuilt from the input alone:
    /// the window holds the input (and what zlib leaves past it), the hash chains get every
    /// position of the last window inserted again. `None` if `cp` cannot come from `p` and
    /// `input`.
    pub fn resume(input: &'a [u8], p: Params, cp: &Checkpoint) -> Option<Self> {
        if !p.valid() {
            return None;
        }
        let mut e = Emulator::new(input, p);
        let slow = e.algo == Algo::Slow;
        if e.algo == Algo::Fast || (!slow && cp.match_available) {
            return None;
        }
        let w = e.w_size;
        let ml = cp.match_length;
        let valid = cp.base.is_multiple_of(w)
            && cp.base <= cp.strstart
            && cp.strstart <= cp.read
            && (cp.read < input.len() || (!slow && cp.read == input.len()))
            && cp.read - cp.base <= 2 * w
            && cp.high_water <= 2 * w
            && (!cp.match_available || cp.strstart > cp.base)
            && (MIN_MATCH - 1..=MAX_MATCH).contains(&ml)
            && if ml >= MIN_MATCH {
                cp.base <= cp.match_start
                    && cp.match_start < cp.strstart
                    && cp.strstart - cp.match_start <= w
            } else {
                cp.match_start == 0
            };
        if !valid {
            return None;
        }
        e.base = cp.base;
        e.read = cp.read;
        e.high_water = cp.high_water;
        e.strstart = cp.strstart - cp.base;
        e.lookahead = cp.read - cp.strstart;
        e.match_available = cp.match_available;
        e.match_length = ml;
        e.match_start = if ml >= MIN_MATCH {
            cp.match_start - cp.base
        } else {
            0
        };
        for (i, b) in e.window.iter_mut().enumerate() {
            let a = cp.base + i;
            *b = if a < cp.read {
                input[a]
            } else if i >= cp.high_water {
                0
            } else {
                // Past the input read so far: what the window held before its last slide.
                a.checked_sub(w).map_or(0, |x| input[x])
            };
        }
        if slow {
            for a in cp.base.max(cp.strstart.saturating_sub(w))..cp.strstart {
                if a + MIN_MATCH <= cp.read {
                    e.insert(a - cp.base);
                }
            }
        }
        Some(e)
    }

    pub fn params(&self) -> Params {
        self.params
    }

    /// Absolute position of the start of zlib's window.
    pub fn window_base(&self) -> usize {
        self.base
    }

    fn max_dist(&self) -> usize {
        self.w_size - MIN_LOOKAHEAD
    }

    fn window_size(&self) -> usize {
        2 * self.w_size
    }

    fn fill_window(&mut self) {
        let wsize = self.w_size;
        loop {
            let mut more = self.window_size() - self.lookahead - self.strstart;
            if self.strstart >= wsize + self.max_dist() {
                self.window.copy_within(wsize..2 * wsize - more, 0);
                self.match_start = self.match_start.wrapping_sub(wsize);
                self.strstart -= wsize;
                self.base += wsize;
                for h in self.head.iter_mut().chain(self.prev.iter_mut()) {
                    *h = if *h as usize >= wsize {
                        *h - wsize as u16
                    } else {
                        0
                    };
                }
                more += wsize;
            }
            let avail = self.input.len() - self.read;
            if avail == 0 {
                break;
            }
            let n = avail.min(more);
            let at = self.strstart + self.lookahead;
            self.window[at..at + n].copy_from_slice(&self.input[self.read..self.read + n]);
            self.read += n;
            self.lookahead += n;
            // (Hash initialization: the hash is computed directly from the bytes; see `hash`.)
            if self.lookahead >= MIN_LOOKAHEAD || self.read == self.input.len() {
                break;
            }
        }
        let window_size = self.window_size();
        if self.high_water < window_size {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (window_size - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(window_size - self.high_water);
                self.window[self.high_water..self.high_water + init].fill(0);
                self.high_water += init;
            }
        }
    }

    /// zlib's rolling hash after three updates, which depends on the last three bytes only.
    #[inline]
    fn hash(&self, s: usize) -> usize {
        let w = &self.window;
        (((w[s] as usize) << (2 * self.hash_shift))
            ^ ((w[s + 1] as usize) << self.hash_shift)
            ^ w[s + 2] as usize)
            & self.hash_mask
    }

    /// INSERT_STRING: returns the previous head of the chain.
    #[inline]
    fn insert(&mut self, s: usize) -> usize {
        let h = self.hash(s);
        let head = self.head[h];
        self.prev[s & self.w_mask] = head;
        self.head[h] = s as u16;
        head as usize
    }

    fn longest_match(&mut self, mut cur: usize) -> usize {
        let mut chain = self.max_chain;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice = self.nice_match;
        let limit = if self.strstart > self.max_dist() {
            self.strstart - self.max_dist()
        } else {
            0
        };
        // zlib reads up to scan[MAX_MATCH], which strstart <= window_size - MIN_LOOKAHEAD allows.
        let ws = &self.window[scan..scan + MAX_MATCH + 1];
        let mut scan_end1 = ws[best_len - 1];
        let mut scan_end = ws[best_len];
        if self.prev_length >= self.good_match {
            chain >>= 2;
        }
        if nice > self.lookahead {
            nice = self.lookahead;
        }
        loop {
            let wm = &self.window[cur..cur + MAX_MATCH + 1];
            if wm[best_len] == scan_end
                && wm[best_len - 1] == scan_end1
                && wm[0] == ws[0]
                && wm[1] == ws[1]
            {
                // Byte 2 is equal by construction of the hash and is not compared.
                let len = match_len(ws, wm);
                if len > best_len {
                    self.match_start = cur;
                    best_len = len;
                    if len >= nice {
                        break;
                    }
                    scan_end1 = ws[best_len - 1];
                    scan_end = ws[best_len];
                }
            }
            cur = self.prev[cur & self.w_mask] as usize;
            if cur <= limit {
                break;
            }
            chain -= 1;
            if chain == 0 {
                break;
            }
        }
        best_len.min(self.lookahead)
    }

    /// The token zlib emits next and its absolute position, or `None` at the end.
    pub fn predict(&mut self) -> Option<(usize, Token)> {
        if self.pending.is_none() && !self.done {
            self.pending = match self.algo {
                Algo::Slow => self.run_slow(),
                Algo::Fast => self.run_fast(),
                Algo::Rle => self.run_rle(),
                Algo::Huff => self.run_huff(),
            };
            if self.pending.is_none() {
                self.done = true;
            }
        }
        self.pending.map(|(_, pos, t)| (pos, t))
    }

    fn lit(&self, rel: usize) -> (usize, Token) {
        (self.base + rel, Token::Lit(self.window[rel]))
    }

    fn mat(&self, rel: usize, len: usize, dist: usize) -> (usize, Token) {
        (
            self.base + rel,
            Token::Match {
                len: len as u16,
                dist: dist as u16,
                alt258: false,
            },
        )
    }

    fn run_slow(&mut self) -> Option<(Point, usize, Token)> {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    if self.match_available {
                        let (p, t) = self.lit(self.strstart - 1);
                        return Some((Point::SlowEnd, p, t));
                    }
                    return None;
                }
            }
            let mut hash_head = 0;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert(self.strstart);
            }
            self.last_head = hash_head;
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            if hash_head != 0
                && self.prev_length < self.max_lazy
                && self.strstart - hash_head <= self.max_dist()
            {
                self.slow_search(hash_head);
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let q = self.strstart - 1;
                // match_start wraps like zlib's unsigned one when a slide leaves it just below the
                // window, so the distance does too.
                let (p, t) = self.mat(q, self.prev_length, q.wrapping_sub(self.prev_match));
                return Some((Point::SlowMatch, p, t));
            } else if self.match_available {
                let (p, t) = self.lit(self.strstart - 1);
                return Some((Point::SlowLit, p, t));
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
    }

    /// longest_match plus deflate_slow's filtering of short matches.
    fn slow_search(&mut self, hash_head: usize) {
        self.match_length = self.longest_match(hash_head);
        if self.match_length <= 5
            && (self.filtered
                || (self.match_length == MIN_MATCH
                    && self.strstart.wrapping_sub(self.match_start) > TOO_FAR))
        {
            self.match_length = MIN_MATCH - 1;
        }
    }

    fn run_fast(&mut self) -> Option<(Point, usize, Token)> {
        if self.lookahead < MIN_LOOKAHEAD {
            self.fill_window();
            if self.lookahead == 0 {
                return None;
            }
        }
        let mut hash_head = 0;
        if self.lookahead >= MIN_MATCH {
            hash_head = self.insert(self.strstart);
        }
        if hash_head != 0 && self.strstart - hash_head <= self.max_dist() {
            self.match_length = self.longest_match(hash_head);
        }
        if self.match_length >= MIN_MATCH {
            let (p, t) = self.mat(
                self.strstart,
                self.match_length,
                self.strstart - self.match_start,
            );
            Some((Point::Match, p, t))
        } else {
            let (p, t) = self.lit(self.strstart);
            Some((Point::Lit, p, t))
        }
    }

    fn run_rle(&mut self) -> Option<(Point, usize, Token)> {
        if self.lookahead <= MAX_MATCH {
            self.fill_window();
            if self.lookahead == 0 {
                return None;
            }
        }
        self.match_length = 0;
        let s = self.strstart;
        let w = &self.window;
        if self.lookahead >= MIN_MATCH && s > 0 {
            let prev = w[s - 1];
            if w[s] == prev && w[s + 1] == prev && w[s + 2] == prev {
                // Same grouping as zlib: 8 compares, then the bound check.
                let mut scan = s + 2;
                let strend = s + MAX_MATCH;
                'outer: loop {
                    for _ in 0..8 {
                        scan += 1;
                        if w[scan] != prev {
                            break 'outer;
                        }
                    }
                    if scan >= strend {
                        break;
                    }
                }
                self.match_length = (MAX_MATCH - (strend - scan)).min(self.lookahead);
            }
        }
        if self.match_length >= MIN_MATCH {
            let (p, t) = self.mat(s, self.match_length, 1);
            Some((Point::Match, p, t))
        } else {
            let (p, t) = self.lit(s);
            Some((Point::Lit, p, t))
        }
    }

    fn run_huff(&mut self) -> Option<(Point, usize, Token)> {
        if self.lookahead == 0 {
            self.fill_window();
            if self.lookahead == 0 {
                return None;
            }
        }
        let (p, t) = self.lit(self.strstart);
        Some((Point::Lit, p, t))
    }

    /// Advances past the predicted token, replaced by `actual` if different. Returns false
    /// when `actual` cannot be followed (the emulation is then unusable).
    pub fn commit(&mut self, actual: Token) -> bool {
        let Some((point, pos, predicted)) = self.pending.take() else {
            return false;
        };
        let hit = actual == predicted;
        let alen = match actual {
            Token::Lit(_) => 0,
            Token::Match { len, .. } => len as usize,
        };
        match (self.algo, point) {
            (Algo::Slow, Point::SlowEnd) => {
                if !hit {
                    return false;
                }
                self.match_available = false;
            }
            (Algo::Slow, Point::SlowMatch | Point::SlowLit) => {
                if alen > 0 {
                    if alen - 1 > self.lookahead {
                        return false;
                    }
                    let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                    self.lookahead -= alen - 1;
                    for _ in 0..alen - 2 {
                        self.strstart += 1;
                        if self.strstart <= max_insert {
                            self.insert(self.strstart);
                        }
                    }
                    self.match_available = false;
                    self.match_length = MIN_MATCH - 1;
                    self.strstart += 1;
                } else {
                    if !hit {
                        // zlib wanted a match here. Make the next position pending with a
                        // fresh search, as if this literal had been chosen normally.
                        self.prev_length = MIN_MATCH - 1;
                        self.match_length = MIN_MATCH - 1;
                        let h = self.last_head;
                        if h != 0 && self.strstart - h <= self.max_dist() {
                            self.slow_search(h);
                        }
                    }
                    self.strstart += 1;
                    self.lookahead -= 1;
                }
            }
            (Algo::Fast, _) => {
                if alen > 0 {
                    if alen > self.lookahead {
                        return false;
                    }
                    self.lookahead -= alen;
                    if alen <= self.max_lazy && self.lookahead >= MIN_MATCH {
                        for _ in 0..alen - 1 {
                            self.strstart += 1;
                            self.insert(self.strstart);
                        }
                        self.strstart += 1;
                    } else {
                        self.strstart += alen;
                    }
                    self.match_length = 0;
                } else {
                    self.lookahead -= 1;
                    self.strstart += 1;
                    if !hit {
                        self.match_length = 0;
                    }
                }
            }
            (Algo::Rle | Algo::Huff, _) => {
                let n = alen.max(1);
                if n > self.lookahead {
                    return false;
                }
                self.lookahead -= n;
                self.strstart += n;
                self.match_length = 0;
            }
            _ => return false,
        }
        debug_assert_eq!(
            self.base + self.strstart - self.match_available as usize,
            pos + alen.max(1)
        );
        true
    }

    /// Lets zlib run on its own until `end` (absolute), e.g. over a stored block whose tokens
    /// are not in the stream. A token that would cross `end` is replaced by literals.
    pub fn skip_to(&mut self, end: usize) -> bool {
        while let Some((pos, t)) = self.predict() {
            if pos >= end {
                break;
            }
            let t = if pos + t_len(t) > end {
                Token::Lit(self.input[pos])
            } else {
                t
            };
            if !self.commit(t) {
                return false;
            }
        }
        true
    }
}

/// Whether zlib's `_tr_flush_block` would rather use fixed than dynamic codes for `tokens`
/// (Z_FIXED aside): fixed when `static_lenb <= opt_lenb`.
pub fn zlib_prefers_fixed(tokens: &[Token]) -> bool {
    let (opt, stat) = zlib_lenb(tokens, &zlib_header(tokens));
    stat <= opt
}

/// zlib's `opt_lenb` and `static_lenb`: the sizes in bytes of the block with dynamic and with
/// fixed codes, as `_tr_flush_block` computes them.
fn zlib_lenb(tokens: &[Token], h: &DynHeader) -> (i64, i64) {
    let mut lf = [0u64; 288];
    let mut df = [0u64; 32];
    lf[256] = 1;
    for &t in tokens {
        match t {
            Token::Lit(b) => lf[b as usize] += 1,
            Token::Match { len, dist, alt258 } => {
                lf[257 + if alt258 { 27 } else { huffman::len_code(len) }] += 1;
                df[huffman::dist_code(dist)] += 1;
            }
        }
    }
    let fixed_lit = huffman::fixed_lit_lens();
    let (mut opt, mut stat) = (0i64, 0i64);
    // gen_bitlen's accounting; symbols zlib forced into a tree count with frequency 1 and
    // get one bit (and their static length) taken off again.
    let mut add = |freq: u64, len: u8, slen: Option<u8>, xbits: u8| {
        if len == 0 {
            return;
        }
        let f = freq.max(1) as i64;
        opt += f * (len + xbits) as i64;
        if let Some(sl) = slen {
            stat += f * (sl + xbits) as i64;
        }
        if freq == 0 {
            opt -= 1;
            if let Some(sl) = slen {
                stat -= sl as i64;
            }
        }
    };
    for (n, &l) in h.lit_lens().iter().enumerate() {
        let x = if n >= 257 {
            huffman::LEN_EXTRA.get(n - 257).copied().unwrap_or(0)
        } else {
            0
        };
        add(lf[n], l, Some(fixed_lit[n]), x);
    }
    for (n, &l) in h.dist_lens().iter().enumerate() {
        add(
            df[n],
            l,
            Some(5),
            huffman::DIST_EXTRA.get(n).copied().unwrap_or(0),
        );
    }
    let mut cf = [0u64; 19];
    for op in &h.ops {
        cf[op.sym as usize] += 1;
    }
    for (n, &l) in h.cl_lens.iter().enumerate() {
        add(cf[n], l, None, RleOp::extra_bits(n as u8) as u8);
    }
    opt += 3 * h.hclen as i64 + 14;
    ((opt + 3 + 7) >> 3, (stat + 3 + 7) >> 3)
}

/// The next block exactly as zlib writes it: its tokens by emulation (up to `lit_bufsize - 1`
/// of them, starting at `start`), then `_tr_flush_block`'s choice between stored, fixed and
/// dynamic. `None` if the emulation cannot continue.
pub fn zlib_block(e: &mut Emulator, start: usize) -> Option<Block> {
    let limit = e.params().block_tokens();
    let mut tokens = vec![];
    let mut end = start;
    let last = loop {
        if tokens.len() == limit {
            break false;
        }
        match e.predict() {
            None => break true,
            Some((pos, t)) => {
                if pos != end || !e.commit(t) {
                    return None;
                }
                end += t.span();
                tokens.push(t);
            }
        }
    };
    let header = zlib_header(&tokens);
    let (opt_lenb, static_lenb) = zlib_lenb(&tokens, &header);
    let mut opt = opt_lenb;
    if static_lenb <= opt || e.params().strategy == Strategy::Fixed {
        opt = static_lenb;
    }
    let stored_len = (end - start) as i64;
    // The data must still be in zlib's window to be stored.
    let body = if stored_len + 4 <= opt && start >= e.window_base() && stored_len <= 65535 {
        Body::Stored {
            pad: 0,
            len: stored_len as u16,
        }
    } else if static_lenb == opt {
        Body::Fixed { tokens }
    } else {
        Body::Dynamic { header, tokens }
    };
    Some(Block { last, body })
}

/// One level's lazy-matching state in a [`MultiSlow`].
#[derive(Debug, Clone)]
struct Lane {
    params: Params,
    good: usize,
    lazy: usize,
    nice: usize,
    chain: usize,
    filtered: bool,
    prev_length: usize,
    prev_match: usize,
    match_length: usize,
    match_start: usize,
    predicted: Option<Token>,
    misses: usize,
    alive: bool,
}

/// A strict improvement found walking a hash chain from best length 2: the candidate's index
/// in the chain, its match length and its position.
#[derive(Debug, Clone, Copy)]
struct ChainRec {
    idx: usize,
    len: usize,
    cur: usize,
}

/// Several deflate_slow levels (4-9, default, filtered or fixed, same windowBits and memLevel)
/// emulated at once against the actual tokens, in SPMD fashion.
///
/// Driven by the actual tokens, these levels insert the same strings and keep the same window,
/// so all of that is shared; only the lazy-matching state differs per level ("lane"). The hash
/// chain at each position is walked once, recording the strict improvements from length 2;
/// every improvement a lane's `longest_match` would find (starting from its own previous
/// length, within its own chain limit and nice length) is among them, so each lane's result
/// follows exactly.
pub struct MultiSlow<'a> {
    e: Emulator<'a>,
    lanes: Vec<Lane>,
    recs: Vec<ChainRec>,
    /// Absolute position the records are for (`usize::MAX`: none).
    recs_at: usize,
    pending: Option<usize>,
    done: bool,
}

impl<'a> MultiSlow<'a> {
    /// `params` must all use deflate_slow (level >= 4, default, filtered or fixed strategy) with the
    /// same windowBits and memLevel.
    pub fn new(input: &'a [u8], params: &[Params]) -> Self {
        assert!(!params.is_empty());
        for p in params {
            assert!(
                p.level >= 4
                    && matches!(
                        p.strategy,
                        Strategy::Default | Strategy::Filtered | Strategy::Fixed
                    )
                    && p.wbits == params[0].wbits
                    && p.mem == params[0].mem,
                "{p} cannot share a MultiSlow"
            );
        }
        let lanes = params
            .iter()
            .map(|&p| {
                let (good, lazy, nice, chain) = CONFIG[p.level as usize];
                Lane {
                    params: p,
                    good,
                    lazy,
                    nice,
                    chain,
                    filtered: p.strategy == Strategy::Filtered,
                    prev_length: MIN_MATCH - 1,
                    prev_match: 0,
                    match_length: MIN_MATCH - 1,
                    match_start: 0,
                    predicted: None,
                    misses: 0,
                    alive: true,
                }
            })
            .collect();
        MultiSlow {
            e: Emulator::new(input, params[0]),
            lanes,
            recs: vec![],
            recs_at: usize::MAX,
            pending: None,
            done: false,
        }
    }

    /// Parameters and mispredictions so far of each lane still alive.
    pub fn lanes(&self) -> impl Iterator<Item = (usize, Params, usize)> + '_ {
        self.lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.alive)
            .map(|(i, l)| (i, l.params, l.misses))
    }

    pub fn kill(&mut self, lane: usize) {
        self.lanes[lane].alive = false;
    }

    pub fn any_alive(&self) -> bool {
        self.lanes.iter().any(|l| l.alive)
    }

    /// Walks the chain from `hash_head` at strstart once, deep enough for every live lane.
    fn records(&mut self, hash_head: usize) {
        // Keyed by absolute position: window slides reuse relative ones.
        if self.recs_at == self.e.base + self.e.strstart {
            return;
        }
        self.recs_at = self.e.base + self.e.strstart;
        self.recs.clear();
        let dmax = self
            .lanes
            .iter()
            .filter(|l| l.alive)
            .map(|l| l.chain)
            .max()
            .unwrap_or(0);
        let e = &self.e;
        let scan = e.strstart;
        let limit = if scan > e.max_dist() {
            scan - e.max_dist()
        } else {
            0
        };
        let ws = &e.window[scan..scan + MAX_MATCH + 1];
        let mut best = MIN_MATCH - 1;
        let (mut scan_end1, mut scan_end) = (ws[best - 1], ws[best]);
        let mut cur = hash_head;
        for idx in 0..dmax {
            let wm = &e.window[cur..cur + MAX_MATCH + 1];
            if wm[best] == scan_end && wm[best - 1] == scan_end1 && wm[0] == ws[0] && wm[1] == ws[1]
            {
                let len = match_len(ws, wm);
                if len > best {
                    self.recs.push(ChainRec { idx, len, cur });
                    best = len;
                    if best >= MAX_MATCH {
                        break;
                    }
                    scan_end1 = ws[best - 1];
                    scan_end = ws[best];
                }
            }
            cur = e.prev[cur & e.w_mask] as usize;
            if cur <= limit {
                break;
            }
        }
    }

    /// `longest_match` for one lane, from the shared records.
    fn lane_search(recs: &[ChainRec], lane: &mut Lane, lookahead: usize, strstart: usize) {
        let mut chain = lane.chain;
        if lane.prev_length >= lane.good {
            chain >>= 2;
        }
        let nice = lane.nice.min(lookahead);
        let mut best = lane.prev_length;
        for r in recs {
            if r.idx >= chain {
                break;
            }
            if r.len > best {
                best = r.len;
                lane.match_start = r.cur;
                if best >= nice {
                    break;
                }
            }
        }
        lane.match_length = best.min(lookahead);
        if lane.match_length <= 5
            && (lane.filtered
                || (lane.match_length == MIN_MATCH
                    && strstart.wrapping_sub(lane.match_start) > TOO_FAR))
        {
            lane.match_length = MIN_MATCH - 1;
        }
    }

    /// Runs to the next emission and returns its absolute position; each live lane's
    /// prediction is then set. `None` at the end of the input.
    pub fn predict(&mut self) -> Option<usize> {
        if self.pending.is_some() || self.done {
            return self.pending;
        }
        loop {
            let e = &mut self.e;
            if e.lookahead < MIN_LOOKAHEAD {
                let base = e.base;
                e.fill_window();
                // fill_window moves zlib's (single) match_start with the window.
                let slid = e.base - base;
                for l in &mut self.lanes {
                    l.match_start = l.match_start.wrapping_sub(slid);
                }
                if e.lookahead == 0 {
                    if e.match_available {
                        let q = e.strstart - 1;
                        let t = Token::Lit(e.window[q]);
                        for l in self.lanes.iter_mut().filter(|l| l.alive) {
                            l.predicted = Some(t);
                        }
                        self.pending = Some(e.base + q);
                        return self.pending;
                    }
                    self.done = true;
                    return None;
                }
            }
            let mut hash_head = 0;
            if e.lookahead >= MIN_MATCH {
                hash_head = e.insert(e.strstart);
            }
            e.last_head = hash_head;
            let (strstart, lookahead, max_dist) = (e.strstart, e.lookahead, e.max_dist());
            let search = hash_head != 0 && strstart - hash_head <= max_dist;
            if search
                && self
                    .lanes
                    .iter()
                    .any(|l| l.alive && l.match_length < l.lazy)
            {
                self.records(hash_head);
            }
            for l in self.lanes.iter_mut().filter(|l| l.alive) {
                l.prev_length = l.match_length;
                l.prev_match = l.match_start;
                l.match_length = MIN_MATCH - 1;
                if search && l.prev_length < l.lazy {
                    Self::lane_search(&self.recs, l, lookahead, strstart);
                }
            }
            let e = &mut self.e;
            if e.match_available {
                let q = e.strstart - 1;
                for l in self.lanes.iter_mut().filter(|l| l.alive) {
                    l.predicted = Some(
                        if l.prev_length >= MIN_MATCH && l.match_length <= l.prev_length {
                            Token::Match {
                                len: l.prev_length as u16,
                                dist: q.wrapping_sub(l.prev_match) as u16,
                                alt258: false,
                            }
                        } else {
                            Token::Lit(e.window[q])
                        },
                    );
                }
                self.pending = Some(e.base + q);
                return self.pending;
            }
            e.match_available = true;
            e.strstart += 1;
            e.lookahead -= 1;
        }
    }

    /// Moves on with the actual token, counting each live lane's misprediction.
    pub fn commit(&mut self, actual: Token) -> bool {
        if self.pending.take().is_none() {
            return false;
        }
        let alen = t_len(actual);
        let is_match = matches!(actual, Token::Match { .. });
        for l in self.lanes.iter_mut().filter(|l| l.alive) {
            l.misses += (l.predicted != Some(actual)) as usize;
        }
        let e = &mut self.e;
        if e.lookahead == 0 {
            // The final pending literal.
            if is_match {
                return false;
            }
            e.match_available = false;
            self.done = true;
            return true;
        }
        if is_match {
            if alen - 1 > e.lookahead {
                return false;
            }
            let max_insert = e.strstart + e.lookahead - MIN_MATCH;
            e.lookahead -= alen - 1;
            for _ in 0..alen - 2 {
                e.strstart += 1;
                if e.strstart <= max_insert {
                    e.insert(e.strstart);
                }
            }
            e.match_available = false;
            e.strstart += 1;
            for l in self.lanes.iter_mut().filter(|l| l.alive) {
                l.match_length = MIN_MATCH - 1;
            }
        } else {
            // Lanes that wanted a match here continue with a fresh search at the next
            // position, as the single emulator does.
            let h = e.last_head;
            let (strstart, lookahead, max_dist) = (e.strstart, e.lookahead, e.max_dist());
            let refresh = |l: &Lane| l.alive && matches!(l.predicted, Some(Token::Match { .. }));
            if h != 0 && strstart - h <= max_dist && self.lanes.iter().any(refresh) {
                self.records(h);
            }
            for l in self.lanes.iter_mut() {
                if refresh(l) {
                    l.prev_length = MIN_MATCH - 1;
                    l.match_length = MIN_MATCH - 1;
                    if h != 0 && strstart - h <= max_dist {
                        Self::lane_search(&self.recs, l, lookahead, strstart);
                    }
                }
            }
            let e = &mut self.e;
            e.strstart += 1;
            e.lookahead -= 1;
        }
        true
    }
}

/// Length of the match between `ws` and `wm` as zlib's `longest_match` measures it: byte 2 is
/// not compared, the result is the first mismatch from byte 3 on, at most MAX_MATCH.
#[inline]
fn match_len(ws: &[u8], wm: &[u8]) -> usize {
    let mut len = 3;
    loop {
        if len + 8 > MAX_MATCH {
            while len < MAX_MATCH && ws[len] == wm[len] {
                len += 1;
            }
            return len;
        }
        let x = u64::from_le_bytes(ws[len..len + 8].try_into().unwrap());
        let y = u64::from_le_bytes(wm[len..len + 8].try_into().unwrap());
        if x != y {
            return len + ((x ^ y).trailing_zeros() / 8) as usize;
        }
        len += 8;
    }
}

fn t_len(t: Token) -> usize {
    match t {
        Token::Lit(_) => 1,
        Token::Match { len, .. } => len as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Free-running emulation reproduces the input.
    #[test]
    fn free_run_covers_input() {
        let data: Vec<u8> = include_bytes!("deflate.rs").repeat(3);
        for level in 1..=9 {
            for strategy in [
                Strategy::Default,
                Strategy::Filtered,
                Strategy::Rle,
                Strategy::HuffmanOnly,
            ] {
                let p = Params {
                    level,
                    strategy,
                    ..Params::default()
                };
                let mut e = Emulator::new(&data, p);
                let mut out = vec![];
                while let Some((pos, t)) = e.predict() {
                    assert_eq!(pos, out.len(), "{p}");
                    match t {
                        Token::Lit(b) => out.push(b),
                        Token::Match { len, dist, .. } => {
                            for _ in 0..len {
                                out.push(out[out.len() - dist as usize]);
                            }
                        }
                    }
                    assert!(e.commit(t));
                }
                assert_eq!(out, data, "{p}");
            }
        }
    }

    fn test_data() -> Vec<u8> {
        let mut lcg = 1u32;
        let mut data = [
            &include_bytes!("deflate.rs")[..],
            include_bytes!("zlib.rs"),
            include_bytes!("detect.rs"),
        ]
        .concat();
        data.extend((0..20_000).map(|_| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 24) as u8
        }));
        data.extend(include_bytes!("huffman.rs"));
        data
    }

    /// An emulator resumed from a checkpoint continues exactly as the original, also when
    /// both are made to follow tokens other than their own.
    #[test]
    fn resume_continues_exactly() {
        let data = test_data();
        // Every `perturb`-th match is replaced by a literal (0: never).
        let drive = |perturb: usize, n: usize, pos: usize, t: Token| match t {
            Token::Match { .. } if perturb > 0 && n.is_multiple_of(perturb) => {
                Token::Lit(data[pos])
            }
            t => t,
        };
        for level in 4..=9 {
            for strategy in [Strategy::Default, Strategy::Filtered] {
                for wbits in [9, 12, 15] {
                    for perturb in [0, 7] {
                        let p = Params {
                            level,
                            strategy,
                            wbits,
                            mem: 8,
                        };
                        let mut e = Emulator::new(&data, p);
                        let (mut n, mut checked) = (0, 0);
                        loop {
                            if n % 997 == 0
                                && let Some(cp) = e.checkpoint()
                            {
                                let mut r = Emulator::resume(&data, p, &cp)
                                    .unwrap_or_else(|| panic!("{p}: {cp:?}"));
                                let mut a = e.clone();
                                for i in n..n + 2000 {
                                    let pa = a.predict();
                                    assert_eq!(pa, r.predict(), "{p} perturb {perturb} {cp:?}");
                                    let Some((pos, t)) = pa else { break };
                                    let t = drive(perturb, i, pos, t);
                                    assert!(a.commit(t) && r.commit(t));
                                }
                                assert_eq!(a.checkpoint(), r.checkpoint());
                                checked += 1;
                            }
                            let Some((pos, t)) = e.predict() else { break };
                            assert!(e.commit(drive(perturb, n, pos, t)));
                            n += 1;
                        }
                        assert!(checked > 10, "{p}: {checked} checkpoints");
                    }
                }
            }
        }
    }

    /// Checkpoints from corrupt metadata are refused or give an emulator that runs to the end
    /// without panicking.
    #[test]
    fn resume_never_panics() {
        let data = test_data();
        let mut lcg = 99u64;
        let mut rnd = |n: usize| {
            lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (lcg >> 33) as usize % n
        };
        let p = Params {
            wbits: 9,
            ..Params::default()
        };
        let w = 1 << p.wbits;
        let mut resumed = 0;
        for _ in 0..20_000 {
            let base = rnd(data.len() / w + 2) * w + [0, 0, 0, 1][rnd(4)];
            let near =
                |x: usize, r: &mut dyn FnMut(usize) -> usize| (x + r(4 * w)).saturating_sub(2 * w);
            let strstart = near(base, &mut rnd);
            let cp = Checkpoint {
                strstart,
                base,
                read: near(base + 2 * w, &mut rnd),
                high_water: near(2 * w, &mut rnd),
                match_available: rnd(2) == 1,
                match_length: rnd(300),
                match_start: near(strstart, &mut rnd),
            };
            let Some(mut e) = Emulator::resume(&data, p, &cp) else {
                continue;
            };
            resumed += 1;
            for _ in 0..2000 {
                let Some((_, t)) = e.predict() else { break };
                if !e.commit(t) {
                    break;
                }
            }
        }
        assert!(resumed > 100, "{resumed}");
    }
}
