//! A small lexer and object parser for PDF syntax (ISO 32000 §7.2-7.3), enough to read
//! stream dictionaries. Values borrow from the input; nothing is decoded that is not needed
//! (names keep their `#xx` escapes, strings their escapes).

/// A parsed object. Dictionaries keep their entries in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Obj<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Real(&'a [u8]),
    /// A name without its slash.
    Name(&'a [u8]),
    /// A literal or hexadecimal string, as it is in the file (delimiters included).
    Str(&'a [u8]),
    Array(Vec<Obj<'a>>),
    Dict(Vec<(&'a [u8], Obj<'a>)>),
    Ref(u32, u16),
}

impl<'a> Obj<'a> {
    pub fn get(&self, key: &[u8]) -> Option<&Obj<'a>> {
        match self {
            Obj::Dict(d) => d.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Obj::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&'a [u8]> {
        match *self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }

    /// The name, or the names of an array (filters, their parameters).
    pub fn names(&self) -> Vec<&'a [u8]> {
        match self {
            Obj::Name(n) => vec![*n],
            Obj::Array(a) => a.iter().filter_map(Obj::as_name).collect(),
            _ => vec![],
        }
    }
}

#[inline]
pub fn is_white(b: u8) -> bool {
    matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\0')
}

#[inline]
pub fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

#[inline]
fn is_regular(b: u8) -> bool {
    !is_white(b) && !is_delim(b)
}

/// Nesting limit for arrays and dictionaries.
const MAX_DEPTH: usize = 64;

pub struct Parser<'a> {
    pub s: &'a [u8],
    pub pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(s: &'a [u8], pos: usize) -> Self {
        Parser { s, pos }
    }

    /// Skips white space and comments.
    pub fn skip_ws(&mut self) {
        while let Some(&b) = self.s.get(self.pos) {
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(&c) = self.s.get(self.pos) {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn regular(&mut self) -> &'a [u8] {
        let st = self.pos;
        while self.s.get(self.pos).is_some_and(|&b| is_regular(b)) {
            self.pos += 1;
        }
        &self.s[st..self.pos]
    }

    /// A keyword or other regular token, after white space.
    pub fn keyword(&mut self) -> &'a [u8] {
        self.skip_ws();
        self.regular()
    }

    pub fn object(&mut self) -> Option<Obj<'a>> {
        self.object_at(0)
    }

    fn object_at(&mut self, depth: usize) -> Option<Obj<'a>> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        let s = self.s;
        let b = *s.get(self.pos)?;
        match b {
            b'/' => {
                self.pos += 1;
                Some(Obj::Name(self.regular()))
            }
            b'(' => {
                let st = self.pos;
                let mut depth = 0usize;
                loop {
                    let c = *s.get(self.pos)?;
                    self.pos += 1;
                    match c {
                        b'\\' => self.pos += 1,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Some(Obj::Str(s.get(st..self.pos)?))
            }
            b'<' if s.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                let mut d = vec![];
                loop {
                    self.skip_ws();
                    match s.get(self.pos)? {
                        b'>' => {
                            if s.get(self.pos + 1) != Some(&b'>') {
                                return None;
                            }
                            self.pos += 2;
                            return Some(Obj::Dict(d));
                        }
                        b'/' => {
                            self.pos += 1;
                            let k = self.regular();
                            let v = self.object_at(depth + 1)?;
                            d.push((k, v));
                        }
                        _ => return None,
                    }
                }
            }
            b'<' => {
                let st = self.pos;
                let end = memchr::memchr(b'>', &s[st..])?;
                if !s[st + 1..st + end].iter().all(|&c| c.is_ascii_hexdigit() || is_white(c)) {
                    return None;
                }
                self.pos = st + end + 1;
                Some(Obj::Str(&s[st..self.pos]))
            }
            b'[' => {
                self.pos += 1;
                let mut a = vec![];
                loop {
                    self.skip_ws();
                    if *s.get(self.pos)? == b']' {
                        self.pos += 1;
                        return Some(Obj::Array(a));
                    }
                    a.push(self.object_at(depth + 1)?);
                }
            }
            _ if is_delim(b) => None,
            _ => {
                let t = self.regular();
                match t {
                    b"null" => Some(Obj::Null),
                    b"true" => Some(Obj::Bool(true)),
                    b"false" => Some(Obj::Bool(false)),
                    _ => {
                        if let Some(i) = parse_int(t) {
                            // `N G R`?
                            let save = self.pos;
                            if (0..=u32::MAX as i64).contains(&i) {
                                self.skip_ws();
                                let g = self.regular();
                                if let Some(g) = parse_int(g)
                                    && (0..=u16::MAX as i64).contains(&g)
                                {
                                    self.skip_ws();
                                    if self.regular() == b"R" {
                                        return Some(Obj::Ref(i as u32, g as u16));
                                    }
                                }
                            }
                            self.pos = save;
                            Some(Obj::Int(i))
                        } else if is_real(t) {
                            Some(Obj::Real(t))
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }
}

pub fn parse_int(t: &[u8]) -> Option<i64> {
    let (neg, d) = match t.first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if d.is_empty() || d.len() > 18 || !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = d.iter().fold(0i64, |a, &c| a * 10 + (c - b'0') as i64);
    Some(if neg { -v } else { v })
}

fn is_real(t: &[u8]) -> bool {
    let d = match t.first() {
        Some(b'-' | b'+') => &t[1..],
        _ => t,
    };
    let dots = d.iter().filter(|&&c| c == b'.').count();
    !d.is_empty() && dots <= 1 && d.len() > dots && d.iter().all(|&c| c == b'.' || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict() {
        let s = b"<< /Length 12 0 R /Filter [/FlateDecode] /A (x(y)\\)) /B <ab 12> /C -1.5 /D << /E null >> >>";
        let o = Parser::new(s, 0).object().unwrap();
        assert_eq!(o.get(b"Length"), Some(&Obj::Ref(12, 0)));
        assert_eq!(o.get(b"Filter").unwrap().names(), vec![b"FlateDecode".as_slice()]);
        assert_eq!(o.get(b"A"), Some(&Obj::Str(b"(x(y)\\))")));
        assert_eq!(o.get(b"B"), Some(&Obj::Str(b"<ab 12>")));
        assert_eq!(o.get(b"C"), Some(&Obj::Real(b"-1.5")));
        assert_eq!(o.get(b"D").unwrap().get(b"E"), Some(&Obj::Null));
    }
}
