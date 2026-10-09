//! Finding the streams of a PDF file.
//!
//! Nothing here needs the file to be well formed: a stream is any `N G obj << ... >> stream`
//! whose dictionary parses, wherever it is (incremental updates, damaged cross-reference
//! tables and junk do not matter). Whatever is not recognised stays in the skeleton, so a
//! miss costs ratio, never correctness.

use crate::lex::{Obj, Parser, is_white};
use memchr::memmem;
use std::ops::Range;

/// A stream: its dictionary and where its data is.
#[derive(Debug, Clone)]
pub struct Stream<'a> {
    pub dict: Obj<'a>,
    /// The data, from after the end of line that follows `stream`: `/Length` bytes if that
    /// is a direct value followed by `endstream`, else up to the end of line before the
    /// next `endstream` (or the end of the file).
    pub data: Range<usize>,
    /// Whether `data` comes from a direct `/Length`.
    pub length_ok: bool,
}

/// How far back the object header may be from `stream`.
const MAX_DICT: usize = 1 << 16;

/// Every stream of `buf`, in order, not overlapping.
pub fn streams(buf: &[u8]) -> Vec<Stream<'_>> {
    let mut out = vec![];
    let kw = memmem::Finder::new(b"stream");
    let endkw = memmem::Finder::new(b"endstream");
    let mut pos = 0;
    // Streams do not start before this.
    let mut floor = 0;
    while let Some(i) = kw.find(&buf[pos..]) {
        let h = pos + i;
        pos = h + 6;
        if h >= 3 && &buf[h - 3..h] == b"end" {
            continue;
        }
        let Some(dict) = dict_before(buf, floor, h) else {
            continue;
        };
        // The end of line after the keyword: CRLF or LF (CR alone is not allowed, but
        // some writers use it).
        let start = match buf.get(h + 6..h + 8) {
            Some(b"\r\n") => h + 8,
            Some([b'\n', _]) => h + 7,
            Some([b'\r', _]) => h + 7,
            _ if buf.get(h + 6) == Some(&b'\n') => h + 7,
            _ => continue,
        };
        let by_length = dict.get(b"Length").and_then(Obj::as_int).and_then(|l| {
            let end = start.checked_add(usize::try_from(l).ok()?)?;
            let mut e = end;
            while buf.get(e).is_some_and(|&b| is_white(b)) && e < end + 4 {
                e += 1;
            }
            buf.get(e..e + 9).is_some_and(|k| k == b"endstream").then_some(end)
        });
        let (end, length_ok) = match by_length {
            Some(e) => (e, true),
            None => {
                let e = endkw.find(&buf[start..]).map_or(buf.len(), |k| start + k);
                let mut d = e;
                if d > start && buf[d - 1] == b'\n' {
                    d -= 1;
                }
                if d > start && buf[d - 1] == b'\r' {
                    d -= 1;
                }
                (d, false)
            }
        };
        out.push(Stream {
            dict,
            data: start..end,
            length_ok,
        });
        pos = end;
        floor = end;
    }
    out
}

/// The dictionary of an object `N G obj << ... >>` that ends (but for white space) at
/// `kw`, starting at or after `floor`.
fn dict_before(buf: &[u8], floor: usize, kw: usize) -> Option<Obj<'_>> {
    let lo = floor.max(kw.saturating_sub(MAX_DICT));
    let mut e = kw;
    while e > lo && is_white(buf[e - 1]) {
        e -= 1;
    }
    if e < lo + 2 || &buf[e - 2..e] != b">>" {
        return None;
    }
    let objkw = memmem::FinderRev::new(b"obj");
    let mut hi = e;
    for _ in 0..4 {
        let o = lo + objkw.rfind(&buf[lo..hi])?;
        hi = o;
        let mut p = Parser::new(buf, o + 3);
        if let Some(d @ Obj::Dict(_)) = p.object() {
            p.skip_ws();
            if p.pos == kw {
                return Some(d);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds() {
        let f = b"%PDF-1.4\n1 0 obj\n<< /Length 5 >>\nstream\nhello\nendstream\nendobj\n\
                  2 0 obj << /Length 3 0 R /T (obj) >>stream\r\nab endstream x\nendstream endobj\n\
                  3 0 obj 99 endobj\n4 0 obj (stream) endobj";
        let s = streams(f);
        assert_eq!(s.len(), 2);
        assert_eq!(&f[s[0].data.clone()], b"hello");
        assert!(s[0].length_ok);
        assert_eq!(&f[s[1].data.clone()], b"ab ");
        assert!(!s[1].length_ok);
    }
}
