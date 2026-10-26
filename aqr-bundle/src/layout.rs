//! The layout (`FORMAT.md`, "The layout"): the records of an archive, the members' headers XOR
//! coded against what their contents predict, and [`build`], which rebuilds the archive from it
//! and the contents. [`Writer`] makes it from the members the readers find.

use std::ops::Range;

use aqr_common::varint;

use crate::{Error, Result, corrupt};

pub const GAP: u8 = 0;
pub const ZEROS: u8 = 1;
pub const AR: u8 = 2;
pub const TAR: u8 = 3;
pub const CPIO: u8 = 4;

/// The body is in the layout, not in the data.
pub const INLINE: u8 = 1;
/// ar: a body of odd size without the pad byte after it. tar and cpio: the padding is in the
/// layout, as it is, rather than zeros.
pub const PAD: u8 = 2;
/// cpio (newc and crc): the hexadecimal fields predicted in upper case.
pub const UPPER: u8 = 4;

pub const AR_HEADER: usize = 60;
pub const TAR_BLOCK: usize = 512;

/// The forms of cpio headers, by their number in the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cpio {
    /// `070701`: hexadecimal fields, 4-byte alignment.
    Newc,
    /// `070702`: newc with the checksum of the body.
    Crc,
    /// `070707`: octal fields, no alignment.
    Odc,
    /// Binary, 16-bit words little endian, 2-byte alignment.
    BinLe,
    /// Binary, big endian.
    BinBe,
}

impl Cpio {
    pub const ALL: [Cpio; 5] = [Cpio::Newc, Cpio::Crc, Cpio::Odc, Cpio::BinLe, Cpio::BinBe];

    pub fn header(self) -> usize {
        match self {
            Cpio::Newc | Cpio::Crc => 110,
            Cpio::Odc => 76,
            Cpio::BinLe | Cpio::BinBe => 26,
        }
    }

    /// The alignment of the name's end and the body's end, by position in the archive.
    pub fn align(self) -> usize {
        match self {
            Cpio::Newc | Cpio::Crc => 4,
            Cpio::Odc => 1,
            Cpio::BinLe | Cpio::BinBe => 2,
        }
    }

    fn from_u8(b: u8) -> Option<Cpio> {
        Cpio::ALL.get(b as usize).copied()
    }
}

/// Bytes from `at` up to the next multiple of `align`.
pub fn pad_to(at: usize, align: usize) -> usize {
    (align - at % align) % align
}

/// `v` in decimal, left aligned and padded with spaces, in `out` (its last digits where it does
/// not fit).
fn decimal(v: u64, out: &mut [u8]) {
    let s = v.to_string();
    let s = &s.as_bytes()[s.len().saturating_sub(out.len())..];
    out.fill(b' ');
    out[..s.len()].copy_from_slice(s);
}

/// `v` in base `radix` (8 or 16), its last `out.len()` digits padded with zeros.
fn digits(mut v: u64, radix: u64, upper: bool, out: &mut [u8]) {
    let hex: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    for o in out.iter_mut().rev() {
        *o = hex[(v % radix) as usize];
        v /= radix;
    }
}

fn xor(field: &mut [u8], pred: &[u8]) {
    field.iter_mut().zip(pred).for_each(|(f, p)| *f ^= p);
}

/// The size field of an ar header for a member of `n` bytes.
fn ar_size(n: u64) -> [u8; 10] {
    let mut f = [0; 10];
    decimal(n, &mut f);
    f
}

/// The size field of a tar header for a body of `n` bytes: 11 octal digits and a NUL, or
/// base-256 where it does not fit.
fn tar_size(n: u64) -> [u8; 12] {
    let mut f = [0; 12];
    if n < 1 << 33 {
        digits(n, 8, false, &mut f[..11]);
    } else {
        f[0] = 0x80;
        f[4..].copy_from_slice(&n.to_be_bytes());
    }
    f
}

/// The checksum field of tar header `h`: its bytes summed, the field taken as spaces, as 6 octal
/// digits, a NUL and a space.
fn tar_sum(h: &[u8]) -> [u8; 8] {
    let sum: u32 = h[..148].iter().chain(&[b' '; 8]).chain(&h[156..TAR_BLOCK]).map(|&b| b as u32).sum();
    let mut f = *b"000000\0 ";
    digits(sum as u64, 8, false, &mut f[..6]);
    f
}

/// Codes cpio header `h` (of `form`) against its prediction: the name's size `m`, the body's
/// size `n`, and the body's checksum `sum`.
fn cpio_code(form: Cpio, upper: bool, h: &mut [u8], m: u64, n: u64, sum: u32) {
    match form {
        Cpio::Newc | Cpio::Crc => {
            let mut p = [0; 8];
            for (at, v) in [(54, n), (94, m), (102, if form == Cpio::Crc { sum as u64 } else { 0 })] {
                digits(v, 16, upper, &mut p);
                xor(&mut h[at..at + 8], &p);
            }
        }
        Cpio::Odc => {
            let (mut p6, mut p11) = ([0; 6], [0; 11]);
            digits(m, 8, false, &mut p6);
            digits(n, 8, false, &mut p11);
            xor(&mut h[59..65], &p6);
            xor(&mut h[65..76], &p11);
        }
        Cpio::BinLe | Cpio::BinBe => {
            let word = |v: u64| if form == Cpio::BinLe { (v as u16).to_le_bytes() } else { (v as u16).to_be_bytes() };
            xor(&mut h[20..22], &word(m));
            xor(&mut h[22..24], &word(n >> 16));
            xor(&mut h[24..26], &word(n));
        }
    }
}

/// The checksum of cpio's crc form: the bytes summed.
pub fn byte_sum(d: &[u8]) -> u32 {
    d.iter().fold(0u32, |s, &b| s.wrapping_add(b as u32))
}

/// What a member's body is to the archive's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A file's contents: data, of the kind of the member's name.
    File,
    /// An index of the archive (an ar symbol table): data, of fixed layout.
    Table,
    /// What tells of other members (long names, pax headers): in the layout, with the headers.
    Meta,
}

/// A member's body as the readers find it.
#[derive(Debug, Clone)]
pub struct Body {
    pub span: Range<usize>,
    pub role: Role,
    /// The member's name, as far as known (for the kind of its contents).
    pub name: Vec<u8>,
}

/// A record of an archive, as the readers find it (positions in the archive).
#[derive(Debug, Clone)]
pub enum Rec {
    Gap(Range<usize>),
    Zeros(Range<usize>),
    /// The header at `at`, `pre` bytes of name after it (BSD), the body, and whether a pad byte
    /// follows a body of odd size.
    Ar {
        at: usize,
        pre: usize,
        body: Body,
        pad: bool,
    },
    /// The header at `at`, the body, then its padding.
    Tar {
        at: usize,
        body: Body,
    },
    /// The header at `at`, the name of `m` bytes after it, then the body, each padded.
    Cpio {
        at: usize,
        form: Cpio,
        m: usize,
        body: Body,
    },
}

impl Rec {
    /// The member's body, if any.
    pub fn body(&self) -> Option<&Body> {
        match self {
            Rec::Ar { body, .. } | Rec::Tar { body, .. } | Rec::Cpio { body, .. } => Some(body),
            _ => None,
        }
    }
}

/// The flags of a member: [`INLINE`], [`PAD`] and [`UPPER`] where set.
fn flags(inline: bool, pad: bool, upper: bool) -> u8 {
    (if inline { INLINE } else { 0 }) | (if pad { PAD } else { 0 }) | (if upper { UPPER } else { 0 })
}

/// Makes the layout and the data of records, from the archive `d`.
pub struct Writer<'a> {
    d: &'a [u8],
    pub layout: Vec<u8>,
    /// The bodies that are data, in order: their spans in `d` and roles.
    pub data: Vec<(Range<usize>, Role, usize)>,
}

impl<'a> Writer<'a> {
    pub fn new(d: &'a [u8]) -> Writer<'a> {
        Writer { d, layout: Vec::new(), data: Vec::new() }
    }

    /// Writes `r`, the `i`-th record.
    pub fn rec(&mut self, r: &Rec, i: usize) {
        let d = self.d;
        let l = &mut self.layout;
        match r {
            Rec::Gap(g) => {
                l.push(GAP);
                varint::put(l, g.len() as u64);
                l.extend_from_slice(&d[g.clone()]);
            }
            Rec::Zeros(z) => {
                l.push(ZEROS);
                varint::put(l, z.len() as u64);
            }
            &Rec::Ar { at, pre, ref body, pad } => {
                let n = body.span.len();
                let odd = (pre + n) % 2 == 1;
                let inline = body.role == Role::Meta;
                l.extend_from_slice(&[AR, flags(inline, odd && !pad, false)]);
                varint::put(l, pre as u64);
                varint::put(l, n as u64);
                let mut h: [u8; AR_HEADER] = d[at..at + AR_HEADER].try_into().unwrap();
                xor(&mut h[48..58], &ar_size((pre + n) as u64));
                l.extend_from_slice(&h);
                l.extend_from_slice(&d[at + AR_HEADER..][..pre]);
                if inline {
                    l.extend_from_slice(&d[body.span.clone()]);
                }
                if odd && pad {
                    l.push(d[body.span.end] ^ b'\n');
                }
            }
            &Rec::Tar { at, ref body } => {
                let n = body.span.len();
                let inline = body.role == Role::Meta;
                let pad = &d[body.span.end..][..pad_to(n, TAR_BLOCK)];
                let raw = pad.iter().any(|&b| b != 0);
                l.extend_from_slice(&[TAR, flags(inline, raw, false)]);
                varint::put(l, n as u64);
                let mut h: [u8; TAR_BLOCK] = d[at..at + TAR_BLOCK].try_into().unwrap();
                let sum = tar_sum(&h);
                xor(&mut h[148..156], &sum);
                xor(&mut h[124..136], &tar_size(n as u64));
                l.extend_from_slice(&h);
                if inline {
                    l.extend_from_slice(&d[body.span.clone()]);
                }
                if raw {
                    l.extend_from_slice(pad);
                }
            }
            &Rec::Cpio { at, form, m, ref body } => {
                let (hl, align) = (form.header(), form.align());
                let n = body.span.len();
                let inline = body.role == Role::Meta;
                let name = at + hl..at + hl + m;
                let name_pad = name.end..name.end + pad_to(name.end, align);
                let body_pad = body.span.end..body.span.end + pad_to(body.span.end, align);
                let raw = d[name_pad.clone()].iter().chain(&d[body_pad.clone()]).any(|&b| b != 0);
                let mut h = d[at..at + hl].to_vec();
                // (upper case where its fields have letters of upper case only)
                let upper = matches!(form, Cpio::Newc | Cpio::Crc)
                    && h[6..].iter().any(u8::is_ascii_uppercase)
                    && !h[6..].iter().any(u8::is_ascii_lowercase);
                l.extend_from_slice(&[CPIO, form as u8, flags(inline, raw, upper)]);
                varint::put(l, m as u64);
                varint::put(l, n as u64);
                cpio_code(form, upper, &mut h, m as u64, n as u64, byte_sum(&d[body.span.clone()]));
                l.extend_from_slice(&h);
                l.extend_from_slice(&d[name]);
                if raw {
                    l.extend_from_slice(&d[name_pad]);
                }
                if inline {
                    l.extend_from_slice(&d[body.span.clone()]);
                }
                if raw {
                    l.extend_from_slice(&d[body_pad]);
                }
            }
        }
        if let Some(b) = r.body().filter(|b| b.role != Role::Meta) {
            self.data.push((b.span.clone(), b.role, i));
        }
    }
}

/// Reads the layout.
struct Reader<'a> {
    l: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8> {
        let b = *self.l.get(self.at).ok_or_else(corrupt)?;
        self.at += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<usize> {
        usize::try_from(varint::get(self.l, &mut self.at)?).map_err(|_| corrupt())
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self.l.get(self.at..).and_then(|r| r.get(..n)).ok_or_else(corrupt)?;
        self.at += n;
        Ok(b)
    }
}

/// What a record of the layout is, for counting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub tag: u8,
    /// Bytes of the body, and whether it is in the layout.
    pub body: usize,
    pub inline: bool,
}

/// The archive of `len` bytes rebuilt from `layout` and `data` (the bodies that are data, in
/// order).
pub fn build(layout: &[u8], data: &[u8], len: u64) -> Result<Vec<u8>> {
    build_with(layout, data, len, &mut |_| {})
}

/// [`build`], telling `info` what each record is.
pub fn build_with(layout: &[u8], data: &[u8], len: u64, info: &mut dyn FnMut(Info)) -> Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    let mut r = Reader { l: layout, at: 0 };
    let mut taken = 0;
    // the next `n` bytes of data, or of the layout where `inline`
    fn body<'a>(r: &mut Reader<'a>, data: &'a [u8], taken: &mut usize, inline: bool, n: usize) -> Result<&'a [u8]> {
        if inline {
            return r.bytes(n);
        }
        let b = data.get(*taken..).and_then(|d| d.get(..n)).ok_or_else(corrupt)?;
        *taken += n;
        Ok(b)
    }
    let room = |out: &Vec<u8>, n: usize| {
        if out.len().saturating_add(n) > len { Err(Error("past the length")) } else { Ok(()) }
    };
    while r.at < layout.len() {
        let tag = r.u8()?;
        let (mut size, mut inline) = (0, false);
        match tag {
            GAP => {
                let n = r.varint()?;
                out.extend_from_slice(r.bytes(n)?);
            }
            ZEROS => {
                let n = r.varint()?;
                room(&out, n)?;
                out.resize(out.len() + n, 0);
            }
            AR => {
                let flags = r.u8()?;
                if flags & !(INLINE | PAD) != 0 {
                    return Err(corrupt());
                }
                let (pre, n) = (r.varint()?, r.varint()?);
                let mut h: [u8; AR_HEADER] = r.bytes(AR_HEADER)?.try_into().unwrap();
                xor(&mut h[48..58], &ar_size(pre.checked_add(n).ok_or_else(corrupt)? as u64));
                out.extend_from_slice(&h);
                out.extend_from_slice(r.bytes(pre)?);
                inline = flags & INLINE != 0;
                out.extend_from_slice(body(&mut r, data, &mut taken, inline, n)?);
                if (pre + n) % 2 == 1 && flags & PAD == 0 {
                    out.push(r.u8()? ^ b'\n');
                }
                size = n;
            }
            TAR => {
                let flags = r.u8()?;
                if flags & !(INLINE | PAD) != 0 {
                    return Err(corrupt());
                }
                let n = r.varint()?;
                let mut h: [u8; TAR_BLOCK] = r.bytes(TAR_BLOCK)?.try_into().unwrap();
                xor(&mut h[124..136], &tar_size(n as u64));
                let sum = tar_sum(&h);
                xor(&mut h[148..156], &sum);
                out.extend_from_slice(&h);
                inline = flags & INLINE != 0;
                out.extend_from_slice(body(&mut r, data, &mut taken, inline, n)?);
                let p = pad_to(n, TAR_BLOCK);
                match flags & PAD {
                    0 => out.resize(out.len() + p, 0),
                    _ => out.extend_from_slice(r.bytes(p)?),
                }
                size = n;
            }
            CPIO => {
                let form = Cpio::from_u8(r.u8()?).ok_or_else(corrupt)?;
                let flags = r.u8()?;
                if flags & !(INLINE | PAD | UPPER) != 0 {
                    return Err(corrupt());
                }
                let (m, n) = (r.varint()?, r.varint()?);
                let (hl, align) = (form.header(), form.align());
                let mut h = r.bytes(hl)?.to_vec();
                let at = out.len();
                out.extend_from_slice(&h);
                out.extend_from_slice(r.bytes(m)?);
                let raw = flags & PAD != 0;
                let pad = |out: &mut Vec<u8>, r: &mut Reader| -> Result<()> {
                    let p = pad_to(out.len(), align);
                    match raw {
                        false => out.resize(out.len() + p, 0),
                        true => out.extend_from_slice(r.bytes(p)?),
                    }
                    Ok(())
                };
                pad(&mut out, &mut r)?;
                inline = flags & INLINE != 0;
                let b = body(&mut r, data, &mut taken, inline, n)?;
                cpio_code(form, flags & UPPER != 0, &mut h, m as u64, n as u64, byte_sum(b));
                out[at..at + hl].copy_from_slice(&h);
                out.extend_from_slice(b);
                pad(&mut out, &mut r)?;
                size = n;
            }
            _ => return Err(corrupt()),
        }
        room(&out, 0)?;
        info(Info { tag, body: size, inline });
    }
    if taken != data.len() {
        return Err(Error("data left over"));
    }
    if out.len() != len {
        return Err(Error("not of the length"));
    }
    Ok(out)
}
