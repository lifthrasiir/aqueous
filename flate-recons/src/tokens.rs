//! Token prediction by a fixed rule over a small hash table, and the sparse corrections of
//! its misses.
//!
//! A [`Rule`] keeps one table of recent positions by a hash of their first 3 or 4 bytes
//! (a bucket of a few ways per hash, most recent first) and predicts the longest match among
//! a bucket's entries, greedily or one step lazily. The table holds what the rule inserted:
//! every position, or token starts (and the inside of short matches). A range of tokens can
//! be coded or rebuilt on its own once its starting position is known.
//!
//! Tokens fall in two classes: those where the table offers a candidate match, and the
//! others (where a literal is predicted). Corrections are the misses of each class, coded as
//! the number of hits before them (gap), what the actual token is relative to the
//! prediction (kind, length), and its distance as an entry of the bucket or verbatim.

use super::code::{self, Counts};
use super::table::{Cands, ChainTable, NoTable, Search, Table, common};
use crate::bits::{BitReader, BitWriter};
use crate::deflate::Token;
use crate::matchfind::{MAX_MATCH, WINDOW};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// The longest match in the bucket.
    Search,
    /// Literals only (Huffman-only encoders).
    Literals,
    /// Runs at distance 1 only (RLE strategies).
    Runs,
    /// The closest longest match among the first `depth` candidates of an exact chain of
    /// every position, stopping at `nice` (see [`crate::matchfind`]); distances are ranked.
    Chain,
}

/// A deterministic token predictor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rule {
    pub kind: Kind,
    /// Positions are hashed by this many bytes (3 or 4).
    pub key: u8,
    /// Entries per bucket (a power of two up to [`MAX_WAYS`]).
    pub ways: u8,
    /// A second table keyed by this many bytes (5 to 8; 0 for none), for long matches the
    /// first one's few entries miss.
    pub long: u8,
    /// Distances the tables do not have are coded by their rank among the earlier positions
    /// matching as long (walking exact chains at the misses only), rather than verbatim.
    pub rank: bool,
    /// Which positions are inserted: token starts, and the positions inside matches up to
    /// this long ([`INNER_ALL`]: every position).
    pub inner: u16,
    /// Lazy evaluation: a match shorter than this gives way to a literal when the next
    /// position has one longer by more than `margin`; 0 for greedy parsing.
    pub lazy: u16,
    pub margin: u8,
    /// Shortest match emitted.
    pub min_len: u8,
    /// Matches up to `far_len` long and farther than `far_dist` are not emitted (zlib's
    /// TOO_FAR); `far_len` 0 for none.
    pub far_len: u8,
    pub far_dist: u16,
    /// Farthest distance used.
    pub window: u16,
    /// [`Kind::Chain`]: the search depth and nice length, as indices into [`DEPTHS`] and
    /// [`NICES`].
    pub depth: u8,
    pub nice: u8,
}

pub const MAX_WAYS: u8 = 8;
/// Search depths of [`Kind::Chain`], by [`Rule::depth`].
pub const DEPTHS: [u16; 16] = [
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768,
];
/// Nice lengths of [`Kind::Chain`], by [`Rule::nice`].
pub const NICES: [u16; 8] = [8, 16, 24, 32, 64, 128, 192, 258];
/// [`Rule::inner`]: every position is inserted.
pub const INNER_ALL: u16 = MAX_MATCH as u16 + 1;

impl Default for Rule {
    fn default() -> Self {
        Rule {
            kind: Kind::Search,
            key: 3,
            ways: 1,
            long: 0,
            rank: false,
            inner: INNER_ALL,
            lazy: 0,
            margin: 0,
            min_len: 3,
            far_len: 0,
            far_dist: 4096,
            window: WINDOW as u16,
            depth: 0,
            nice: 0,
        }
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            Kind::Literals => return f.write_str("literals"),
            Kind::Runs => return write!(f, "runs,min={}", self.min_len),
            Kind::Chain => {
                return write!(
                    f,
                    "chain,depth={},nice={}",
                    DEPTHS[self.depth as usize], NICES[self.nice as usize]
                );
            }
            Kind::Search => {}
        }
        write!(f, "key={},ways={}", self.key, self.ways)?;
        if self.long > 0 {
            write!(f, ",long={}", self.long)?;
        }
        if self.rank {
            f.write_str(",rank")?;
        }
        match self.inner {
            INNER_ALL => {}
            0 => f.write_str(",starts")?,
            n => write!(f, ",inner={n}")?,
        }
        if self.lazy > 0 {
            write!(f, ",lazy={}", self.lazy)?;
        }
        if self.margin > 0 {
            write!(f, ",margin={}", self.margin)?;
        }
        if self.min_len != 3 {
            write!(f, ",min={}", self.min_len)?;
        }
        if self.far_len > 0 {
            f.write_str(",far")?;
            if self.far_dist != 4096 {
                write!(f, "={}", self.far_dist)?;
            }
            if self.far_len != 3 {
                write!(f, ",farlen={}", self.far_len)?;
            }
        }
        if self.window as usize != WINDOW {
            write!(f, ",window={}", self.window)?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Rule {
    type Err = Error;
    fn from_str(s: &str) -> Result<Rule> {
        let bad = || Error::Param(format!("bad rule {s:?}"));
        let mut r = Rule::default();
        for kv in s.split(',') {
            let num = |v: &str| v.parse::<u16>().map_err(|_| bad());
            match kv.split_once('=') {
                None if kv == "literals" => r.kind = Kind::Literals,
                None if kv == "runs" => r.kind = Kind::Runs,
                None if kv == "chain" => r.kind = Kind::Chain,
                Some(("depth", v)) => {
                    let v = num(v)?;
                    r.depth = DEPTHS.iter().position(|&x| x == v).ok_or_else(bad)? as u8
                }
                Some(("nice", v)) => {
                    let v = num(v)?;
                    r.nice = NICES.iter().position(|&x| x == v).ok_or_else(bad)? as u8
                }
                None if kv == "far" => r.far_len = r.far_len.max(3),
                None if kv == "starts" => r.inner = 0,
                None if kv == "rank" => r.rank = true,
                Some(("key", v)) => r.key = num(v)? as u8,
                Some(("ways", v)) => r.ways = num(v)? as u8,
                Some(("long", v)) => r.long = num(v)? as u8,
                Some(("inner", v)) => r.inner = num(v)?,
                Some(("lazy", v)) => r.lazy = num(v)?,
                Some(("margin", v)) => r.margin = num(v)? as u8,
                Some(("min", v)) => r.min_len = num(v)? as u8,
                Some(("far", v)) => {
                    r.far_len = r.far_len.max(3);
                    r.far_dist = num(v)?;
                }
                Some(("farlen", v)) => r.far_len = num(v)? as u8,
                Some(("window", v)) => r.window = num(v)?,
                _ => return Err(bad()),
            }
        }
        r.check()?;
        Ok(r)
    }
}

impl Rule {
    pub fn check(&self) -> Result<()> {
        let ok = (self.depth as usize) < DEPTHS.len()
            && (self.nice as usize) < NICES.len()
            && (3..=4).contains(&self.key)
            && (1..=MAX_WAYS).contains(&self.ways)
            && self.ways.is_power_of_two()
            && (self.long == 0 || (5..=8).contains(&self.long))
            && self.inner <= INNER_ALL
            && self.lazy <= MAX_MATCH as u16 + 1
            && self.min_len >= 3
            && (self.far_len == 0 || self.far_len >= 3)
            && (1..=WINDOW as u16).contains(&self.window);
        if ok {
            Ok(())
        } else {
            Err(Error::Param(format!("rule out of range: {self:?}")))
        }
    }

    /// Writes the rule: its kind in 2 bits, then for [`Kind::Chain`] the depth and nice
    /// indices (4 and 3 bits), else every other field.
    pub fn write(&self, w: &mut BitWriter) {
        w.bits(2, self.kind as u32);
        if self.kind == Kind::Chain {
            w.bits(4, self.depth as u32);
            w.bits(3, self.nice as u32);
            return;
        }
        w.bits(1, self.key as u32 - 3);
        w.bits(3, self.ways as u32 - 1);
        w.bits(4, self.long as u32);
        w.bits(1, self.rank as u32);
        code::put_gamma(w, self.inner as u64);
        code::put_gamma(w, self.lazy as u64);
        code::put_gamma(w, self.margin as u64);
        code::put_gamma(w, self.min_len as u64 - 3);
        code::put_gamma(w, self.far_len as u64);
        code::put_gamma(w, self.far_dist as u64);
        code::put_gamma(w, self.window as u64 - 1);
    }

    pub fn read(r: &mut BitReader) -> Result<Rule> {
        let kind = match r.bits(2)? {
            0 => Kind::Search,
            1 => Kind::Literals,
            2 => Kind::Runs,
            _ => {
                return Ok(Rule {
                    kind: Kind::Chain,
                    depth: r.bits(4)? as u8,
                    nice: r.bits(3)? as u8,
                    ..Rule::default()
                });
            }
        };
        let g = |r: &mut BitReader, base: usize, max: usize| -> Result<u16> {
            Ok((code::get_usize(r, max)? + base) as u16)
        };
        let rule = Rule {
            kind,
            key: r.bits(1)? as u8 + 3,
            ways: r.bits(3)? as u8 + 1,
            long: r.bits(4)? as u8,
            rank: r.bits(1)? == 1,
            inner: g(r, 0, INNER_ALL as usize)?,
            lazy: g(r, 0, MAX_MATCH + 1)?,
            margin: g(r, 0, 255)? as u8,
            min_len: g(r, 3, u8::MAX as usize - 3)? as u8,
            far_len: g(r, 0, u8::MAX as usize)? as u8,
            far_dist: g(r, 0, u16::MAX as usize)?,
            window: g(r, 1, WINDOW - 1)?,
            depth: 0,
            nice: 0,
        };
        rule.check().map_err(|_| Error::Corrupt)?;
        Ok(rule)
    }

    /// Estimated time of coding with this rule, in nanoseconds per byte of data (measured on
    /// one core of an M4 over the held-out samples, with `flate-recons-train speed`).
    pub fn ns_per_byte(&self) -> f64 {
        match self.kind {
            Kind::Literals => return 2.2,
            Kind::Runs => return 3.2,
            // (Not measured: it depends on the data far more than the others.)
            Kind::Chain => return 30.0,
            Kind::Search => {}
        }
        let ways = match self.ways {
            1 => 3.6,
            2 => 4.8,
            4 => 5.3,
            _ => 7.0,
        };
        // Ranking links every position by 3 bytes on its own unless the table does.
        let own_chain = self.key != 3 || self.inner != INNER_ALL;
        let rank = match (self.rank, own_chain) {
            (false, _) => 0.0,
            (true, false) => 0.8,
            (true, true) => 1.4,
        };
        ways + rank
            + if self.long > 0 { 0.7 } else { 0.0 }
            + if self.lazy > 0 { 0.4 } else { 0.0 }
            + if self.key == 4 { -0.4 } else { 0.0 }
            + if self.inner != INNER_ALL { -0.5 } else { 0.0 }
    }

    /// The predicted token at `p` and its class (1 if the table offered a candidate).
    #[inline(always)]
    pub fn predict<T: Search>(&self, t: &mut T, d: &[u8], p: usize) -> (Token, usize) {
        let lit = Token::Lit(d[p]);
        match self.kind {
            Kind::Literals => (lit, 0),
            Kind::Runs => {
                let max = (d.len() - p).min(MAX_MATCH);
                if p == 0 || max < 3 {
                    return (lit, 0);
                }
                let l = common(d, p - 1, p, max);
                if l < 3 {
                    (lit, 0)
                } else if l < self.min_len as usize {
                    (lit, 1)
                } else {
                    (
                        Token::Match {
                            len: l as u16,
                            dist: 1,
                            alt258: false,
                        },
                        1,
                    )
                }
            }
            Kind::Chain => {
                let (len, dist) = t.search(d, p, WINDOW);
                if len < 3 {
                    return (lit, 0);
                }
                (
                    Token::Match {
                        len: len as u16,
                        dist: dist as u16,
                        alt258: false,
                    },
                    1,
                )
            }
            Kind::Search => {
                let window = self.window as usize;
                let (len, dist) = t.search(d, p, window);
                if len < 3 {
                    return (lit, 0);
                }
                if len < self.min_len as usize
                    || (len <= self.far_len as usize && dist > self.far_dist as usize)
                {
                    return (lit, 1);
                }
                if len < self.lazy as usize && p + 1 < d.len() {
                    t.token_start(d, p);
                    if t.search(d, p + 1, window).0 > len + self.margin as usize {
                        return (lit, 1);
                    }
                }
                (
                    Token::Match {
                        len: len as u16,
                        dist: dist as u16,
                        alt258: false,
                    },
                    1,
                )
            }
        }
    }
}

/// Fields of a correction, each with its own code.
pub const GAP: usize = 0; // and 1: per class
pub const KIND: usize = 2; // and 3: per class
/// Length of a match where a literal was predicted, minus 3.
pub const LEN_FRESH: usize = 4;
/// Length of a match relative to the predicted match's, zigzag coded.
pub const LEN_DELTA: usize = 5;
/// Distance as 1 + the index of the bucket entry it points to, 0 if none.
pub const DIST_IDX: usize = 6;
/// Distance - 1, when no entry has it (nor a rank).
pub const DIST_RAW: usize = 7;
/// Rank of the distance among earlier positions matching as long, plus one; 0 if too far.
pub const DIST_RANK: usize = 8;
pub const FIELDS: usize = 9;

/// Candidates walked when ranking a distance.
const RANK_LIMIT: usize = 1024;

/// Kinds of corrections.
pub(crate) const K_LIT: u32 = 0;
pub(crate) const K_FRESH: u32 = 1;
pub(crate) const K_DELTA: u32 = 2;
pub(crate) const K_ALT258: u32 = 3;

fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

pub(crate) fn unzigzag(v: u32) -> i32 {
    (v >> 1) as i32 ^ -((v & 1) as i32)
}

/// Where a run of tokens starts. Stored blocks interrupt the tokens: `skips` lists, by
/// global token index, how many bytes they cover before that token.
pub struct Range<'a> {
    pub first: usize,
    pub count: usize,
    /// Data position before the skips at `first`.
    pub pos: usize,
    /// No token goes past this position (the next range's start).
    pub end: usize,
    pub skips: &'a [(usize, usize)],
}

/// Receives every token: given the prediction and its class, returns the actual token.
pub trait Corr {
    fn token(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        actual: Option<Token>,
    ) -> Result<Token>;

    /// A prediction to use instead of the rule's at `p` (coded as class 2), from a
    /// predictor of the coder's own.
    #[inline(always)]
    fn predict(&mut self, _d: &[u8], _p: usize) -> Option<Token> {
        None
    }

    /// The token at `p` once known (after every prediction, the rule's or the coder's).
    #[inline(always)]
    fn commit(&mut self, _p: usize, _tok: Token) {}

    /// The data up to `end` is in stored blocks.
    #[inline(always)]
    fn skip(&mut self, _end: usize) {}

    /// Whether [`Corr::next_len`] is wanted.
    #[inline(always)]
    fn lookahead(&self) -> bool {
        false
    }

    /// Before a token the rule predicts to be a match, the length its table finds at the
    /// next position (without inserting anything).
    #[inline(always)]
    fn next_len(&mut self, _len: usize) {}
}

/// Codes the tokens of `range` under `rule`, both ways: `actual` gives them when
/// deconstructing, and when reconstructing they are appended to `out`. Returns the position
/// after the last token.
pub fn code_range<C: Corr>(
    c: &mut C,
    d: &[u8],
    rule: Rule,
    range: &Range,
    actual: Option<&[Token]>,
    out: Option<&mut Vec<Token>>,
) -> Result<usize> {
    match rule.kind {
        Kind::Search => {}
        Kind::Chain => {
            let t = ChainTable::new(d, range.pos, range.end, rule.depth, rule.nice);
            return code_with(c, t, d, rule, range, actual, out);
        }
        _ => return code_with(c, NoTable, d, rule, range, actual, out),
    }
    let inner = (rule.inner != INNER_ALL).then_some(rule.inner as usize);
    macro_rules! go {
        ($w:literal, $k:literal, $l:literal) => {
            code_with(
                c,
                Table::<$w, $k, $l>::new(inner, range.pos, range.end, rule.rank),
                d,
                rule,
                range,
                actual,
                out,
            )
        };
    }
    macro_rules! by_long {
        ($w:literal, $k:literal) => {
            match rule.long {
                5 => go!($w, $k, 5),
                6 => go!($w, $k, 6),
                7 => go!($w, $k, 7),
                8 => go!($w, $k, 8),
                _ => go!($w, $k, 0),
            }
        };
    }
    macro_rules! by_key {
        ($w:literal) => {
            if rule.key == 4 {
                by_long!($w, 4)
            } else {
                by_long!($w, 3)
            }
        };
    }
    match rule.ways {
        1 => by_key!(1),
        2 => by_key!(2),
        4 => by_key!(4),
        _ => by_key!(8),
    }
}

#[inline(always)]
fn code_with<C: Corr, T: Search>(
    c: &mut C,
    mut t: T,
    d: &[u8],
    rule: Rule,
    range: &Range,
    actual: Option<&[Token]>,
    mut out: Option<&mut Vec<Token>>,
) -> Result<usize> {
    let n = d.len().min(range.end);
    let mut p = range.pos;
    let skip0 = range.skips.partition_point(|s| s.0 < range.first);
    let mut skips = range.skips[skip0..].iter().peekable();
    for k in 0..range.count {
        while let Some(&&(at, len)) = skips.peek()
            && at == range.first + k
        {
            p += len;
            skips.next();
            c.skip(p);
        }
        if p >= n {
            return Err(Error::Corrupt);
        }
        let (pred, class) = match c.predict(d, p) {
            Some(t) => (t, 2),
            None => {
                let (pred, class) = rule.predict(&mut t, d, p);
                if c.lookahead()
                    && let Token::Match { .. } = pred
                {
                    let next = if p + 1 < n {
                        t.peek(d, p + 1, rule.window as usize).0
                    } else {
                        0
                    };
                    c.next_len(next);
                }
                (pred, class)
            }
        };
        let tok = c.token(&mut t, d, p, pred, class, actual.map(|a| a[k]))?;
        c.commit(p, tok);
        t.token(d, p, tok.span());
        p += tok.span();
        if let Some(o) = out.as_deref_mut() {
            o.push(tok);
        }
    }
    Ok(p)
}

/// The distance of a match at `p` as a bucket entry (1 + index) or 0.
fn dist_index(t: &mut dyn Cands, d: &[u8], p: usize, dist: usize) -> u32 {
    t.index_of(d, p, dist).map_or(0, |i| i as u32 + 1)
}

/// Records the corrections while deconstructing.
pub struct Recorder {
    /// The payload fields of every miss, (field, value).
    vals: Vec<(u8, u32)>,
    /// Per miss: class, gap before it, end of its payload in `vals`.
    log: Vec<(u8, u32, u32)>,
    pub counts: [Counts; FIELDS],
    /// Per class.
    pub misses: [usize; 2],
    hits: [u32; 2],
    /// Coding gives up ([`Error::Corrupt`]) after this many misses.
    pub limit: usize,
    /// Only the counts are kept, not the values (for estimating the cost).
    pub count_only: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder {
            vals: vec![],
            log: vec![],
            counts: Default::default(),
            misses: [0; 2],
            hits: [0; 2],
            limit: usize::MAX,
            count_only: false,
        }
    }
}

impl Recorder {
    #[inline]
    fn put(&mut self, f: usize, v: u32) {
        if !self.count_only {
            self.vals.push((f as u8, v));
        }
        self.counts[f].add(v);
    }

    /// The values coded with the given models, in the order [`Player`] reads them: the first
    /// gap of each class, then every miss's payload followed by the gap to the next miss of
    /// its class.
    pub fn write(&self, models: &[code::Model; FIELDS]) -> Vec<u8> {
        let mut w = code::Writer::default();
        // The next miss of the same class, for every miss.
        let mut next = vec![usize::MAX; self.log.len()];
        let mut last = [usize::MAX; 2];
        for (i, &(c, ..)) in self.log.iter().enumerate().rev() {
            next[i] = last[c as usize];
            last[c as usize] = i;
        }
        for i in last.into_iter().filter(|&i| i != usize::MAX) {
            let (c, g, _) = self.log[i];
            w.put(&models[GAP + c as usize], g);
        }
        let mut start = 0;
        for (i, &(_, _, end)) in self.log.iter().enumerate() {
            for &(f, v) in &self.vals[start..end as usize] {
                w.put(&models[f as usize], v);
            }
            start = end as usize;
            if let Some(&(c, g, _)) = self.log.get(next[i]) {
                w.put(&models[GAP + c as usize], g);
            }
        }
        w.finish()
    }

    /// Estimated cost in bits with codes of its own.
    pub fn cost(&self) -> f64 {
        self.counts.iter().map(Counts::cost).sum::<f64>()
    }
}

impl Corr for Recorder {
    #[inline(always)]
    fn token(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        actual: Option<Token>,
    ) -> Result<Token> {
        let a = actual.ok_or(Error::Inconsistent)?;
        if a == pred {
            self.hits[class] += 1;
            return Ok(a);
        }
        self.miss(t, d, p, pred, class, a)
    }
}

impl Recorder {
    /// Records a miss (kept out of line: hits are the common case).
    #[inline(never)]
    fn miss(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        a: Token,
    ) -> Result<Token> {
        self.counts[GAP + class].add(self.hits[class]);
        let gap = self.hits[class];
        self.hits[class] = 0;
        self.misses[class] += 1;
        if self.misses[0] + self.misses[1] > self.limit {
            return Err(Error::Corrupt);
        }
        self.payload(t, d, p, pred, class, a);
        if !self.count_only {
            self.log.push((class as u8, gap, self.vals.len() as u32));
        }
        Ok(a)
    }

    #[inline]
    fn payload(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        a: Token,
    ) {
        payload(t, d, p, pred, class, a, |f, v| self.put(f, v));
    }
}

/// The payload fields of a miss (`a` where `pred` was predicted), as (field, value) in
/// coding order.
#[inline]
pub fn payload(
    t: &mut dyn Cands,
    d: &[u8],
    p: usize,
    pred: Token,
    class: usize,
    a: Token,
    mut put: impl FnMut(usize, u32),
) {
    let Token::Match { len, dist, alt258 } = a else {
        put(KIND + class, K_LIT);
        return;
    };
    match pred {
        _ if alt258 => put(KIND + class, K_ALT258),
        Token::Lit(_) => {
            put(KIND + class, K_FRESH);
            put(LEN_FRESH, len as u32 - 3);
        }
        Token::Match { len: pl, .. } => {
            put(KIND + class, K_DELTA);
            put(LEN_DELTA, zigzag(len as i32 - pl as i32));
        }
    }
    let i = dist_index(t, d, p, dist as usize);
    put(DIST_IDX, i);
    if i == 0 {
        if t.ranked() {
            let r = t
                .rank(d, p, len as usize, dist as usize, RANK_LIMIT)
                .map_or(0, |r| r as u32 + 1);
            put(DIST_RANK, r);
            if r > 0 {
                return;
            }
        }
        put(DIST_RAW, dist as u32 - 1);
    }
}

/// Replays the corrections while reconstructing.
pub struct Player<'a> {
    r: code::Reader<'a>,
    models: &'a [code::Model; FIELDS],
    /// Per class: misses left, and hits before the next one.
    misses: [usize; 2],
    gap: [u32; 2],
}

impl<'a> Player<'a> {
    pub fn new(
        buf: &'a [u8],
        models: &'a [code::Model; FIELDS],
        misses: [usize; 2],
    ) -> Result<Self> {
        let mut r = code::Reader::new(buf)?;
        let mut gap = [0; 2];
        for c in 0..2 {
            if misses[c] > 0 {
                gap[c] = r.get(&models[GAP + c])?;
            }
        }
        Ok(Player {
            r,
            models,
            misses,
            gap,
        })
    }

    /// Whether every correction has been used.
    pub fn done(&self) -> bool {
        self.misses == [0, 0]
    }
}

impl Corr for Player<'_> {
    #[inline(always)]
    fn token(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        _: Option<Token>,
    ) -> Result<Token> {
        if self.misses[class] == 0 {
            return Ok(pred);
        }
        if self.gap[class] > 0 {
            self.gap[class] -= 1;
            return Ok(pred);
        }
        self.miss(t, d, p, pred, class)
    }
}

impl Player<'_> {
    /// Reads a miss's correction (kept out of line: hits are the common case).
    #[inline(never)]
    fn miss(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
    ) -> Result<Token> {
        let (r, models) = (&mut self.r, self.models);
        let tok = read_miss(t, d, p, pred, class, |f| r.get(&models[f]))?;
        self.misses[class] -= 1;
        if self.misses[class] > 0 {
            self.gap[class] = self.r.get(&self.models[GAP + class])?;
        }
        Ok(tok)
    }
}

/// A miss's token (where `pred` was predicted) from its payload fields, read by `get` in
/// the order [`payload`] gives them.
#[inline]
pub fn read_miss(
    t: &mut dyn Cands,
    d: &[u8],
    p: usize,
    pred: Token,
    class: usize,
    mut get: impl FnMut(usize) -> Result<u32>,
) -> Result<Token> {
    let maxl = (d.len() - p).min(MAX_MATCH);
    let kind = get(KIND + class)?;
    let (len, alt258) = match (kind, pred) {
        (K_LIT, Token::Match { .. }) => (0, false),
        (K_FRESH, Token::Lit(_)) => (3 + get(LEN_FRESH)? as usize, false),
        (K_DELTA, Token::Match { len: pl, .. }) => {
            let l = pl as i64 + unzigzag(get(LEN_DELTA)?) as i64;
            (usize::try_from(l).map_err(|_| Error::Corrupt)?, false)
        }
        (K_ALT258, _) => (MAX_MATCH, true),
        _ => return Err(Error::Corrupt),
    };
    if len == 0 {
        return Ok(Token::Lit(d[p]));
    }
    if !(3..=maxl).contains(&len) {
        return Err(Error::Corrupt);
    }
    let dist = match get(DIST_IDX)? {
        0 => match if t.ranked() { get(DIST_RANK)? } else { 0 } {
            0 => get(DIST_RAW)? as usize + 1,
            k => t
                .select(d, p, len, k as usize - 1, RANK_LIMIT)
                .ok_or(Error::Corrupt)?,
        },
        i => {
            let e = t.entries(d, p);
            p - *e.get(i as usize - 1).ok_or(Error::Corrupt)?
        }
    };
    if !valid(d, p, len, dist) {
        return Err(Error::Corrupt);
    }
    Ok(Token::Match {
        len: len as u16,
        dist: dist as u16,
        alt258,
    })
}

/// Whether `d[p - dist..]` matches `d[p..]` for `len` bytes, within DEFLATE's window.
fn valid(d: &[u8], p: usize, len: usize, dist: usize) -> bool {
    dist >= 1
        && dist <= p
        && dist <= WINDOW
        && p + len <= d.len()
        && common(d, p - dist, p, len) == len
}
