//! Hugging Face's safetensors files (FORMAT.md §2): the bytes of a header of JSON (a u64,
//! little-endian), the header, an object of each tensor's `dtype`, `shape` and `data_offsets`
//! (and `__metadata__`, strings), then the data, each tensor at its offsets in it. Only the types
//! and offsets are read; the rest of the JSON is skipped as it parses.

use crate::layout::Float;
use crate::tensors::{self, Container, Data, Tensors};

/// The header's bytes at most taken (safetensors' own limit is 100 MB).
const MAX_HEADER: u64 = 100_000_000;

/// The bytes of the header of `d`, where its first bytes are of one: within the file, an object.
fn header_len(d: &[u8]) -> Option<usize> {
    let n = u64::from_le_bytes(d.get(..8)?.try_into().unwrap());
    if !(2..=MAX_HEADER).contains(&n) || 8 + n > d.len() as u64 {
        return None;
    }
    let h = &d[8..8 + n as usize];
    // (the writers pad the header with spaces to an alignment)
    (h[0] == b'{' && h.trim_ascii_end().ends_with(b"}")).then_some(n as usize)
}

/// Whether `d` starts as a safetensors file: its header's bytes, then an object (of a string
/// first, or empty).
pub fn starts(d: &[u8]) -> bool {
    header_len(d).is_some_and(|n| matches!(d[9..8 + n].trim_ascii_start().first(), Some(b'"' | b'}')))
}

/// A cursor over JSON.
struct Json<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Json<'a> {
    fn ws(&mut self) {
        while self.d.get(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.d.get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.peek()? == b).then(|| self.at += 1)
    }

    /// A string, its raw bytes between the quotes (escapes not undone).
    fn string(&mut self) -> Option<&'a [u8]> {
        self.eat(b'"')?;
        let start = self.at;
        loop {
            match *self.d.get(self.at)? {
                b'"' => break,
                b'\\' => self.at += 2,
                _ => self.at += 1,
            }
        }
        self.at += 1;
        Some(&self.d[start..self.at - 1])
    }

    /// An unsigned integer.
    fn uint(&mut self) -> Option<u64> {
        self.ws();
        let start = self.at;
        let mut v = 0u64;
        while let Some(&b) = self.d.get(self.at).filter(|b| b.is_ascii_digit()) {
            v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
            self.at += 1;
        }
        (self.at > start).then_some(v)
    }

    /// The members of an object, `f` given each key and the cursor at its value, which it takes.
    fn object(&mut self, mut f: impl FnMut(&mut Self, &'a [u8]) -> Option<()>) -> Option<()> {
        self.eat(b'{')?;
        if self.eat(b'}').is_some() {
            return Some(());
        }
        loop {
            let k = self.string()?;
            self.eat(b':')?;
            f(self, k)?;
            if self.eat(b',').is_none() {
                return self.eat(b'}');
            }
        }
    }

    /// A value skipped, of arrays and objects nested `depth` deep at most.
    fn skip(&mut self, depth: u32) -> Option<()> {
        match self.peek()? {
            b'"' => _ = self.string()?,
            b'{' if depth > 0 => self.object(|j, _| j.skip(depth - 1))?,
            b'[' if depth > 0 => {
                self.at += 1;
                if self.eat(b']').is_none() {
                    loop {
                        self.skip(depth - 1)?;
                        if self.eat(b',').is_none() {
                            break self.eat(b']')?;
                        }
                    }
                }
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                let start = self.at;
                while self.d.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || b"+-.".contains(b)) {
                    self.at += 1;
                }
                (self.at > start).then_some(())?;
            }
            _ => return None,
        }
        Some(())
    }
}

/// The float of a safetensors dtype.
fn float_of(dtype: &[u8]) -> Option<Float> {
    match dtype {
        b"F32" => Some(Float::F32),
        b"F16" => Some(Float::F16),
        b"BF16" => Some(Float::Bf16),
        _ => None,
    }
}

/// The layout of safetensors file `d`, None where it is not one (or its header does not hold).
pub fn parse(d: &[u8]) -> Option<Tensors> {
    if !starts(d) {
        return None;
    }
    let n = header_len(d)?;
    let data = 8 + n;
    let mut j = Json { d: &d[8..data], at: 0 };
    let mut coded = Vec::new();
    j.object(|j, key| {
        if key == b"__metadata__" {
            return j.skip(2);
        }
        let (mut dtype, mut offsets) = (None, None);
        j.object(|j, k| match k {
            b"dtype" => j.string().map(|s| dtype = Some(s)),
            b"data_offsets" => {
                j.eat(b'[')?;
                let a = j.uint()?;
                j.eat(b',')?;
                let b = j.uint()?;
                offsets = Some((a, b));
                j.eat(b']')
            }
            _ => j.skip(2),
        })?;
        let (dtype, (a, b)) = (dtype?, offsets?);
        if let Some(f) = float_of(dtype)
            && let Some(len) = b.checked_sub(a)
            && len % f.bytes() as u64 == 0
        {
            coded.push(((data as u64).checked_add(a)?, Data::Float(f, len / f.bytes() as u64)));
        }
        Some(())
    })?;
    Some(tensors::apart(Container::Safetensors, d, data, coded))
}
