//! The structure of a FITS file (the FITS Standard 4.0, §3–§4): header and data units (HDUs),
//! each a header of 80-byte cards in 2880-byte blocks up to the `END` card, then the data, padded
//! to a whole block.

/// Bytes of a block, the unit of headers and padded data.
pub const BLOCK: usize = 2880;
/// Bytes of a card.
pub const CARD: usize = 80;

/// A header: its cards' keywords and values, as far as this crate reads them.
pub struct Header<'a> {
    cards: &'a [u8],
}

/// A value of a card (§4.2), as far as this crate reads them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Logical(bool),
    Int(i64),
    /// The string, its quotes undone and its trailing spaces left out.
    Str(String),
    /// Anything else (a real or complex number), as it is.
    Other(&'a [u8]),
}

impl<'a> Header<'a> {
    /// The cards of `d` (its whole blocks up to the `END` card).
    pub fn new(d: &'a [u8]) -> Header<'a> {
        Header { cards: d }
    }

    pub fn cards(&self) -> impl Iterator<Item = &'a [u8]> {
        self.cards.chunks_exact(CARD)
    }

    /// The value of the first card of keyword `key`.
    pub fn get(&self, key: &str) -> Option<Value<'a>> {
        self.cards().find(|c| keyword(c) == key.as_bytes()).and_then(value)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            Value::Int(v) => Some(v),
            _ => None,
        }
    }

    pub fn logical(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            Value::Logical(v) => Some(v),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            Value::Str(v) => Some(v),
            _ => None,
        }
    }

    /// `NAXISn` for n = 1 to `NAXIS`, if all are there and not negative.
    pub fn axes(&self, prefix: &str) -> Option<Vec<u64>> {
        let n = self.int(prefix)?;
        if !(0..=999).contains(&n) {
            return None;
        }
        (1..=n).map(|i| self.int(&format!("{prefix}{i}")).and_then(|v| u64::try_from(v).ok())).collect()
    }
}

/// The keyword of card `c`, its trailing spaces left out.
pub fn keyword(c: &[u8]) -> &[u8] {
    let k = &c[..8];
    &k[..k.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1)]
}

/// The value of card `c`, if it has a value indicator (`= ` in bytes 9–10).
pub fn value(c: &[u8]) -> Option<Value<'_>> {
    if &c[8..10] != b"= " {
        return None;
    }
    let v = &c[10..];
    let start = v.iter().position(|&b| b != b' ')?;
    let v = &v[start..];
    if v[0] == b'\'' {
        // a string: '' is a quote; it ends at a single quote
        let mut s = Vec::new();
        let mut i = 1;
        loop {
            match v.get(i)? {
                b'\'' if v.get(i + 1) == Some(&b'\'') => {
                    s.push(b'\'');
                    i += 2;
                }
                b'\'' => break,
                &b => {
                    s.push(b);
                    i += 1;
                }
            }
        }
        while s.last() == Some(&b' ') {
            s.pop();
        }
        return Some(Value::Str(String::from_utf8_lossy(&s).into_owned()));
    }
    let end = v.iter().position(|&b| b == b'/' || b == b' ').unwrap_or(v.len());
    let tok = &v[..end];
    match tok {
        b"T" => Some(Value::Logical(true)),
        b"F" => Some(Value::Logical(false)),
        _ => match std::str::from_utf8(tok).ok().and_then(|s| s.parse::<i64>().ok()) {
            Some(i) => Some(Value::Int(i)),
            None => Some(Value::Other(tok)),
        },
    }
}

/// An HDU: where its header and data are in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hdu {
    /// The header's blocks.
    pub header: std::ops::Range<usize>,
    /// The data, without the padding (perhaps past the end of a truncated file: see
    /// [`parse`]).
    pub data: std::ops::Range<usize>,
    /// The end of the padding after the data, a whole block (or the file's end).
    pub end: usize,
}

/// The HDUs of FITS file `d`, one after another from its start, and where they end: what is
/// after them (a truncated HDU, or bytes that are not one) is not an HDU. None where `d` does
/// not start with a primary header.
pub fn parse(d: &[u8]) -> Option<(Vec<Hdu>, usize)> {
    if !starts(d) {
        return None;
    }
    let mut hdus = Vec::new();
    let mut at = 0;
    while at < d.len() {
        let Some(h) = hdu(d, at, hdus.is_empty()) else { break };
        at = h.end;
        hdus.push(h);
    }
    (!hdus.is_empty()).then_some((hdus, at))
}

/// Whether `d` starts with a FITS primary header: `SIMPLE  =` then `T`, right-justified in
/// column 30 (§4.4.1.1).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= BLOCK && d.starts_with(b"SIMPLE  = ") && d[10..29].iter().all(|&b| b == b' ') && d[29] == b'T'
}

/// The HDU at `at` (whole of it in `d`), primary or an extension.
fn hdu(d: &[u8], at: usize, primary: bool) -> Option<Hdu> {
    let first = d.get(at..at + CARD)?;
    let ok = if primary { first.starts_with(b"SIMPLE  = ") } else { first.starts_with(b"XTENSION= ") };
    if !ok {
        return None;
    }
    // the header's blocks, up to the one with the END card
    let mut end = at;
    loop {
        let block = d.get(end..end + BLOCK)?;
        end += BLOCK;
        if block.chunks_exact(CARD).any(|c| keyword(c) == b"END") {
            break;
        }
    }
    let h = Header::new(&d[at..end]);
    let len = data_len(&h)?;
    let data = end.checked_add(usize::try_from(len).ok()?)?;
    let padded = data.checked_next_multiple_of(BLOCK)?;
    if padded > d.len() {
        return None;
    }
    Some(Hdu { header: at..end, data: end..data, end: padded })
}

/// The bytes of the data of header `h` (§4.4.1.1, §4.4.1.2, §6): |BITPIX| / 8 × GCOUNT ×
/// (PCOUNT + NAXIS1 × ... × NAXISn), NAXIS1 left out for random groups.
pub fn data_len(h: &Header) -> Option<u64> {
    let bitpix = h.int("BITPIX")?;
    if ![8, 16, 32, 64, -32, -64].contains(&bitpix) {
        return None;
    }
    let axes = h.axes("NAXIS")?;
    if axes.is_empty() {
        return Some(0);
    }
    let groups = h.logical("GROUPS") == Some(true) && axes[0] == 0;
    let n = axes[groups as usize..].iter().try_fold(1u64, |a, &x| a.checked_mul(x))?;
    let pcount = u64::try_from(h.int("PCOUNT").unwrap_or(0)).ok()?;
    let gcount = u64::try_from(h.int("GCOUNT").unwrap_or(1)).ok()?;
    (bitpix.unsigned_abs() / 8).checked_mul(gcount)?.checked_mul(pcount.checked_add(n)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(s: &str) -> Vec<u8> {
        let mut c = s.as_bytes().to_vec();
        c.resize(CARD, b' ');
        c
    }

    fn header(cards: &[&str]) -> Vec<u8> {
        let mut h: Vec<u8> = cards.iter().flat_map(|c| card(c)).collect();
        h.extend(card("END"));
        h.resize(h.len().next_multiple_of(BLOCK), b' ');
        h
    }

    #[test]
    fn values() {
        let c = card("ZCMPTYPE= 'RICE_1  '           / compression algorithm");
        assert_eq!(value(&c), Some(Value::Str("RICE_1".into())));
        let c = card("A       = 'it''s'");
        assert_eq!(value(&c), Some(Value::Str("it's".into())));
        let c = card("NAXIS1  =                 1024 / length");
        assert_eq!(value(&c), Some(Value::Int(1024)));
        let c = card("SIMPLE  =                    T");
        assert_eq!(value(&c), Some(Value::Logical(true)));
        let c = card("BSCALE  =               0.0625");
        assert_eq!(value(&c), Some(Value::Other(b"0.0625")));
        assert_eq!(value(&card("COMMENT   x = 1")), None);
    }

    #[test]
    fn hdus() {
        let mut d = header(&[
            "SIMPLE  =                    T",
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                   10",
            "NAXIS2  =                    3",
        ]);
        d.extend([0u8; BLOCK]);
        let ext = header(&[
            "XTENSION= 'BINTABLE'",
            "BITPIX  =                    8",
            "NAXIS   =                    2",
            "NAXIS1  =                    4",
            "NAXIS2  =                    2",
            "PCOUNT  =                    5",
            "GCOUNT  =                    1",
        ]);
        d.extend(&ext);
        d.extend([0u8; BLOCK]);
        d.extend(b"junk");
        let (hdus, end) = parse(&d).unwrap();
        assert_eq!(hdus.len(), 2);
        assert_eq!(hdus[0].data, BLOCK..BLOCK + 60);
        assert_eq!(hdus[1].data, 3 * BLOCK..3 * BLOCK + 13);
        assert_eq!(end, 4 * BLOCK);
    }
}
