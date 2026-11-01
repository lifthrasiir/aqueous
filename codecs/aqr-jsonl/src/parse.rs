//! The encoder's parser: a line as the tokens of its value and its separators, where it is a
//! JSON value, and the style of a file told from a sample.

use memchr::{memchr, memchr2};

use crate::tree::{E_CRLF, E_LF, E_NONE, MAX_DEPTH, Style};

/// A token of a line's value, in order (a container before its contents); ranges are of the file.
/// Its separators are in a list of their own ([`S_COLON`], [`S_COMMA`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Null,
    False,
    True,
    /// The bytes between the quotes.
    Str(usize, usize),
    Num(usize, usize),
    /// An object of `n` members, each a key then a value; the tokens end before `end`.
    Obj {
        n: u32,
        end: u32,
    },
    Key(usize, usize),
    Arr {
        n: u32,
        end: u32,
    },
}

/// A key's colon is followed by a space.
pub const S_COLON: u8 = 1;
/// The comma before a key or an array's element is followed by a space.
pub const S_COMMA: u8 = 2;
/// An object's or array's opening is followed by a space.
pub const S_OPEN: u8 = 4;
/// An object's or array's last member or element is followed by a space.
pub const S_CLOSE: u8 = 8;

/// The index of the token after the value at `i`.
#[inline]
pub fn skip(toks: &[Tok], i: usize) -> usize {
    match toks[i] {
        Tok::Obj { end, .. } | Tok::Arr { end, .. } => end as usize,
        _ => i + 1,
    }
}

/// A line of the file: its bytes (without its end) and its end.
pub struct Line {
    pub start: usize,
    pub end: usize,
    /// The start of the next line.
    pub next: usize,
    pub term: u64,
}

/// The line of `d` at `at` (`at` < `d.len()`).
#[inline]
pub fn line(d: &[u8], at: usize) -> Line {
    match memchr(b'\n', &d[at..]) {
        Some(k) => {
            let lf = at + k;
            let cr = lf > at && d[lf - 1] == b'\r';
            Line { start: at, end: lf - cr as usize, next: lf + 1, term: if cr { E_CRLF } else { E_LF } }
        }
        None => Line { start: at, end: d.len(), next: d.len(), term: E_NONE },
    }
}

struct Parser<'a> {
    d: &'a [u8],
    i: usize,
    end: usize,
    toks: &'a mut Vec<Tok>,
    seps: &'a mut Vec<u8>,
    /// Separators: of a colon, then of a comma, without a space and with one.
    counts: [[usize; 2]; 2],
}

impl Parser<'_> {
    #[inline]
    fn peek(&self) -> Option<u8> {
        (self.i < self.end).then(|| self.d[self.i])
    }

    #[inline]
    fn expect(&mut self, s: &[u8]) -> Option<()> {
        self.d[self.i..self.end].starts_with(s).then(|| self.i += s.len())
    }

    #[inline]
    fn push(&mut self, t: Tok) {
        self.toks.push(t);
        self.seps.push(0);
    }

    /// A space after an opening at `at`, if any, taken.
    #[inline]
    fn open(&mut self, at: usize) {
        if self.peek() == Some(b' ') {
            self.i += 1;
            self.seps[at] |= S_OPEN;
        }
    }

    /// The end of a container (`close`) after a member or element, perhaps after a space, of the
    /// container at `at`; else a comma (its bit), or nothing.
    #[inline]
    fn next(&mut self, at: usize, close: u8) -> Option<Option<u8>> {
        match self.peek()? {
            b',' => Some(Some(self.sep(1, S_COMMA))),
            c if c == close => {
                self.i += 1;
                Some(None)
            }
            b' ' if self.i + 1 < self.end && self.d[self.i + 1] == close => {
                self.i += 2;
                self.seps[at] |= S_CLOSE;
                Some(None)
            }
            _ => None,
        }
    }

    /// A colon (`which` 0) or comma (1) taken, with a space after it or not: its bit `bit`.
    #[inline]
    fn sep(&mut self, which: usize, bit: u8) -> u8 {
        self.i += 1;
        let sp = self.peek() == Some(b' ');
        self.i += sp as usize;
        self.counts[which][sp as usize] += 1;
        if sp { bit } else { 0 }
    }

    /// A string's bytes between the quotes, if it is closed.
    fn string(&mut self) -> Option<(usize, usize)> {
        let start = self.i + 1;
        let mut j = start;
        loop {
            let k = j + memchr2(b'"', b'\\', &self.d[j..self.end])?;
            if self.d[k] == b'"' {
                self.i = k + 1;
                return Some((start, k));
            }
            j = k + 2;
            if j > self.end {
                return None;
            }
        }
    }

    /// A number of JSON's grammar.
    fn number(&mut self) -> Option<(usize, usize)> {
        let (d, start) = (self.d, self.i);
        let digits = |i: &mut usize| {
            let s = *i;
            while *i < self.end && d[*i].is_ascii_digit() {
                *i += 1;
            }
            *i > s
        };
        let mut i = start;
        if d[i] == b'-' {
            i += 1;
        }
        match d.get(i).filter(|_| i < self.end) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                digits(&mut i);
            }
            _ => return None,
        }
        if i < self.end && d[i] == b'.' {
            i += 1;
            if !digits(&mut i) {
                return None;
            }
        }
        if i < self.end && d[i] | 0x20 == b'e' {
            i += 1;
            if i < self.end && matches!(d[i], b'+' | b'-') {
                i += 1;
            }
            if !digits(&mut i) {
                return None;
            }
        }
        self.i = i;
        Some((start, i))
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        match self.peek()? {
            b'{' if depth < MAX_DEPTH => {
                self.i += 1;
                let at = self.toks.len();
                self.push(Tok::Null);
                let mut n = 0u32;
                let mut comma = 0;
                self.open(at);
                if self.peek() == Some(b'}') {
                    self.i += 1;
                } else {
                    loop {
                        if self.peek() != Some(b'"') {
                            return None;
                        }
                        let (a, b) = self.string()?;
                        self.push(Tok::Key(a, b));
                        let k = self.seps.len() - 1;
                        if self.peek() != Some(b':') {
                            return None;
                        }
                        self.seps[k] = comma | self.sep(0, S_COLON);
                        self.value(depth + 1)?;
                        n += 1;
                        match self.next(at, b'}')? {
                            Some(c) => comma = c,
                            None => break,
                        }
                    }
                }
                self.toks[at] = Tok::Obj { n, end: self.toks.len() as u32 };
            }
            b'[' if depth < MAX_DEPTH => {
                self.i += 1;
                let at = self.toks.len();
                self.push(Tok::Null);
                let mut n = 0u32;
                let mut comma = 0;
                self.open(at);
                if self.peek() == Some(b']') {
                    self.i += 1;
                } else {
                    loop {
                        let k = self.seps.len();
                        self.value(depth + 1)?;
                        self.seps[k] |= comma;
                        n += 1;
                        match self.next(at, b']')? {
                            Some(c) => comma = c,
                            None => break,
                        }
                    }
                }
                self.toks[at] = Tok::Arr { n, end: self.toks.len() as u32 };
            }
            b'"' => {
                let (a, b) = self.string()?;
                self.push(Tok::Str(a, b));
            }
            b'n' => {
                self.expect(b"null")?;
                self.push(Tok::Null);
            }
            b't' => {
                self.expect(b"true")?;
                self.push(Tok::True);
            }
            b'f' => {
                self.expect(b"false")?;
                self.push(Tok::False);
            }
            b'-' | b'0'..=b'9' => {
                let (a, b) = self.number()?;
                self.push(Tok::Num(a, b));
            }
            _ => return None,
        }
        Some(())
    }
}

/// The tokens of a line and their separators.
#[derive(Default)]
pub struct Toks {
    pub t: Vec<Tok>,
    /// Of each token: of a key, its colon and the comma before it; of an array's element, the
    /// comma before it; of an object or array, a space within its opening and closing
    /// ([`S_COLON`], [`S_COMMA`], [`S_OPEN`], [`S_CLOSE`]).
    pub seps: Vec<u8>,
    /// Separators: of colons, then of commas, without a space and with one.
    pub counts: [[usize; 2]; 2],
    /// Whether the value is followed by a comma (that ends the line).
    pub comma: bool,
}

/// The tokens of line `l` of `d` (into `toks`, cleared first; their separators counted), where
/// it is a JSON value and nothing else (but a comma), its separators a comma or colon and perhaps
/// a space; else false (`toks` left in any state).
pub fn tokens(d: &[u8], l: &Line, toks: &mut Toks) -> bool {
    toks.t.clear();
    toks.seps.clear();
    // (a token's indices fit in 32 bits)
    if l.end - l.start >= u32::MAX as usize >> 1 {
        return false;
    }
    let mut p = Parser { d, i: l.start, end: l.end, toks: &mut toks.t, seps: &mut toks.seps, counts: [[0; 2]; 2] };
    let ok = p.value(0).is_some() && (p.i == l.end || p.i + 1 == l.end && d[p.i] == b',');
    toks.comma = p.i != l.end;
    let c = p.counts;
    if ok {
        for (a, b) in toks.counts.iter_mut().flatten().zip(c.iter().flatten()) {
            *a += b;
        }
    }
    ok
}

/// The lines of a sample that tell its style.
const SAMPLE_LINES: usize = 1000;

/// The style of JSON lines starting with `s` (a sample of the file, `whole` if it is all of it),
/// if any: where 9 lines in 10 at least are objects or arrays, of 8 lines at least (2 in a short
/// file), the separators most of theirs are.
pub fn style(s: &[u8], whole: bool) -> Option<Style> {
    let mut n = 0;
    let mut good = 0;
    let mut toks = Toks::default();
    let mut at = 0;
    while at < s.len() && n < SAMPLE_LINES {
        let l = line(s, at);
        // (the last line cut by the sample's end)
        if l.term == E_NONE && !whole {
            break;
        }
        at = l.next;
        n += 1;
        if tokens(s, &l, &mut toks) && matches!(toks.t[0], Tok::Obj { .. } | Tok::Arr { .. }) {
            good += 1;
        }
    }
    if n < if whole { 2 } else { 8 } || good * 10 < n * 9 {
        return None;
    }
    let [colon, comma] = toks.counts;
    Some(Style { comma_space: comma[1] > comma[0], colon_space: colon[1] > colon[0] })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Option<Toks> {
        let d = s.as_bytes();
        let mut t = Toks::default();
        tokens(d, &line(d, 0), &mut t).then_some(t)
    }

    #[test]
    fn values() {
        let t = toks(r#"{"a":[1,-2.5e+3,"x\"y"],"b":{},"c":null}"#).unwrap().t;
        assert_eq!(t[0], Tok::Obj { n: 3, end: t.len() as u32 });
        assert!(matches!(t[2], Tok::Arr { n: 3, end: 6 }));
        assert_eq!(skip(&t, 2), 6);
        assert_eq!(t[5], Tok::Str(17, 21));
        for bad in [
            r#"{"a":1,}"#,
            r#"{"a" :1}"#,
            r#"{"a":1} "#,
            r#"{"a":1},,"#,
            r#"{"a":1} ,"#,
            r#"{"a":  1}"#,
            r#"[01]"#,
            r#"[1.]"#,
            r#"[-]"#,
            r#"["a\"]"#,
            r#"[tru]"#,
        ] {
            assert!(toks(bad).is_none(), "{bad}");
        }
        let t = toks(r#"{ "a": 1,"b":[ 2, 3,4 ], "c":5}"#).unwrap();
        assert_eq!(t.seps, [S_OPEN, S_COLON, 0, 0, S_OPEN | S_CLOSE, 0, S_COMMA, 0, S_COMMA, 0]);
        assert!(toks("[1],").is_some_and(|t| t.comma) && toks("[1]").is_some_and(|t| !t.comma));
        assert!(toks("[ ]").is_some() && toks("[ 1]").is_some() && toks("{ }").is_some());
        assert!(toks("[  1]").is_none() && toks("[1  ]").is_none() && toks("[ ,1]").is_none());
        assert_eq!(t.counts, [[2, 1], [2, 2]]);
        let deep = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(toks(&deep).is_some());
        let deeper = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert!(toks(&deeper).is_none());
    }

    #[test]
    fn styles() {
        let s = "{\"a\": 1, \"b\": 2}\n".repeat(10);
        assert_eq!(style(s.as_bytes(), true), Some(Style { comma_space: true, colon_space: true }));
        let s = "{\"a\":1,\"b\":[2,3]}\r\n".repeat(10);
        assert_eq!(style(s.as_bytes(), false), Some(Style::default()));
        let s = "{\"a\": 1, \"b\": {\"c\": 2, \"e\": 4},\"d\": 3}\n".repeat(10);
        assert_eq!(style(s.as_bytes(), true), Some(Style { comma_space: true, colon_space: true }));
        assert_eq!(style(b"1\n2\n3\n4\n5\n6\n7\n8\n9\n", true), None);
        assert_eq!(style(b"a,b\n1,2\n", true), None);
    }
}
