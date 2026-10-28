//! The encoder's side: the dialect of a table told from a sample, and a file cut into records
//! and fields (`FORMAT.md`, "Encoding").

use std::ops::Range;

use memchr::{memchr, memchr2, memmem};

use crate::table::{Dialect, E_CRLF, E_NONE, E_TERM};

/// A field of a record: its bytes (within the quotes, of a quoted one), whether it is quoted,
/// and whether it holds doubled quotes.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub start: usize,
    pub end: usize,
    pub quoted: bool,
    pub escaped: bool,
}

/// A record: kept as it is (its bytes, its end included), or its fields (a range of
/// [`Table::fields`]) and how it ends.
#[derive(Debug, Clone)]
pub enum Record {
    Raw(Range<usize>),
    Fields { fields: Range<usize>, end: u64 },
}

/// A file cut into records.
#[derive(Debug, Default)]
pub struct Table {
    pub records: Vec<Record>,
    pub fields: Vec<Field>,
    /// The bytes of the records kept as they are.
    pub raw: usize,
}

const UNIT_SEP: &[u8] = "\u{241f}".as_bytes();
const RECORD_SEP: &[u8] = "\u{241e}".as_bytes();

/// Cuts `d` into records of dialect `dia`, at most `limit` of them.
pub fn parse(d: &[u8], dia: &Dialect, limit: usize) -> Table {
    let mut t = Table::default();
    let mut p = 0;
    while p < d.len() && t.records.len() < limit {
        let first = t.fields.len();
        match record(d, p, dia, &mut t.fields) {
            Some((next, end)) => {
                t.records.push(Record::Fields { fields: first..t.fields.len(), end });
                p = next;
            }
            None => {
                t.fields.truncate(first);
                let next = raw_end(d, p, dia);
                t.raw += next - p;
                t.records.push(Record::Raw(p..next));
                p = next;
            }
        }
    }
    t
}

/// Where the record at `p` that is kept as it is ends: after its line's end, or after the
/// terminator, or at the end of `d`.
fn raw_end(d: &[u8], p: usize, dia: &Dialect) -> usize {
    match dia.term.is_empty() {
        true => memchr(b'\n', &d[p..]).map_or(d.len(), |k| p + k + 1),
        false => memmem::find(&d[p..], &dia.term).map_or(d.len(), |k| p + k + dia.term.len()),
    }
}

/// The record at `p`, its fields pushed to `fields`: where the next one starts, and how it
/// ends ([`E_TERM`] and the like). None where it is not one of the dialect: a quoted field not
/// closed, or followed by other bytes than a delimiter or the record's end, or a field holding
/// a zero byte.
fn record(d: &[u8], mut p: usize, dia: &Dialect, fields: &mut Vec<Field>) -> Option<(usize, u64)> {
    let line = dia.term.is_empty();
    let end_byte = if line { b'\n' } else { dia.term[0] };
    // How the record ends at `k`, and the byte after: None if neither a delimiter nor its end.
    let after = |k: usize| -> Option<Option<(usize, u64)>> {
        if k == d.len() {
            Some(Some((k, E_NONE)))
        } else if d[k..].starts_with(&dia.delim) {
            Some(None)
        } else if line && d[k] == b'\n' {
            Some(Some((k + 1, E_TERM)))
        } else if line && d[k..].starts_with(b"\r\n") {
            Some(Some((k + 2, E_CRLF)))
        } else if !line && d[k..].starts_with(&dia.term) {
            Some(Some((k + dia.term.len(), E_TERM)))
        } else {
            None
        }
    };
    loop {
        if let Some(q) = dia.quote
            && d.get(p) == Some(&q)
        {
            let mut i = p + 1;
            let mut escaped = false;
            let close = loop {
                let k = i + memchr(q, &d[i..])?;
                if d.get(k + 1) == Some(&q) {
                    escaped = true;
                    i = k + 2;
                } else {
                    break k;
                }
            };
            if memchr(0, &d[p + 1..close]).is_some() {
                return None;
            }
            fields.push(Field { start: p + 1, end: close, quoted: true, escaped });
            match after(close + 1)? {
                Some(r) => return Some(r),
                None => p = close + 1 + dia.delim.len(),
            }
            continue;
        }
        // an unquoted field: up to the first delimiter or end
        let mut i = p;
        let (stop, r) = loop {
            let Some(k) = memchr2(dia.delim[0], end_byte, &d[i..]).map(|k| i + k) else {
                break (d.len(), Some((d.len(), E_NONE)));
            };
            if d[k..].starts_with(&dia.delim) {
                break (k, None);
            }
            if line {
                // (d[k] is LF)
                match k > p && d[k - 1] == b'\r' {
                    true => break (k - 1, Some((k + 1, E_CRLF))),
                    false => break (k, Some((k + 1, E_TERM))),
                }
            }
            if d[k..].starts_with(&dia.term) {
                break (k, Some((k + dia.term.len(), E_TERM)));
            }
            i = k + 1;
        };
        if memchr(0, &d[p..stop]).is_some() {
            return None;
        }
        fields.push(Field { start: p, end: stop, quoted: false, escaped: false });
        match r {
            Some(r) => return Some(r),
            None => p = stop + dia.delim.len(),
        }
    }
}

/// The records of a sample that tell its dialect.
const SAMPLE_RECORDS: usize = 1000;

/// The dialect of a table starting with `s` (a sample of the file, `whole` if it is all of
/// it), if any: of the candidates, those whose records are cut into the same number of fields
/// (two or more) at least 9 in 10 times, with at most 1 in 50 kept as they are, at least 8 of
/// them (all of them in a short file); the one of the most consistent records, then of the
/// most fields.
pub fn dialect(s: &[u8], whole: bool) -> Option<Dialect> {
    if memchr(0, s).is_some() {
        return None;
    }
    let term = |sep: &[u8]| -> Vec<u8> {
        let n = memmem::find_iter(s, sep).count();
        let nl = memmem::find_iter(s, &[sep, b"\n"].concat()).count();
        if n > 0 && nl * 10 >= n * 9 { [sep, b"\n"].concat() } else { sep.to_vec() }
    };
    let q = Some(b'"');
    let mut cands = Vec::new();
    for delim in [&b","[..], b"\t", b";", b"|"] {
        for quote in [q, None] {
            cands.push(Dialect { delim: delim.to_vec(), quote, term: Vec::new() });
        }
    }
    if memmem::find(s, UNIT_SEP).is_some() {
        cands.push(Dialect { delim: UNIT_SEP.to_vec(), quote: None, term: term(RECORD_SEP) });
    }
    if memchr(0x1f, s).is_some() {
        cands.push(Dialect { delim: vec![0x1f], quote: None, term: term(&[0x1e]) });
    }
    let mut best: Option<((u64, usize), Dialect)> = None;
    for dia in cands {
        let mut t = parse(s, &dia, SAMPLE_RECORDS);
        // (the last record cut by the sample's end)
        if !whole && t.records.len() > 1 && matches!(t.records.last(), Some(Record::Fields { end: E_NONE, .. })) {
            t.records.pop();
        }
        let n = t.records.len();
        let raw = t.records.iter().filter(|r| matches!(r, Record::Raw(_))).count();
        let mut hist = std::collections::HashMap::<usize, usize>::new();
        for r in &t.records {
            if let Record::Fields { fields, .. } = r {
                *hist.entry(fields.len()).or_default() += 1;
            }
        }
        let Some((&width, &count)) = hist.iter().max_by_key(|&(w, c)| (c, w)) else { continue };
        let least = if whole { n.clamp(2, 8) } else { 8 };
        if width < 2 || count * 10 < n * 9 || raw * 50 > n || n < least {
            continue;
        }
        let key = ((count * 1000 / n) as u64, width);
        if best.as_ref().is_none_or(|(k, _)| key > *k) {
            best = Some((key, dia));
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dia(s: &str) -> Option<(Vec<u8>, Option<u8>, Vec<u8>)> {
        dialect(s.as_bytes(), true).map(|d| (d.delim, d.quote, d.term))
    }

    #[test]
    fn dialects() {
        let csv = "a,b,c\n1,\"x, y\",3\n".repeat(10);
        assert_eq!(dia(&csv), Some((b",".to_vec(), Some(b'"'), vec![])));
        let tsv = "a\tb\tc\r\n1\t2,3\t4\r\n".repeat(10);
        assert_eq!(dia(&tsv).map(|d| d.0), Some(b"\t".to_vec()));
        let usv = "a␟b␟c␞\n1␟2␟3␞\n".repeat(10);
        assert_eq!(dia(&usv), Some(("␟".as_bytes().to_vec(), None, "␞\n".as_bytes().to_vec())));
        let usv = "a␟b␞1␟2␞".repeat(10);
        assert_eq!(dia(&usv).map(|d| d.2), Some("␞".as_bytes().to_vec()));
        assert_eq!(dia("just some words, and more\nof them\nand, then, some, more\nand the end\n"), None);
        assert_eq!(dia(&"int main(void) {\n  return f(a, b);\n}\n".repeat(10)), None);
    }

    #[test]
    fn records() {
        let d = Dialect { delim: b",".to_vec(), quote: Some(b'"'), term: vec![] };
        let s = b"a,\"b\"\"c\",\r\n\"x\ny\",2\n\"bad\"x,1\nlast";
        let t = parse(s, &d, usize::MAX);
        let f = |r: &Range<usize>| -> Vec<&[u8]> { t.fields[r.clone()].iter().map(|f| &s[f.start..f.end]).collect() };
        let Record::Fields { fields, end } = &t.records[0] else { panic!() };
        assert_eq!((f(fields), *end), (vec![&b"a"[..], b"b\"\"c", b""], E_CRLF));
        assert!(t.fields[fields.start + 1].escaped);
        let Record::Fields { fields, end } = &t.records[1] else { panic!() };
        assert_eq!((f(fields), *end), (vec![&b"x\ny"[..], b"2"], E_TERM));
        assert!(matches!(&t.records[2], Record::Raw(r) if &s[r.clone()] == b"\"bad\"x,1\n"));
        let Record::Fields { fields, end } = &t.records[3] else { panic!() };
        assert_eq!((f(fields), *end), (vec![&b"last"[..]], E_NONE));
    }
}
