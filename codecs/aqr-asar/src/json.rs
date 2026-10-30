//! A reader of JSON that keeps where everything is, for the encoder to find the values it
//! derives: values are spans of the text, strings as they are written (escapes kept).

use std::ops::Range;

/// A value, and its span in the text.
#[derive(Debug, Clone)]
pub struct Value {
    pub span: Range<usize>,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// The members: each key's span between its quotes, and the value.
    Object(Vec<(Range<usize>, Value)>),
    Array(Vec<Value>),
    String,
    Number,
    /// `true`, `false` or `null`.
    Literal,
}

impl Value {
    /// The value of the first member of key `key` (as written), if an object.
    pub fn get<'a>(&'a self, text: &[u8], key: &[u8]) -> Option<&'a Value> {
        let Kind::Object(ms) = &self.kind else { return None };
        ms.iter().find(|(k, _)| &text[k.clone()] == key).map(|(_, v)| v)
    }

    /// The span between the quotes, if a string.
    pub fn inner(&self) -> Option<Range<usize>> {
        matches!(self.kind, Kind::String).then(|| self.span.start + 1..self.span.end - 1)
    }
}

/// Nesting deeper than this is not read.
const DEPTH: usize = 1000;

/// `text` as a JSON value, whitespace around it allowed; None where it is not one.
pub fn parse(text: &[u8]) -> Option<Value> {
    let mut p = Parser { t: text, at: 0 };
    let v = p.value(0)?;
    p.space();
    (p.at == text.len()).then_some(v)
}

struct Parser<'a> {
    t: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.t.get(self.at).is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.space();
        let ok = self.t.get(self.at) == Some(&b);
        self.at += ok as usize;
        ok
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > DEPTH {
            return None;
        }
        self.space();
        let start = self.at;
        let kind = match *self.t.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut ms = Vec::new();
                if !self.eat(b'}') {
                    loop {
                        self.space();
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        ms.push((k.start + 1..k.end - 1, self.value(depth + 1)?));
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Kind::Object(ms)
            }
            b'[' => {
                self.at += 1;
                let mut vs = Vec::new();
                if !self.eat(b']') {
                    loop {
                        vs.push(self.value(depth + 1)?);
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Kind::Array(vs)
            }
            b'"' => {
                self.string()?;
                Kind::String
            }
            b'-' | b'0'..=b'9' => {
                self.at += 1;
                while self.t.get(self.at).is_some_and(|b| matches!(b, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
                    self.at += 1;
                }
                Kind::Number
            }
            _ => {
                let w = [&b"true"[..], b"false", b"null"].into_iter().find(|w| self.t[self.at..].starts_with(w))?;
                self.at += w.len();
                Kind::Literal
            }
        };
        Some(Value { span: start..self.at, kind })
    }

    /// The span of the string at the cursor, its quotes included.
    fn string(&mut self) -> Option<Range<usize>> {
        let start = self.at;
        if self.t.get(self.at) != Some(&b'"') {
            return None;
        }
        self.at += 1;
        loop {
            match *self.t.get(self.at)? {
                b'"' => break,
                b'\\' => self.at += 2,
                _ => self.at += 1,
            }
        }
        self.at += 1;
        Some(start..self.at)
    }
}
