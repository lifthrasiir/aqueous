//! A FASTA file taken apart (FORMAT.md, "Parts"): records of a header line (`>` and anything
//! up to the line's end) and lines of letters, the sequence. Apart from the bases A, C, G and
//! T (case aside, [`crate::bases`]) are kept the headers, the layout of each record's lines
//! (of the file's usual width, else told line by line), the case of the letters (runs) and the
//! other letters (runs of one letter: N runs, IUPAC codes). Lines end with LF; a file of CR,
//! of anything before the first header or of letters other than ASCII ones is not taken, nor
//! one of more runs of other letters than one per 64 letters (and 16; proteins).

use aqr_common::varint;

use crate::{Result, corrupt};

/// A FASTA file's parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parts {
    /// The usual length of a sequence line.
    pub width: u64,
    /// Whether the file ends with a line's end.
    pub final_newline: bool,
    /// The headers, without their `>`, each ended by LF.
    pub headers: Vec<u8>,
    /// Of each record: its letters (varint), then 0 where its lines are of `width` (but the
    /// last, of what is left), else 1, the number of lines and the length of each (varints).
    pub layout: Vec<u8>,
    /// The runs of upper and lower case letters, alternately, from an upper one (varints).
    pub case: Vec<u8>,
    /// The runs of letters but A, C, G and T (case aside): the A, C, G and T letters before it
    /// since the last run (varint), its length (varint) and its letter, in upper case.
    pub other: Vec<u8>,
    /// The bases, 0 to 3 for A, C, G and T.
    pub bases: Vec<u8>,
}

/// [`Parts`] borrowed, as a module's inputs are.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    pub width: u64,
    pub final_newline: bool,
    pub headers: &'a [u8],
    pub layout: &'a [u8],
    pub case: &'a [u8],
    pub other: &'a [u8],
}

impl Parts {
    pub fn view(&self) -> View<'_> {
        View {
            width: self.width,
            final_newline: self.final_newline,
            headers: &self.headers,
            layout: &self.layout,
            case: &self.case,
            other: &self.other,
        }
    }
}

/// The base of a letter (in upper case), if one.
#[inline(always)]
fn base(u: u8) -> Option<u8> {
    match u {
        b'A' => Some(0),
        b'C' => Some(1),
        b'G' => Some(2),
        b'T' => Some(3),
        _ => None,
    }
}

const LETTERS: [u8; 4] = *b"ACGT";

/// What a byte is, for the parse: the base of A, C, G and T (0 to 3), `OTHER` for another
/// letter, with `LOWER` for a lower case one; `NOT_A_LETTER` else.
const LOWER: u8 = 8;
const OTHER: u8 = 4;
const NOT_A_LETTER: u8 = 0xff;
const CLASS: [u8; 256] = {
    let mut t = [NOT_A_LETTER; 256];
    let mut c = 0;
    while c < 26 {
        let (u, l) = (b'A' + c, b'a' + c);
        let k = match u {
            b'A' => 0,
            b'C' => 1,
            b'G' => 2,
            b'T' => 3,
            _ => OTHER,
        };
        (t[u as usize], t[l as usize]) = (k, k | LOWER);
        c += 1;
    }
    t
};

/// The parts of FASTA file `d`, or `None` where it is not one this crate takes (see the module
/// docs).
pub fn parse(d: &[u8]) -> Option<Parts> {
    if d.first() != Some(&b'>') || d.contains(&b'\r') {
        return None;
    }
    let final_newline = d.last() == Some(&b'\n');
    let body = if final_newline { &d[..d.len() - 1] } else { d };
    // the records: a header, then lines
    let lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();
    let mut widths = std::collections::HashMap::<usize, u64>::new();
    for l in &lines {
        if l.first() != Some(&b'>') {
            *widths.entry(l.len()).or_default() += 1;
        }
    }
    let width = widths.iter().max_by_key(|&(&w, &n)| (n, w)).map_or(0, |(&w, _)| w) as u64;
    let mut p = Parts { width, final_newline, bases: Vec::with_capacity(d.len()), ..Parts::default() };
    let mut i = 0;
    let (mut upper, mut run, mut letters) = (true, 0u64, 0u64);
    // the other letters' current run: its letter and length, and the bases before it
    let (mut other, mut olen, mut gap, mut runs) = (0u8, 0u64, 0u64, 0u64);
    while i < lines.len() {
        let h = lines[i];
        if h.first() != Some(&b'>') {
            return None;
        }
        p.headers.extend_from_slice(&h[1..]);
        p.headers.push(b'\n');
        i += 1;
        let start = i;
        while i < lines.len() && lines[i].first() != Some(&b'>') {
            i += 1;
        }
        let seq = &lines[start..i];
        let n: u64 = seq.iter().map(|l| l.len() as u64).sum();
        varint::put(&mut p.layout, n);
        let regular = width > 0 && seq.len() as u64 == n.div_ceil(width) && {
            let (last, rest) = seq.split_last().map_or((&[][..], &[][..]), |(l, r)| (*l, r));
            rest.iter().all(|l| l.len() as u64 == width) && !last.is_empty()
        };
        if regular || seq.is_empty() {
            p.layout.push(0);
        } else {
            p.layout.push(1);
            varint::put(&mut p.layout, seq.len() as u64);
            seq.iter().for_each(|l| varint::put(&mut p.layout, l.len() as u64));
        }
        for l in seq {
            for &c in *l {
                let k = CLASS[c as usize];
                if k == NOT_A_LETTER {
                    return None;
                }
                let up = k & LOWER == 0;
                if up != upper {
                    varint::put(&mut p.case, run);
                    (upper, run) = (up, 0);
                }
                run += 1;
                let u = c.to_ascii_uppercase();
                match k & !LOWER {
                    b @ 0..4 => {
                        if olen > 0 {
                            varint::put(&mut p.other, gap);
                            varint::put(&mut p.other, olen);
                            p.other.push(other);
                            (olen, gap, runs) = (0, 0, runs + 1);
                        }
                        p.bases.push(b);
                        gap += 1;
                    }
                    _ if olen > 0 && u == other => olen += 1,
                    _ => {
                        if olen > 0 {
                            varint::put(&mut p.other, gap);
                            varint::put(&mut p.other, olen);
                            p.other.push(other);
                            (gap, runs) = (0, runs + 1);
                        }
                        (other, olen) = (u, 1);
                    }
                }
            }
            letters += l.len() as u64;
        }
    }
    if olen > 0 {
        varint::put(&mut p.other, gap);
        varint::put(&mut p.other, olen);
        p.other.push(other);
        runs += 1;
    }
    varint::put(&mut p.case, run);
    // (proteins: letters of their own almost everywhere)
    if runs > letters / 64 + 16 {
        return None;
    }
    Some(p)
}

/// Reads varints.
struct Rd<'a> {
    d: &'a [u8],
    at: usize,
}

impl Rd<'_> {
    fn num(&mut self) -> Result<u64> {
        varint::get(self.d, &mut self.at).map_err(|_| corrupt())
    }

    fn done(&self) -> bool {
        self.at == self.d.len()
    }
}

/// The number of bases of parts whose layout and other letters are `layout` and `other`: their
/// letters, but the other ones.
pub fn bases_len(layout: &[u8], other: &[u8]) -> Result<u64> {
    let mut letters = 0u64;
    let mut r = Rd { d: layout, at: 0 };
    while !r.done() {
        letters = letters.checked_add(r.num()?).ok_or_else(corrupt)?;
        if r.d.get(r.at) == Some(&1) {
            r.at += 1;
            let n = r.num()?;
            for _ in 0..n {
                r.num()?;
            }
        } else {
            r.at += 1;
        }
    }
    let mut o = Rd { d: other, at: 0 };
    let mut others = 0u64;
    while !o.done() {
        o.num()?;
        others = others.checked_add(o.num()?).ok_or_else(corrupt)?;
        o.at += 1;
    }
    letters.checked_sub(others).ok_or_else(corrupt)
}

/// The file of `p` and `bases` (taken as they come), of `len` bytes; fails where they do not
/// make one of that length.
pub fn build(p: View, mut bases: impl Iterator<Item = u8>, len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    let mut layout = Rd { d: p.layout, at: 0 };
    let mut case = Rd { d: p.case, at: 0 };
    let mut other = Rd { d: p.other, at: 0 };
    let mut headers = p.headers.split(|&b| b == b'\n');
    // the letters as they come: the bases left before the next run of another letter, and
    // the case's run left
    // (after the last run, none: as many bases as there are)
    let next_other = |o: &mut Rd| -> Result<(u64, u64, u8)> {
        if o.done() {
            return Ok((u64::MAX, 0, 0));
        }
        let (gap, n) = (o.num()?, o.num()?);
        let c = *o.d.get(o.at).ok_or_else(corrupt)?;
        o.at += 1;
        if n == 0 || base(c).is_some() || !c.is_ascii_uppercase() {
            return Err(corrupt());
        }
        Ok((gap, n, c))
    };
    let (mut gap, mut olen, mut oc) = next_other(&mut other)?;
    let (mut upper, mut run) = (true, case.num()?);
    let mut line = |out: &mut Vec<u8>, mut n: u64| -> Result<()> {
        while n > 0 {
            while run == 0 {
                (upper, run) = (!upper, case.num()?);
            }
            // a piece of one case, and of bases or another letter alone
            let mut k = n.min(run);
            let lower = if upper { 0 } else { 0x20 };
            if gap == 0 {
                k = k.min(olen);
                out.extend(std::iter::repeat_n(oc | lower, k as usize));
                olen -= k;
                if olen == 0 {
                    (gap, olen, oc) = next_other(&mut other)?;
                }
            } else {
                k = k.min(gap);
                for _ in 0..k {
                    let b = bases.next().ok_or_else(corrupt)?;
                    out.push(LETTERS[b as usize & 3] | lower);
                }
                gap = gap.saturating_sub(k);
            }
            (n, run) = (n - k, run - k);
        }
        Ok(())
    };
    let mut first = true;
    while !layout.done() {
        let h = headers.next().ok_or_else(corrupt)?;
        if !first {
            out.push(b'\n');
        }
        first = false;
        out.push(b'>');
        out.extend_from_slice(h);
        let n = layout.num()?;
        let kind = *layout.d.get(layout.at).ok_or_else(corrupt)?;
        layout.at += 1;
        match kind {
            0 if n == 0 => {}
            0 => {
                if p.width == 0 {
                    return Err(corrupt());
                }
                let mut left = n;
                while left > 0 {
                    let k = left.min(p.width);
                    out.push(b'\n');
                    line(&mut out, k)?;
                    left -= k;
                }
            }
            1 => {
                let lines = layout.num()?;
                let mut total = 0u64;
                for _ in 0..lines {
                    let k = layout.num()?;
                    total = total.checked_add(k).ok_or_else(corrupt)?;
                    out.push(b'\n');
                    line(&mut out, k)?;
                }
                if total != n {
                    return Err(corrupt());
                }
            }
            _ => return Err(corrupt()),
        }
        if out.len() > len {
            return Err(corrupt());
        }
    }
    if p.final_newline {
        out.push(b'\n');
    }
    // everything taken: the headers, the case, the bases and the other letters
    let rest = headers.next();
    if out.len() != len
        || rest.is_some_and(|r| !r.is_empty())
        || headers.next().is_some()
        || bases.next().is_some()
        || !case.done()
        || run != 0
        || olen != 0
        || !other.done()
    {
        return Err(corrupt());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn back(d: &[u8]) {
        let p = parse(d).unwrap_or_else(|| panic!("not taken: {:?}", String::from_utf8_lossy(d)));
        assert_eq!(bases_len(&p.layout, &p.other).unwrap(), p.bases.len() as u64);
        assert_eq!(build(p.view(), p.bases.iter().copied(), d.len()).unwrap(), d, "{p:?}");
        assert!(build(p.view(), p.bases.iter().copied(), d.len() + 1).is_err());
    }

    #[test]
    fn roundtrips() {
        back(b">a\nACGT\nAC\n");
        back(b">a b c\nACGTacgtNNNNnnnnACGT\nAC");
        back(b">a\n>b\nAC\n\n>c\nRYKMacgu\n");
        back(b">\n");
        back(b">x");
        back(b">a\nACG\nACGT\nA\n>b\nNNNN\nNNNN\nNA\n");
        back(b">a\nACGT\nACGT\nAC\n>b\nAC\nGT\nAC\n");
        let mut big = b">chr\n".to_vec();
        for i in 0..1000 {
            // (N in long runs, as in assemblies)
            let line: Vec<u8> =
                (0..50).map(|j| if i % 100 < 3 { b'N' } else { b"ACGTacgtGa"[(i * 7 + j * 3 + j / 7) % 10] }).collect();
            big.extend_from_slice(&line);
            big.push(b'\n');
        }
        back(&big);
    }

    #[test]
    fn turned_down() {
        for d in [&b"ACGT\n"[..], b">a\r\nACGT\r\n", b">a\nAC GT\n", b">a\nAC1\n", b""] {
            assert!(parse(d).is_none(), "{:?}", String::from_utf8_lossy(d));
        }
        // a protein
        let p: Vec<u8> = b">p\n".iter().copied().chain((0..1000).map(|i| b"MKVLAWQERTYIPSDFHN"[i % 18])).collect();
        assert!(parse(&p).is_none());
    }
}
