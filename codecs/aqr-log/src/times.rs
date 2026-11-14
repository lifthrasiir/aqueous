//! The encoder's side of times: timestamps found in a line, as spans of its bytes with an
//! aqr-num pattern and the integer it prints (FORMAT.md, section 5).

use std::collections::HashMap;

use aqr_num::time::{self, MONTHS, WEEKDAYS};

/// A timestamp in a line: its bytes `start..end`, its pattern (an index of [`Times::pattern`])
/// and its integer.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub pat: u32,
    pub v: i64,
}

/// How the segment writes the fraction of a second after `H:M:S`, from its sample: always the
/// same digits, or any number of them up to the most seen with the trailing zeros dropped (and
/// none where it is zero), or none seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frac {
    No,
    Fixed(u8, u8),
    Trimmed(u8, u8),
}

/// What the encoder knows of the times of a segment: how it writes fractions and dates, the
/// patterns it has used, and the spans of the last line it looked at.
pub struct Times {
    frac: Frac,
    /// Dates `dd/dd/yyyy` are month first (the sample has a second field over 12 and no first).
    mdy: bool,
    pats: Vec<Vec<u8>>,
    ids: HashMap<Vec<u8>, u32>,
    pub spans: Vec<Span>,
    pat: Vec<u8>,
    out: Vec<u8>,
}

fn digits(b: &[u8], at: usize, n: usize) -> bool {
    b.get(at..at + n).is_some_and(|d| d.iter().all(u8::is_ascii_digit))
}

fn num(b: &[u8], at: usize, n: usize) -> u32 {
    b[at..at + n].iter().fold(0, |a, &c| a * 10 + (c - b'0') as u32)
}

impl Times {
    /// The times of a segment of which `sample` (lines) is the start.
    pub fn new(sample: &[u8]) -> Times {
        let (mut seen, mut points) = (0u16, [0usize; 2]);
        let (mut first, mut second) = (0u32, 0u32);
        for line in sample.split(|&b| b == b'\n') {
            let mut at = 0;
            while let Some(i) = memchr::memchr(b':', &line[at..]) {
                let p = at + i;
                at = p + 1;
                // dd:dd:dd[.,]digits
                let hms = p >= 2 && digits(line, p - 2, 2) && digits(line, p + 1, 2) && line.get(p + 3) == Some(&b':');
                if hms && digits(line, p + 4, 2) && matches!(line.get(p + 6), Some(b'.' | b',')) {
                    let k = line[p + 7..].iter().take_while(|b| b.is_ascii_digit()).count();
                    if (1..=9).contains(&k) {
                        seen |= 1 << k;
                        points[(line[p + 6] == b',') as usize] += 1;
                    }
                }
            }
            // dd/dd/dddd
            for (i, w) in line.windows(10).enumerate() {
                if w[2] == b'/'
                    && w[5] == b'/'
                    && digits(line, i, 2)
                    && digits(line, i + 3, 2)
                    && digits(line, i + 6, 4)
                {
                    first = first.max(num(line, i, 2));
                    second = second.max(num(line, i + 3, 2));
                }
            }
        }
        let point = if points[1] > points[0] { b',' } else { b'.' };
        let frac = match seen.count_ones() {
            0 => Frac::No,
            1 => Frac::Fixed(point, seen.trailing_zeros() as u8),
            _ => Frac::Trimmed(point, 15 - seen.leading_zeros() as u8),
        };
        Times {
            frac,
            mdy: first <= 12 && second > 12,
            pats: Vec::new(),
            ids: HashMap::new(),
            spans: Vec::new(),
            pat: Vec::new(),
            out: Vec::new(),
        }
    }

    /// The bytes of pattern `id`.
    pub fn pattern(&self, id: u32) -> &[u8] {
        &self.pats[id as usize]
    }

    /// Finds the timestamps of `line` into `self.spans`, left to right, none inside another.
    pub fn find(&mut self, line: &[u8]) {
        self.spans.clear();
        let mut i = 0;
        while i < line.len() {
            let c = line[i];
            let starts = c.is_ascii_digit() || c.is_ascii_uppercase();
            let boundary =
                i == 0 || !matches!(line[i - 1], b'.' | b':' | b'_' | b'+') && !line[i - 1].is_ascii_alphanumeric();
            if starts
                && boundary
                && let Some(sp) = self.at(line, i)
            {
                i = sp.end;
                self.spans.push(sp);
                continue;
            }
            i += 1;
        }
    }

    /// The timestamp starting at `b[i]`, by what the next bytes look like.
    fn at(&mut self, b: &[u8], i: usize) -> Option<Span> {
        let c = b[i];
        if c.is_ascii_digit() {
            if digits(b, i, 4) {
                let s = *b.get(i + 4)?;
                if matches!(s, b'-' | b'/' | b'.')
                    && b.get(i + 7) == Some(&s)
                    && digits(b, i + 5, 2)
                    && digits(b, i + 8, 2)
                {
                    return self.dated(b, i, &[b'Y', s, b'm', s, b'd'], 10);
                }
            } else if digits(b, i, 2) {
                let s = *b.get(i + 2)?;
                if s == b':' {
                    return self.times(b, i, b"", b'H');
                }
                if matches!(s, b'/' | b'.' | b'-') {
                    if digits(b, i + 3, 2) && b.get(i + 5) == Some(&s) && digits(b, i + 6, 4) {
                        let (x, y) = if self.mdy { (b'm', b'd') } else { (b'd', b'm') };
                        return self.dated(b, i, &[x, s, y, s, b'Y'], 10);
                    }
                    if b.get(i + 3..i + 6).is_some_and(|m| MONTHS.iter().any(|n| n[..] == *m))
                        && b.get(i + 6) == Some(&s)
                        && digits(b, i + 7, 4)
                    {
                        return self.dated(b, i, &[b'd', s, b'b', s, b'Y'], 11);
                    }
                }
            } else if b.get(i + 1) == Some(&b':') {
                return self.times(b, i, b"", b'h');
            }
            return None;
        }
        let w = b.get(i..i + 3)?;
        if MONTHS.iter().any(|n| n[..] == *w) && b.get(i + 3) == Some(&b' ') {
            // syslog: Dec 10 06:55:46, Dec  1 06:55:46
            for head in [&b"b e "[..], b"b D "] {
                if let Some(sp) = self.times(b, i, head, b'H') {
                    return Some(sp);
                }
            }
        } else if WEEKDAYS.iter().any(|n| n[..] == *w) {
            // ctime: Thu Jun 09 06:07:04 2005; RFC 5322: Thu, 09 Jun 2005 06:07:04
            for (head, tail) in [(&b"a b d "[..], &b" Y"[..]), (b"a b e ", b" Y"), (b"a, d b Y ", b"")] {
                if let Some(sp) = self.times_then(b, i, head, tail) {
                    return Some(sp);
                }
            }
        }
        None
    }

    /// A date of pattern `date` of `len` bytes at `b[i]`: with a time of day after it (a space,
    /// `T` or `:` between), or alone.
    fn dated(&mut self, b: &[u8], i: usize, date: &[u8], len: usize) -> Option<Span> {
        let j = *b.get(i + len)?;
        let joiner: &[u8] = match j {
            b' ' => b" ",
            b'T' => b"\\T",
            b':' => b":",
            _ => b"",
        };
        if !joiner.is_empty() {
            let mut head = date.to_vec();
            head.extend_from_slice(joiner);
            if let Some(sp) = self.times(b, i, &head, b'H') {
                return Some(sp);
            }
        }
        self.pat.clear();
        self.pat.extend_from_slice(date);
        self.check(b, i)
    }

    /// `head` and a time of day (`hour` is its pattern's hour), the longest that is printed
    /// back: seconds with the segment's fraction, as many digits as there are, none, and
    /// then minutes only.
    fn times(&mut self, b: &[u8], i: usize, head: &[u8], hour: u8) -> Option<Span> {
        self.times_tail(b, i, head, hour, b"")
    }

    /// [`Times::times`] with `tail` after the time of day in the pattern (ctime's year).
    fn times_then(&mut self, b: &[u8], i: usize, head: &[u8], tail: &[u8]) -> Option<Span> {
        self.times_tail(b, i, head, b'H', tail)
    }

    fn times_tail(&mut self, b: &[u8], i: usize, head: &[u8], hour: u8, tail: &[u8]) -> Option<Span> {
        // (the digits of a fraction where the text has one, and the segment's way of writing it)
        let (point, k) = self.fraction_at(b, i + head_len(head, hour, b, i));
        let mut cands: Vec<Vec<u8>> = Vec::new();
        let mut frac = |f: Vec<u8>| {
            if !cands.contains(&f) {
                cands.push(f)
            }
        };
        let t = |f: &[u8]| {
            let mut p = head.to_vec();
            p.extend_from_slice(&[hour, b':', b'M', b':', b'S']);
            p.extend_from_slice(f);
            p.extend_from_slice(tail);
            p
        };
        match self.frac {
            Frac::Fixed(p, k) => frac(t(&[b'f', p, b'0' + k])),
            Frac::Trimmed(p, k) => frac(t(&[b't', p, b'0' + k])),
            Frac::No => {}
        }
        if k > 0 {
            frac(t(&[b'f', point, b'0' + k]));
        }
        frac(t(b""));
        let mut hm = head.to_vec();
        hm.extend_from_slice(&[hour, b':', b'M']);
        hm.extend_from_slice(tail);
        frac(hm);
        for p in cands {
            self.pat = p;
            if let Some(sp) = self.check(b, i) {
                return Some(sp);
            }
        }
        None
    }

    /// The point and the digits (1 to 9, else 0) of a fraction after the `H:M:S` that a time
    /// of day at `b[at..]` starts with.
    fn fraction_at(&self, b: &[u8], at: usize) -> (u8, u8) {
        // (hours of one or two digits)
        let h = if digits(b, at, 2) { 2 } else { 1 };
        let p = at + h + 6;
        match b.get(p) {
            Some(&c @ (b'.' | b',')) => {
                let k = b[p + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                if (1..=9).contains(&k) { (c, k as u8) } else { (c, 0) }
            }
            _ => (b'.', 0),
        }
    }

    /// The span of the pattern `self.pat` at `b[i]`, if it is a time that prints back as the
    /// bytes and ends at the end of a token (not by a letter, a digit, `_` or `:`, and not
    /// before the digits of a fraction).
    fn check(&mut self, b: &[u8], i: usize) -> Option<Span> {
        let (v, n) = time::parse(&self.pat, &b[i..])?;
        self.out.clear();
        if !time::put(&mut self.out, &self.pat, v) || self.out[..] != b[i..i + n] {
            return None;
        }
        let end = i + n;
        if let Some(&c) = b.get(end) {
            let zulu = c == b'Z' && b.get(end + 1).is_none_or(|d| !d.is_ascii_alphanumeric());
            let fraction = matches!(c, b'.' | b',') && b.get(end + 1).is_some_and(u8::is_ascii_digit);
            if (c.is_ascii_alphanumeric() && !zulu) || matches!(c, b'_' | b':') || fraction {
                return None;
            }
        }
        let id = match self.ids.get(&self.pat) {
            Some(&id) => id,
            None => {
                let id = self.pats.len() as u32;
                self.pats.push(self.pat.clone());
                self.ids.insert(self.pat.clone(), id);
                id
            }
        };
        Some(Span { start: i, end, pat: id, v })
    }
}

/// The bytes of the date or words of `head` as the text at `b[i..]` has them (they are fixed
/// width, but for an unpadded day), so as to find where its time of day starts.
fn head_len(head: &[u8], _hour: u8, b: &[u8], i: usize) -> usize {
    let mut n = 0;
    let mut at = 0;
    while at < head.len() {
        let w = match head[at] {
            b'Y' => 4,
            b'b' | b'a' => 3,
            b'\\' => 0,
            b'D' => {
                // (one or two digits)
                if digits(b, i + n, 2) { 2 } else { 1 }
            }
            b'm' | b'd' | b'e' => 2,
            _ => 1,
        };
        n += w;
        at += 1;
    }
    n
}
