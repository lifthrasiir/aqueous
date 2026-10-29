//! Emulation of zlib 1.3.1's compressor as a token predictor (FORMAT.md, "zlib emulation"),
//! for a one-shot `deflate(Z_FINISH)` call: [`Emulator::predict`] gives the token zlib would
//! emit next, and [`Emulator::commit`] moves on with the actual one, so that prediction resumes
//! right after a mismatch.

use crate::deflate::{Block, Body, DynHeader, Token};
use crate::header::{freqs, zlib_header_of};
use crate::huffman::{self, RleOp};

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

/// What an emulator of the lazy parser (or of runs or Huffman only) knows at a token boundary
/// beyond its input, enough to resume it there with [`Emulator::resume`]: FORMAT.md,
/// "Checkpoints". Positions are data positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    /// The cursor's data position.
    pub cursor: usize,
    /// The data position of the window's start (the origin); a multiple of its size.
    pub origin: usize,
    /// The data fed into the window so far.
    pub fed: usize,
    /// The zero mark (a window index).
    pub zero_mark: usize,
    /// The lazy parser's held flag.
    pub held: bool,
    /// The current match's length.
    pub length: usize,
    /// The current match's data position; 0 when `length` is 2 or less.
    pub source: usize,
}

/// The parsers of FORMAT.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Huffman,
    Runs,
    Greedy,
    Lazy,
}

/// When a pending token is emitted, relative to the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// At the cursor (greedy, runs and Huffman).
    Here,
    /// The lazy parser's last literal.
    Final,
    /// The lazy parser's token at the held byte.
    Deferred,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    token: Token,
    /// Window index of the token.
    at: usize,
    when: When,
}

/// The match state of one parser: everything that depends on its parameters. Lanes of a lazy
/// run share the window, the chains, the cursor and the held flag.
#[derive(Clone)]
struct Lane {
    p: Params,
    good: usize,
    lazy: usize,
    nice: usize,
    chain: usize,
    filtered: bool,
    /// The current match (length, window index of the source).
    len: usize,
    src: isize,
    /// The deferred match.
    dlen: usize,
    dsrc: isize,
    pending: Option<Pending>,
    /// Whether the last commit hit the pending token.
    hit: bool,
    alive: bool,
    misses: usize,
    log: Vec<(usize, Token)>,
}

impl Lane {
    fn new(p: Params) -> Self {
        const GOOD: [usize; 9] = [4, 4, 4, 4, 8, 8, 8, 32, 32];
        const LAZY: [usize; 9] = [4, 5, 6, 4, 16, 16, 32, 128, 258];
        const NICE: [usize; 9] = [8, 16, 32, 16, 32, 128, 128, 258, 258];
        const CHAIN: [usize; 9] = [4, 8, 32, 16, 32, 128, 256, 1024, 4096];
        let l = p.level.clamp(1, 9) as usize - 1;
        Lane {
            p,
            good: GOOD[l],
            lazy: LAZY[l],
            nice: NICE[l],
            chain: CHAIN[l],
            filtered: p.strategy == Strategy::Filtered,
            len: 2,
            src: 0,
            dlen: 2,
            dsrc: 0,
            pending: None,
            hit: false,
            alive: true,
            misses: 0,
            log: vec![],
        }
    }
}

fn kind_of(p: Params) -> Kind {
    match p.strategy {
        Strategy::HuffmanOnly => Kind::Huffman,
        Strategy::Rle => Kind::Runs,
        _ if p.level <= 3 => Kind::Greedy,
        _ => Kind::Lazy,
    }
}

/// A chain search of one lane, walked along with others over the same chain.
#[derive(Clone, Copy)]
struct Job {
    lane: usize,
    best: usize,
    sigma: isize,
    budget: usize,
    stop: usize,
    active: bool,
}

/// Slack behind the window: the longest match plus two words.
const SLACK: usize = 280;

/// The window, the chains and the data fed so far.
#[derive(Clone)]
struct Window<'a> {
    input: &'a [u8],
    /// The window size.
    w: usize,
    max_dist: usize,
    hash_mask: usize,
    shift: u32,
    buf: Vec<u8>,
    heads: Vec<u16>,
    links: Vec<u16>,
    origin: usize,
    fed: usize,
    cursor: usize,
    ahead: usize,
    zero_mark: usize,
}

impl<'a> Window<'a> {
    fn new(input: &'a [u8], p: Params) -> Self {
        let w = 1usize << p.wbits.clamp(9, 15);
        let bits = p.mem.clamp(1, 9) as u32 + 7;
        Window {
            input,
            w,
            max_dist: w - 262,
            hash_mask: (1 << bits) - 1,
            shift: bits.div_ceil(3),
            buf: vec![0; 2 * w + SLACK],
            heads: vec![0; 1 << bits],
            links: vec![0; w],
            origin: 0,
            fed: 0,
            cursor: 0,
            ahead: 0,
            zero_mark: 0,
        }
    }

    #[inline(always)]
    fn key(&self, i: usize) -> usize {
        let t = self.shift;
        (((self.buf[i] as usize) << (2 * t))
            ^ ((self.buf[i + 1] as usize) << t)
            ^ self.buf[i + 2] as usize)
            & self.hash_mask
    }

    /// Chains window index `i`, giving the previous head of its key.
    #[inline(always)]
    fn chain(&mut self, i: usize) -> usize {
        let k = self.key(i);
        let v = self.heads[k];
        self.links[i & (self.w - 1)] = v;
        self.heads[k] = i as u16;
        v as usize
    }

    /// Slides the window by `w`.
    fn slide(&mut self) {
        let w = self.w;
        self.buf.copy_within(w..self.cursor + self.ahead, 0);
        self.origin += w;
        self.cursor -= w;
        let down = |v: &mut u16| *v = v.saturating_sub(w as u16);
        self.heads.iter_mut().for_each(down);
        self.links.iter_mut().for_each(down);
    }

    /// Copies data in; whether the refill procedure goes back to sliding.
    fn feed(&mut self) -> bool {
        let n = self.input.len();
        if self.fed == n {
            return false;
        }
        let room = 2 * self.w - self.ahead - self.cursor;
        let k = (n - self.fed).min(room);
        let at = self.cursor + self.ahead;
        self.buf[at..at + k].copy_from_slice(&self.input[self.fed..self.fed + k]);
        self.fed += k;
        self.ahead += k;
        self.ahead < 262 && self.fed < n
    }

    /// Zeroes the bytes just past the data, once.
    fn zero_tail(&mut self) {
        let top = 2 * self.w;
        if self.zero_mark < top {
            let e = self.cursor + self.ahead;
            let u = (e + 258).min(top);
            let from = self.zero_mark.max(e);
            if from < u {
                self.buf[from..u].fill(0);
            }
            self.zero_mark = self.zero_mark.max(u);
        }
    }

    /// The length (at most 258) of the match between the cursor and `x`, whose first two
    /// bytes and the hash agree.
    #[inline(always)]
    fn common(&self, x: usize) -> usize {
        let c = self.cursor;
        // Fixed-size views, so that the loop needs no bounds checks.
        let xs: &[u8; 274] = self.buf[x..x + 274].try_into().unwrap();
        let cs: &[u8; 274] = self.buf[c..c + 274].try_into().unwrap();
        let mut n = 3;
        while n < 258 {
            let a = u128::from_le_bytes(xs[n..n + 16].try_into().unwrap());
            let b = u128::from_le_bytes(cs[n..n + 16].try_into().unwrap());
            let d = a ^ b;
            if d != 0 {
                n += (d.trailing_zeros() / 8) as usize;
                break;
            }
            n += 16;
        }
        n.min(258)
    }

    /// One search from `h`: the best length (2 if none) and its source.
    #[inline(always)]
    fn walk1(&self, h: usize, mut budget: usize, stop: usize) -> (usize, usize) {
        let c = self.cursor;
        let floor = c.saturating_sub(self.max_dist);
        let buf = &self.buf[..];
        let (mut best, mut src) = (2, 0);
        let mut x = h;
        loop {
            if buf[x + best] == buf[c + best]
                && buf[x + best - 1] == buf[c + best - 1]
                && buf[x] == buf[c]
                && buf[x + 1] == buf[c + 1]
            {
                let len = self.common(x);
                if len > best {
                    best = len;
                    src = x;
                    if len >= stop {
                        break;
                    }
                }
            }
            budget -= 1;
            let next = self.links[x & (self.w - 1)] as usize;
            if budget == 0 || next <= floor {
                break;
            }
            x = next;
        }
        (best, src)
    }

    /// Searches the chain from `h` for the cursor on behalf of all `jobs` at once, each as
    /// its own search with its own best length and budget would.
    fn walk(&self, h: usize, jobs: &mut [Job]) {
        let c = self.cursor;
        let floor = c.saturating_sub(self.max_dist);
        let buf = &self.buf[..];
        let mut live = jobs.iter().filter(|j| j.active).count();
        let mut x = h;
        while live > 0 {
            let mut len = 0;
            for job in jobs.iter_mut().filter(|j| j.active) {
                let b = job.best;
                if buf[x + b] == buf[c + b]
                    && buf[x + b - 1] == buf[c + b - 1]
                    && buf[x] == buf[c]
                    && buf[x + 1] == buf[c + 1]
                {
                    if len == 0 {
                        len = self.common(x);
                    }
                    if len > b {
                        job.best = len;
                        job.sigma = x as isize;
                        if len >= job.stop {
                            job.active = false;
                            live -= 1;
                            continue;
                        }
                    }
                }
                job.budget -= 1;
                if job.budget == 0 {
                    job.active = false;
                    live -= 1;
                }
            }
            let next = self.links[x & (self.w - 1)] as usize;
            if live == 0 || next <= floor {
                break;
            }
            x = next;
        }
    }
}

/// The parsers' common machinery: a window and one or more lanes (several only for the lazy
/// parser, in [`MultiSlow`]).
#[derive(Clone)]
struct Engine<'a> {
    win: Window<'a>,
    kind: Kind,
    held: bool,
    last_head: usize,
    finished: bool,
    /// Whether lanes count and log their misses.
    track: bool,
    lanes: Vec<Lane>,
    jobs: Vec<Job>,
}

impl<'a> Engine<'a> {
    fn new(input: &'a [u8], params: &[Params], track: bool) -> Self {
        let first = params.first().copied().unwrap_or_default();
        Engine {
            win: Window::new(input, first),
            kind: kind_of(first),
            held: false,
            last_head: 0,
            finished: false,
            track,
            lanes: params.iter().map(|&p| Lane::new(p)).collect(),
            jobs: vec![],
        }
    }

    /// Refills the window.
    fn refill(&mut self) {
        loop {
            if self.win.cursor + 262 >= 2 * self.win.w {
                self.win.slide();
                let w = self.win.w as isize;
                for l in &mut self.lanes {
                    l.src -= w;
                }
            }
            if !self.win.feed() {
                break;
            }
        }
        self.win.zero_tail();
    }

    /// Queues lane `i`'s chain search at the deferred length.
    fn add_job(&mut self, i: usize) {
        let l = &self.lanes[i];
        let budget = if l.dlen >= l.good {
            l.chain / 4
        } else {
            l.chain
        };
        self.jobs.push(Job {
            lane: i,
            best: l.dlen,
            sigma: l.src,
            budget,
            stop: l.nice.min(self.win.ahead),
            active: true,
        });
    }

    /// Runs the queued searches from `h` as lazy searches, setting the lanes' current matches.
    fn run_jobs(&mut self, h: usize) {
        if self.jobs.is_empty() {
            return;
        }
        self.win.walk(h, &mut self.jobs);
        let (c, a) = (self.win.cursor as isize, self.win.ahead);
        for job in &self.jobs {
            let l = &mut self.lanes[job.lane];
            let mut len = job.best.min(a);
            if len <= 5 && (l.filtered || (len == 3 && c - job.sigma > 4096)) {
                len = 2;
            }
            l.len = len;
            l.src = job.sigma;
        }
        self.jobs.clear();
    }

    fn set_pending(&mut self, token: Token, at: usize, when: When) {
        for l in self.lanes.iter_mut().filter(|l| l.alive) {
            l.pending = Some(Pending { token, at, when });
        }
    }

    /// Runs the parser's step, giving the alive lanes pending tokens (or finishing).
    fn step(&mut self) {
        match self.kind {
            Kind::Lazy => self.step_lazy(),
            Kind::Greedy => self.step_greedy(),
            Kind::Runs => self.step_runs(),
            Kind::Huffman => self.step_huffman(),
        }
    }

    fn step_lazy(&mut self) {
        loop {
            if self.win.ahead < 262 {
                self.refill();
                if self.win.ahead == 0 {
                    if self.held {
                        let at = self.win.cursor - 1;
                        let token = Token::Lit(self.win.buf[at]);
                        self.set_pending(token, at, When::Final);
                    } else {
                        self.finished = true;
                    }
                    return;
                }
            }
            let c = self.win.cursor;
            let h = if self.win.ahead >= 3 {
                self.win.chain(c)
            } else {
                0
            };
            self.last_head = h;
            self.jobs.clear();
            let near = h != 0 && c.wrapping_sub(h) <= self.win.max_dist;
            for i in 0..self.lanes.len() {
                let l = &mut self.lanes[i];
                if !l.alive {
                    continue;
                }
                l.dlen = l.len;
                l.dsrc = l.src;
                l.len = 2;
                if near && l.dlen < l.lazy {
                    self.add_job(i);
                }
            }
            self.run_jobs(h);
            let mut ended = false;
            for l in self.lanes.iter_mut().filter(|l| l.alive) {
                if l.dlen >= 3 && l.len <= l.dlen {
                    let dist = (c as isize - 1 - l.dsrc) as u16;
                    let token = Token::Match {
                        len: l.dlen as u16,
                        dist,
                        alt258: false,
                    };
                    l.pending = Some(Pending {
                        token,
                        at: c - 1,
                        when: When::Deferred,
                    });
                    ended = true;
                } else if self.held {
                    let token = Token::Lit(self.win.buf[c - 1]);
                    l.pending = Some(Pending {
                        token,
                        at: c - 1,
                        when: When::Deferred,
                    });
                    ended = true;
                }
            }
            if ended {
                return;
            }
            self.held = true;
            self.win.cursor += 1;
            self.win.ahead -= 1;
        }
    }

    fn step_greedy(&mut self) {
        if self.win.ahead < 262 {
            self.refill();
            if self.win.ahead == 0 {
                self.finished = true;
                return;
            }
        }
        let c = self.win.cursor;
        let h = if self.win.ahead >= 3 {
            self.win.chain(c)
        } else {
            0
        };
        let l = &mut self.lanes[0];
        if h != 0 && c.wrapping_sub(h) <= self.win.max_dist {
            let mut job = [Job {
                lane: 0,
                best: 2,
                sigma: l.src,
                budget: l.chain,
                stop: l.nice.min(self.win.ahead),
                active: true,
            }];
            self.win.walk(h, &mut job);
            l.len = job[0].best.min(self.win.ahead);
            l.src = job[0].sigma;
        }
        let token = if l.len >= 3 {
            Token::Match {
                len: l.len as u16,
                dist: (c as isize - l.src) as u16,
                alt258: false,
            }
        } else {
            Token::Lit(self.win.buf[c])
        };
        self.set_pending(token, c, When::Here);
    }

    fn step_runs(&mut self) {
        if self.win.ahead <= 258 {
            self.refill();
            if self.win.ahead == 0 {
                self.finished = true;
                return;
            }
        }
        let (c, a) = (self.win.cursor, self.win.ahead);
        let buf = &self.win.buf;
        let mut len = 0;
        if a >= 3 && c > 0 {
            let b = buf[c - 1];
            if buf[c] == b && buf[c + 1] == b && buf[c + 2] == b {
                let mut r = 3;
                while r < 258 && buf[c + r] == b {
                    r += 1;
                }
                len = r.min(a);
            }
        }
        self.lanes[0].len = len;
        let token = if len >= 3 {
            Token::Match {
                len: len as u16,
                dist: 1,
                alt258: false,
            }
        } else {
            Token::Lit(buf[c])
        };
        self.set_pending(token, c, When::Here);
    }

    fn step_huffman(&mut self) {
        if self.win.ahead == 0 {
            self.refill();
            if self.win.ahead == 0 {
                self.finished = true;
                return;
            }
        }
        let c = self.win.cursor;
        let token = Token::Lit(self.win.buf[c]);
        self.set_pending(token, c, When::Here);
    }

    /// Follows the actual token `t`; false where it cannot.
    fn commit(&mut self, t: Token) -> bool {
        let Some(rep) = self.lanes.iter().position(|l| l.alive) else {
            return false;
        };
        let Some(first) = self.lanes[rep].pending else {
            return false;
        };
        let (o, track) = (self.win.origin, self.track);
        for l in self.lanes.iter_mut().filter(|l| l.alive) {
            if let Some(p) = l.pending.take() {
                l.hit = p.token == t;
                if track && !l.hit {
                    l.misses += 1;
                    l.log.push((o + p.at, p.token));
                }
            }
        }
        let n = match t {
            Token::Lit(_) => 0,
            Token::Match { len, .. } => len as usize,
        };
        if matches!(t, Token::Match { .. }) && n < 3 {
            return false;
        }
        match (self.kind, first.when) {
            (Kind::Lazy, When::Final) => {
                if !self.lanes[rep].hit {
                    return false;
                }
                self.held = false;
            }
            (Kind::Lazy, _) if n > 0 => {
                let win = &mut self.win;
                if n - 1 > win.ahead {
                    return false;
                }
                let last = (win.cursor + win.ahead) as isize - 3;
                win.ahead -= n - 1;
                for _ in 0..n - 2 {
                    win.cursor += 1;
                    if win.cursor as isize <= last {
                        win.chain(win.cursor);
                    }
                }
                self.held = false;
                for l in self.lanes.iter_mut().filter(|l| l.alive) {
                    l.len = 2;
                }
                win.cursor += 1;
            }
            (Kind::Lazy, _) => {
                if self.win.ahead == 0 {
                    return false;
                }
                let (h, c) = (self.last_head, self.win.cursor);
                let near = h != 0 && c.wrapping_sub(h) <= self.win.max_dist;
                self.jobs.clear();
                for i in 0..self.lanes.len() {
                    let l = &mut self.lanes[i];
                    if l.alive && !l.hit {
                        l.dlen = 2;
                        l.len = 2;
                        if near {
                            self.add_job(i);
                        }
                    }
                }
                self.run_jobs(h);
                self.win.cursor += 1;
                self.win.ahead -= 1;
            }
            (Kind::Greedy, _) if n > 0 => {
                let win = &mut self.win;
                if n > win.ahead {
                    return false;
                }
                win.ahead -= n;
                let l = &mut self.lanes[0];
                if n <= l.lazy && win.ahead >= 3 {
                    for _ in 0..n - 1 {
                        win.cursor += 1;
                        win.chain(win.cursor);
                    }
                    win.cursor += 1;
                } else {
                    win.cursor += n;
                }
                l.len = 0;
            }
            (Kind::Greedy, _) => {
                if self.win.ahead == 0 {
                    return false;
                }
                self.win.ahead -= 1;
                self.win.cursor += 1;
                let l = &mut self.lanes[0];
                if !l.hit {
                    l.len = 0;
                }
            }
            _ => {
                let k = n.max(1);
                if k > self.win.ahead {
                    return false;
                }
                self.win.ahead -= k;
                self.win.cursor += k;
                self.lanes[0].len = 0;
            }
        }
        true
    }
}

/// Bytes an [`Emulator`] of window bits `wbits` and memory level `mem` takes: the window's
/// buffer (twice the window), its heads and links (u16 each) and its lane.
pub const fn emulator_bytes(wbits: u8, mem: u8) -> u64 {
    let w = 1u64 << clamp(wbits, 9, 15);
    2 * w + 280 + 2 * (1 << (clamp(mem, 1, 9) as u32 + 7)) + 2 * w + 256
}

const fn clamp(x: u8, lo: u8, hi: u8) -> u8 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// The emulator of FORMAT.md, "zlib emulation".
#[derive(Clone)]
pub struct Emulator<'a> {
    p: Params,
    e: Engine<'a>,
}

impl<'a> Emulator<'a> {
    /// A fresh emulator of `p` over the whole data `input`.
    pub fn new(input: &'a [u8], p: Params) -> Self {
        assert!(p.valid(), "bad zlib parameters {p:?}");
        Emulator {
            p,
            e: Engine::new(input, &[p], false),
        }
    }

    /// The checkpoint of this emulator, where it can be resumed (see [`Params::resumable`]),
    /// it has no pending token and is not finished.
    pub fn checkpoint(&self) -> Option<Checkpoint> {
        let e = &self.e;
        let l = &e.lanes[0];
        if !self.p.resumable() || e.finished || l.pending.is_some() {
            return None;
        }
        let w = &e.win;
        let (length, source) = if e.kind == Kind::Lazy && l.len > 2 {
            (l.len, usize::try_from(w.origin as isize + l.src).ok()?)
        } else if e.kind == Kind::Lazy {
            (l.len, 0)
        } else {
            (2, 0)
        };
        Some(Checkpoint {
            cursor: w.origin + w.cursor,
            origin: w.origin,
            fed: w.fed,
            zero_mark: w.zero_mark,
            held: e.held,
            length,
            source,
        })
    }

    /// The emulator rebuilt from `input` and a checkpoint (FORMAT.md, "Checkpoints"), or
    /// None where the checkpoint is invalid.
    pub fn resume(input: &'a [u8], p: Params, cp: &Checkpoint) -> Option<Self> {
        if !p.valid() || !p.resumable() {
            return None;
        }
        let mut em = Emulator::new(input, p);
        let e = &mut em.e;
        let (w, n) = (e.win.w, input.len());
        let (o, c_pos, f) = (cp.origin, cp.cursor, cp.fed);
        let held = cp.held as usize;
        let lazy = e.kind == Kind::Lazy;
        if o % w != 0
            || (cp.held && (!lazy || c_pos <= o))
            || !(o <= c_pos && c_pos <= f && f <= n)
            || f - o > 2 * w
            || cp.zero_mark > 2 * w
            || cp.length > 258
        {
            return None;
        }
        if cp.length >= 3 && !(o <= cp.source && cp.source + held < c_pos && c_pos - cp.source <= w)
        {
            return None;
        }
        let z = cp.zero_mark;
        let win = &mut e.win;
        win.origin = o;
        win.fed = f;
        win.zero_mark = z;
        win.cursor = c_pos - o;
        win.ahead = f - c_pos;
        for i in 0..2 * w {
            let p = o + i;
            win.buf[i] = if p < f {
                input[p]
            } else if i >= z {
                0
            } else {
                let k = if p < n { 1 } else { (p - n) / w + 1 };
                if k * w <= o { input[p - k * w] } else { 0 }
            };
        }
        e.held = cp.held;
        let l = &mut e.lanes[0];
        l.len = cp.length;
        l.src = if cp.length >= 3 {
            cp.source as isize - o as isize
        } else {
            0
        };
        if lazy {
            for p in o.max(c_pos.saturating_sub(w))..c_pos {
                if p + 3 <= f {
                    win.chain(p - o);
                }
            }
        }
        Some(em)
    }

    pub fn params(&self) -> Params {
        self.p
    }

    /// The data position of the window's start (the origin).
    pub fn window_base(&self) -> usize {
        self.e.win.origin
    }

    /// `predict()`: the token zlib emits next and its data position, or `None` at the end.
    pub fn predict(&mut self) -> Option<(usize, Token)> {
        if self.e.lanes[0].pending.is_none() && !self.e.finished {
            self.e.step();
        }
        let p = self.e.lanes[0].pending?;
        Some((self.e.win.origin + p.at, p.token))
    }

    /// `commit(actual)`: false where it fails.
    pub fn commit(&mut self, actual: Token) -> bool {
        self.e.commit(actual)
    }

    /// Runs the parser on its own, as zlib does: pushes the tokens it emits next to `out`
    /// until `out` holds `limit` of them (false) or the data ends (true). As [`predict`] and
    /// [`commit`] of each predicted token would, but faster for deflate_fast (levels 1-3).
    ///
    /// [`predict`]: Emulator::predict
    /// [`commit`]: Emulator::commit
    pub fn run(&mut self, out: &mut Vec<Token>, limit: usize) -> bool {
        if self.e.kind != Kind::Greedy || self.e.lanes[0].pending.is_some() {
            while out.len() < limit {
                let Some((_, t)) = self.predict() else {
                    return true;
                };
                let ok = self.commit(t);
                debug_assert!(ok);
                out.push(t);
            }
            return false;
        }
        let e = &mut self.e;
        let l = &e.lanes[0];
        let (chain, nice, lazy) = (l.chain, l.nice, l.lazy);
        let ended = loop {
            if out.len() >= limit {
                break false;
            }
            if e.finished {
                break true;
            }
            if e.win.ahead < 262 {
                e.refill();
                if e.win.ahead == 0 {
                    e.finished = true;
                    break true;
                }
            }
            let win = &mut e.win;
            let (c, a) = (win.cursor, win.ahead);
            let mut len = 0;
            let mut src = 0;
            if a >= 3 {
                let h = win.chain(c);
                if h != 0 && c.wrapping_sub(h) <= win.max_dist {
                    (len, src) = win.walk1(h, chain, nice.min(a));
                    len = len.min(a);
                }
            }
            if len >= 3 {
                out.push(Token::Match {
                    len: len as u16,
                    dist: (c - src) as u16,
                    alt258: false,
                });
                win.ahead -= len;
                if len <= lazy && win.ahead >= 3 {
                    for _ in 0..len - 1 {
                        win.cursor += 1;
                        win.chain(win.cursor);
                    }
                    win.cursor += 1;
                } else {
                    win.cursor += len;
                }
            } else {
                out.push(Token::Lit(win.buf[c]));
                win.ahead -= 1;
                win.cursor += 1;
            }
        };
        e.lanes[0].len = 0;
        ended
    }

    /// `skip_to(end)`: false where it fails.
    pub fn skip_to(&mut self, end: usize) -> bool {
        while let Some((q, t)) = self.predict() {
            if q >= end {
                break;
            }
            let t = if q + t.span() > end {
                Token::Lit(self.e.win.input[q])
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
    let (lf, df) = freqs(tokens);
    let (opt, stat) = zlib_lenb(&lf, &df, &zlib_header_of(&lf, &df));
    stat <= opt
}

/// zlib's `opt_lenb` and `static_lenb`: the sizes in bytes of the block with dynamic and with
/// fixed codes, as `_tr_flush_block` computes them, for a block whose [`freqs`] are `lf` and
/// `df`.
fn zlib_lenb(lf: &[u32; 288], df: &[u32; 32], h: &DynHeader) -> (i64, i64) {
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
        add(lf[n] as u64, l, Some(fixed_lit[n]), x);
    }
    for (n, &l) in h.dist_lens().iter().enumerate() {
        add(
            df[n] as u64,
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
    let (lf, df) = freqs(&tokens);
    Some(flush_block(e, start, end, tokens, &lf, &df, last))
}

/// `_tr_flush_block`'s choice between stored, fixed and dynamic for the block of `tokens`,
/// data `start..end`, whose [`freqs`] are `lf` and `df`, as the emulator `e` stands after them.
fn flush_block(
    e: &Emulator,
    start: usize,
    end: usize,
    tokens: Vec<Token>,
    lf: &[u32; 288],
    df: &[u32; 32],
    last: bool,
) -> Block {
    let header = zlib_header_of(lf, df);
    let (opt_lenb, static_lenb) = zlib_lenb(lf, df, &header);
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
    Block { last, body }
}

/// `data` compressed as zlib 1.3.1 does with parameters `p` in one `deflate(Z_FINISH)` call,
/// as a raw DEFLATE stream (without the zlib header and Adler-32); `None` for invalid
/// parameters.
pub fn compress(data: &[u8], p: Params) -> Option<Vec<u8>> {
    if !p.valid() {
        return None;
    }
    let mut e = Emulator::new(data, p);
    let limit = p.block_tokens();
    let mut w = crate::write::Writer::new(data.len() / 4 + 64);
    let mut start = 0;
    loop {
        let mut tokens = Vec::with_capacity(limit.min(data.len() - start + 1));
        let last = e.run(&mut tokens, limit);
        let (lf, df) = freqs(&tokens);
        let end = start + tokens.iter().map(|t| t.span()).sum::<usize>();
        let b = flush_block(&e, start, end, tokens, &lf, &df, last);
        let toks = match &b.body {
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => &tokens[..],
            Body::Stored { .. } => &[],
        };
        w.block(&b, toks, data).ok()?;
        start = end;
        if last {
            return w.finish(0, &[]).ok();
        }
    }
}

/// An emulation that has followed a stream's first tokens (in [`crate::detect`]): what it
/// mispredicted on the way, and the emulator after them, to carry on from.
pub struct Followed<'a> {
    /// The mispredictions: (position, predicted token).
    pub log: Vec<(usize, Token)>,
    /// Where the next token starts (before any stored block there): the emulator is there.
    pub pos: usize,
    pub emu: Emulator<'a>,
}

/// Several lazy-parser emulations (levels 4-9, strategies default, filtered or fixed, the same
/// windowBits and memLevel) run at once against the actual tokens, each predicting and missing
/// exactly as its own [`Emulator`] would. Driven by the same actual tokens, they chain the same
/// positions and keep the same window, so that much can be shared.
pub struct MultiSlow<'a> {
    e: Engine<'a>,
}

impl<'a> MultiSlow<'a> {
    /// All `params` must be lazy-parser ones with the same windowBits and memLevel.
    pub fn new(input: &'a [u8], params: &[Params]) -> Self {
        assert!(!params.is_empty());
        assert!(
            params
                .iter()
                .all(|&p| p.valid() && kind_of(p) == Kind::Lazy)
        );
        assert!(
            params
                .iter()
                .all(|p| (p.wbits, p.mem) == (params[0].wbits, params[0].mem))
        );
        MultiSlow {
            e: Engine::new(input, params, true),
        }
    }

    /// The lanes still alive: (index, parameters, misses so far).
    pub fn lanes(&self) -> impl Iterator<Item = (usize, Params, usize)> + '_ {
        self.e
            .lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.alive)
            .map(|(i, l)| (i, l.p, l.misses))
    }

    /// Stops following lane `lane`.
    pub fn kill(&mut self, lane: usize) {
        self.e.lanes[lane].alive = false;
    }

    pub fn any_alive(&self) -> bool {
        self.e.lanes.iter().any(|l| l.alive)
    }

    /// Lane `k` as a single emulation that has followed the tokens so far, at data position
    /// `pos` (where the next token starts), with its log of mispredictions.
    pub fn into_followed(self, k: usize, pos: usize) -> Followed<'a> {
        let mut e = self.e;
        let mut lane = e.lanes.swap_remove(k);
        lane.alive = true;
        let log = std::mem::take(&mut lane.log);
        let p = lane.p;
        e.lanes = vec![lane];
        e.track = false;
        e.jobs.clear();
        Followed {
            log,
            pos,
            emu: Emulator { p, e },
        }
    }

    /// The data position of the next token that the lanes predict, or None at the end (all
    /// alive lanes predict at the same position, or the emulation is over).
    pub fn predict(&mut self) -> Option<usize> {
        let rep = self.e.lanes.iter().position(|l| l.alive)?;
        if self.e.lanes[rep].pending.is_none() && !self.e.finished {
            self.e.step();
        }
        let p = self.e.lanes[rep].pending?;
        Some(self.e.win.origin + p.at)
    }

    /// Moves every alive lane on with the actual token, counting and logging its miss where it
    /// predicted otherwise; false where the emulation cannot follow it.
    pub fn commit(&mut self, actual: Token) -> bool {
        self.e.commit(actual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        let mut next = move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 33) as usize
        };
        let mut v: Vec<u8> = vec![];
        while v.len() < n {
            match next() % 4 {
                0 => v.push((next() % 7) as u8 + b'a'),
                1 if v.len() > 10 => {
                    let d = 1 + next() % v.len().min(3000);
                    let l = 3 + next() % 40;
                    for _ in 0..l {
                        v.push(v[v.len() - d]);
                    }
                }
                2 => v.extend(std::iter::repeat_n((next() % 3) as u8, next() % 300)),
                _ => v.push(next() as u8),
            }
        }
        v.truncate(n);
        v
    }

    fn all_params() -> Vec<Params> {
        let mut v = vec![];
        for level in [1, 3, 4, 6, 9] {
            for strategy in Strategy::ALL {
                for (wbits, mem) in [(15, 8), (9, 1), (10, 3)] {
                    v.push(Params {
                        level,
                        strategy,
                        wbits,
                        mem,
                    });
                }
            }
        }
        v
    }

    /// Runs `e` freely, giving its tokens.
    fn run(e: &mut Emulator, limit: usize) -> Vec<(usize, Token)> {
        let mut out = vec![];
        while out.len() < limit {
            let Some((at, t)) = e.predict() else { break };
            assert!(e.commit(t));
            out.push((at, t));
        }
        out
    }

    #[test]
    fn free_run_covers_input() {
        for seed in 0..3 {
            let data = sample(30_000, seed);
            for p in all_params() {
                let mut e = Emulator::new(&data, p);
                let mut pos = 0;
                for (at, t) in run(&mut e, usize::MAX) {
                    assert_eq!(at, pos, "{p}");
                    pos += t.span();
                }
                assert_eq!(pos, data.len(), "{p}");
                assert_eq!(e.predict(), None);
            }
        }
        let mut e = Emulator::new(&[], Params::default());
        assert_eq!(e.predict(), None);
    }

    #[test]
    fn resume_continues_the_run() {
        let data = sample(12_000, 7);
        for p in all_params().into_iter().filter(|p| p.resumable()) {
            let mut e = Emulator::new(&data, p);
            let mut n = 0;
            loop {
                if let Some(cp) = e.checkpoint()
                    && let Some(mut r) = Emulator::resume(&data, p, &cp)
                {
                    let mut orig = e.clone();
                    assert_eq!(
                        run(&mut r, 60),
                        run(&mut orig, 60),
                        "{p} after {n} tokens {cp:?}"
                    );
                }
                let Some((_, t)) = e.predict() else { break };
                assert!(e.commit(t));
                n += 1;
            }
        }
    }

    #[test]
    fn resume_survives_arbitrary_checkpoints() {
        let data = sample(9_000, 3);
        let mut s = 12345u64;
        let mut next = move |m: usize| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 33) as usize % m
        };
        for p in all_params() {
            let w = 1usize << p.wbits;
            for _ in 0..300 {
                let origin = next(2) * w;
                let cursor = origin + next(2 * w + 1);
                let cp = Checkpoint {
                    cursor,
                    origin,
                    fed: cursor + next(3000),
                    zero_mark: next(2 * w + 1),
                    held: next(2) == 1,
                    length: 2 + next(270),
                    source: cursor.saturating_sub(next(w + 1)),
                };
                if let Some(mut e) = Emulator::resume(&data, p, &cp) {
                    let mut k = 0;
                    while let Some((_, t)) = e.predict() {
                        if !e.commit(t) || k > 50 {
                            break;
                        }
                        k += 1;
                    }
                }
            }
        }
    }

    #[test]
    fn multi_lanes_match_single_emulators() {
        let data = sample(20_000, 11);
        let lanes: Vec<Params> = (4..=9)
            .flat_map(|level| {
                [Strategy::Default, Strategy::Filtered].map(|strategy| Params {
                    level,
                    strategy,
                    wbits: 10,
                    mem: 8,
                })
            })
            .collect();
        // Follow tokens of a level-6 run, so that other lanes miss.
        let mut driver = Emulator::new(&data, lanes[4]);
        let tokens: Vec<Token> = run(&mut driver, usize::MAX)
            .into_iter()
            .map(|x| x.1)
            .collect();
        let mut m = MultiSlow::new(&data, &lanes);
        let mut singles: Vec<_> = lanes
            .iter()
            .map(|&p| (Emulator::new(&data, p), 0))
            .collect();
        let mut pos = 0;
        for &t in &tokens {
            assert_eq!(m.predict(), Some(pos));
            for (e, misses) in &mut singles {
                let (at, pt) = e.predict().unwrap();
                assert_eq!(at, pos);
                *misses += (pt != t) as usize;
                assert!(e.commit(t));
            }
            assert!(m.commit(t));
            pos += t.span();
        }
        for (k, p, misses) in m.lanes() {
            assert_eq!((p, misses), (lanes[k], singles[k].1));
        }
    }
}
