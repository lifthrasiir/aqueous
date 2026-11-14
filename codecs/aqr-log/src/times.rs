//! The encoder's side of times: timestamps found in a line, as spans of its bytes with an
//! aqr-num pattern and the integer it prints (FORMAT.md, section 5).

use std::collections::HashMap;

use aqr_num::time::{self, MONTHS, NO_YEAR, WEEKDAYS};

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
    /// The patterns used, by id.
    pats: Vec<Vec<u8>>,
    /// The patterns tried, by bytes, as an index of `comp`.
    index: HashMap<Vec<u8>, usize>,
    comp: Vec<Option<Compiled>>,
    pub spans: Vec<Span>,
    pat: Vec<u8>,
    /// What the generic printing makes (for a pattern of bytes not made here).
    out: Vec<u8>,
}

/// One item of a pattern (aqr-num's, README "Times"), so that it is read once.
#[derive(Clone, Copy, Debug)]
enum It {
    Lit(u8),
    Year,
    Month2,
    MonthName,
    Day2,
    DayPad,
    Day,
    Weekday,
    Hour2,
    Hour,
    Minute,
    Second,
    /// A fraction: its point, digits, and whether trailing zeros are dropped.
    Frac(u8, u8, bool),
}

/// A pattern read once. `items` is `None` where it holds a byte that the encoder does not make
/// (then aqr-num's [`time::parse`] and [`time::put`] do the work).
struct Compiled {
    items: Option<Vec<It>>,
    /// The id the pattern has been given, once it has matched.
    id: Option<u32>,
}

impl Compiled {
    /// `pat` read, or `None` where it is no pattern.
    fn new(pat: &[u8]) -> Option<Compiled> {
        if !time::valid(pat) {
            return None;
        }
        let mut items = Some(Vec::new());
        let mut at = 0;
        while at < pat.len() {
            let it = match pat[at] {
                b'Y' => It::Year,
                b'm' => It::Month2,
                b'b' => It::MonthName,
                b'd' => It::Day2,
                b'e' => It::DayPad,
                b'D' => It::Day,
                b'a' => It::Weekday,
                b'H' => It::Hour2,
                b'h' => It::Hour,
                b'M' => It::Minute,
                b'S' => It::Second,
                c @ (b'f' | b't') => match pat.get(at + 1..at + 3) {
                    Some(&[point @ (b'.' | b','), n @ b'1'..=b'9']) => {
                        at += 2;
                        It::Frac(point, n - b'0', c == b't')
                    }
                    _ => {
                        items = None;
                        break;
                    }
                },
                b'\\' => match pat.get(at + 1) {
                    Some(&c) => {
                        at += 1;
                        It::Lit(c)
                    }
                    None => {
                        items = None;
                        break;
                    }
                },
                c if c.is_ascii_alphabetic() || c < 5 || c == b'\n' => {
                    items = None;
                    break;
                }
                c => It::Lit(c),
            };
            at += 1;
            items.as_mut().unwrap().push(it);
        }
        Some(Compiled { items, id: None })
    }

    /// The integer of the time at the start of `t`, and its length, if [`time::parse`] takes it
    /// and [`time::put`] prints it as it is: the same as both together, the printing not made
    /// (it is the same bytes where every item is in the way it prints: no padded zero where it
    /// is none, no trailing zero of a trimmed fraction).
    fn exact(&self, pat: &[u8], t: &[u8], out: &mut Vec<u8>) -> Option<(i64, usize)> {
        let Some(items) = &self.items else {
            let (v, n) = time::parse(pat, t)?;
            out.clear();
            return (time::put(out, pat, v) && out[..] == t[..n]).then_some((v, n));
        };
        let (mut y, mut mo, mut d, mut wd) = (NO_YEAR, 0, 0, None);
        let (mut h, mut mi, mut s, mut frac) = (0i64, 0i64, 0i64, 0i64);
        let (mut date, mut hour, mut minute, mut second, mut fd) = (false, false, false, false, 0u32);
        let mut ti = 0;
        // (one or two digits, as many as there are, with none padded: the digits and their count)
        let unpadded = |at: usize| -> Option<(i64, usize)> {
            let a = *t.get(at)?;
            if !a.is_ascii_digit() {
                return None;
            }
            match t.get(at + 1) {
                Some(&b) if b.is_ascii_digit() => (a != b'0').then(|| (((a - b'0') * 10 + (b - b'0')) as i64, 2)),
                _ => Some(((a - b'0') as i64, 1)),
            }
        };
        for &it in items {
            match it {
                It::Lit(c) => {
                    if t.get(ti) != Some(&c) {
                        return None;
                    }
                    ti += 1;
                }
                It::Year => {
                    y = digits_at(t, ti, 4)? as i64;
                    ti += 4;
                    date = true;
                }
                It::Month2 => {
                    mo = digits_at(t, ti, 2)? as i64;
                    ti += 2;
                    date = true;
                }
                It::MonthName => {
                    let b = t.get(ti..ti + 3)?;
                    mo = MONTHS.iter().position(|m| m[..] == *b)? as i64 + 1;
                    ti += 3;
                    date = true;
                }
                It::Day2 => {
                    d = digits_at(t, ti, 2)? as i64;
                    ti += 2;
                    date = true;
                }
                It::DayPad => {
                    if t.get(ti) == Some(&b' ') {
                        d = digits_at(t, ti + 1, 1)? as i64;
                    } else {
                        d = digits_at(t, ti, 2)? as i64;
                        // (a day under 10 is printed with a space)
                        if d < 10 {
                            return None;
                        }
                    }
                    ti += 2;
                    date = true;
                }
                It::Day => {
                    let (v, n) = unpadded(ti)?;
                    (d, ti) = (v, ti + n);
                    date = true;
                }
                It::Weekday => {
                    let b = t.get(ti..ti + 3)?;
                    wd = Some(WEEKDAYS.iter().position(|m| m[..] == *b)?);
                    ti += 3;
                    date = true;
                }
                It::Hour2 => {
                    h = digits_at(t, ti, 2)? as i64;
                    ti += 2;
                    hour = true;
                }
                It::Hour => {
                    let (v, n) = unpadded(ti)?;
                    (h, ti) = (v, ti + n);
                    hour = true;
                }
                It::Minute => {
                    mi = digits_at(t, ti, 2)? as i64;
                    ti += 2;
                    minute = true;
                }
                It::Second => {
                    s = digits_at(t, ti, 2)? as i64;
                    ti += 2;
                    second = true;
                }
                It::Frac(point, n, trim) => {
                    let n = n as usize;
                    fd = n as u32;
                    if trim {
                        if t.get(ti) == Some(&point) {
                            let k = t[ti + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                            // (a trailing zero is not printed)
                            if !(1..=n).contains(&k) || t[ti + k] == b'0' {
                                return None;
                            }
                            frac = digits_at(t, ti + 1, k)? as i64 * 10i64.pow((n - k) as u32);
                            ti += 1 + k;
                        }
                    } else {
                        if t.get(ti) != Some(&point) {
                            return None;
                        }
                        frac = digits_at(t, ti + 1, n)? as i64;
                        ti += 1 + n;
                    }
                }
            }
        }
        if h > 23 || mi > 59 || s > 59 {
            return None;
        }
        let mut x = 0i128;
        if date {
            if !(1..=12).contains(&mo) || !(1..=days_in_month(y, mo)).contains(&d) {
                return None;
            }
            let days = time::days_from_civil(y, mo, d);
            if wd.is_some_and(|w| w != (days + 3).rem_euclid(7) as usize) {
                return None;
            }
            x = days as i128;
        }
        if hour {
            x = x * 24 + h as i128;
        }
        if minute {
            x = x * 60 + mi as i128;
        }
        if second {
            x = x * 60 + s as i128;
        }
        x = x * 10i128.pow(fd) + frac as i128;
        Some((i64::try_from(x).ok()?, ti))
    }
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The number of the `n` digits at `b[at..]`, if they are digits.
fn digits_at(b: &[u8], at: usize, n: usize) -> Option<u32> {
    let d = b.get(at..at + n)?;
    d.iter().all(u8::is_ascii_digit).then(|| d.iter().fold(0, |a, &c| a * 10 + (c - b'0') as u32))
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
            index: HashMap::new(),
            comp: Vec::new(),
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
        // the fractions' parts of the patterns, no two the same: the segment's, the text's, none
        let mut fracs = [[0u8; 3]; 3];
        let mut lens = [0usize; 3];
        let mut n = 0;
        let mut add = |f: &[u8]| {
            if !(0..n).any(|j| fracs[j][..lens[j]] == *f) {
                fracs[n][..f.len()].copy_from_slice(f);
                lens[n] = f.len();
                n += 1;
            }
        };
        match self.frac {
            Frac::Fixed(p, k) => add(&[b'f', p, b'0' + k]),
            Frac::Trimmed(p, k) => add(&[b't', p, b'0' + k]),
            Frac::No => {}
        }
        if k > 0 {
            add(&[b'f', point, b'0' + k]);
        }
        add(b"");
        for j in 0..n {
            self.pat.clear();
            self.pat.extend_from_slice(head);
            self.pat.extend_from_slice(&[hour, b':', b'M', b':', b'S']);
            self.pat.extend_from_slice(&fracs[j][..lens[j]]);
            self.pat.extend_from_slice(tail);
            if let Some(sp) = self.check(b, i) {
                return Some(sp);
            }
        }
        // minutes only
        self.pat.clear();
        self.pat.extend_from_slice(head);
        self.pat.extend_from_slice(&[hour, b':', b'M']);
        self.pat.extend_from_slice(tail);
        self.check(b, i)
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
        let k = match self.index.get(&self.pat[..]) {
            Some(&k) => k,
            None => {
                self.comp.push(Compiled::new(&self.pat));
                self.index.insert(self.pat.clone(), self.comp.len() - 1);
                self.comp.len() - 1
            }
        };
        let c = self.comp[k].as_ref()?;
        let (v, n) = c.exact(&self.pat, &b[i..], &mut self.out)?;
        let end = i + n;
        if let Some(&c) = b.get(end) {
            let zulu = c == b'Z' && b.get(end + 1).is_none_or(|d| !d.is_ascii_alphanumeric());
            let fraction = matches!(c, b'.' | b',') && b.get(end + 1).is_some_and(u8::is_ascii_digit);
            if (c.is_ascii_alphanumeric() && !zulu) || matches!(c, b'_' | b':') || fraction {
                return None;
            }
        }
        let c = self.comp[k].as_mut()?;
        let id = *c.id.get_or_insert_with(|| {
            self.pats.push(self.pat.clone());
            self.pats.len() as u32 - 1
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What `Compiled::exact` stands for.
    fn reference(pat: &[u8], t: &[u8]) -> Option<(i64, usize)> {
        let (v, n) = time::parse(pat, t)?;
        let mut out = Vec::new();
        (time::put(&mut out, pat, v) && out[..] == t[..n]).then_some((v, n))
    }

    #[test]
    fn exact_is_parse_and_put() {
        let pats: [&[u8]; 16] = [
            b"Y-m-d H:M:S",
            b"Y-m-d H:M:St.6",
            b"Y-m-d H:M:Sf.3",
            b"Y/m/d\\TH:M:St,9",
            b"Y-m-d H:M",
            b"Y-m-d",
            b"b e H:M:S",
            b"b D H:M:S",
            b"h:M:S",
            b"H:M:Sf,2",
            b"a b d H:M:S Y",
            b"a b e H:M:S Y",
            b"a, d b Y H:M:S",
            b"d/b/Y:H:M:S",
            b"m/d/Y H:M",
            b"d.m.Y",
        ];
        let mut r = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        let (mut taken, mut tried) = (0, 0);
        for pat in pats {
            let c = Compiled::new(pat).unwrap();
            assert!(c.items.is_some());
            for i in 0..30000 {
                let x = next();
                let v = match i % 3 {
                    0 => ((x % 4_000_000_000) as i64) * 86400 / 3,
                    1 => (x % 100_000_000_000) as i64,
                    _ => ((x >> 10) % 10_000_000_000_000) as i64 * 1000,
                };
                let mut text = Vec::new();
                if !time::put(&mut text, pat, v) {
                    continue;
                }
                // (the text, as it is, or with a byte changed, or one more digit, zeros padded)
                for _ in 0..4 {
                    let mut t = text.clone();
                    match next() % 6 {
                        0 => {}
                        1 => {
                            let at = (next() as usize) % t.len();
                            t[at] = b"0123456789 .,:-/ZTa"[(next() as usize) % 19];
                        }
                        2 => t.extend_from_slice(b"0"),
                        3 => {
                            let at = (next() as usize) % t.len();
                            t.insert(at, b'0');
                        }
                        4 => {
                            let at = (next() as usize) % t.len();
                            t.remove(at);
                        }
                        _ => {
                            let at = (next() as usize) % t.len();
                            t[at] = b'0';
                        }
                    }
                    let want = reference(pat, &t);
                    let got = c.exact(pat, &t, &mut Vec::new());
                    assert_eq!(got, want, "{} {}", String::from_utf8_lossy(pat), String::from_utf8_lossy(&t));
                    tried += 1;
                    taken += want.is_some() as usize;
                }
            }
        }
        // (both what is taken and what is not are many)
        assert!(taken > 100_000 && tried - taken > 100_000, "{taken} of {tried}");
    }
}
