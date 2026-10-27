//! gzip and zlib: the container sliced around aqr-flate on its DEFLATE stream, whose
//! data is a content of the application, recompressed in turn (composition, §4.5): gzip's of
//! the kind of its name, if it has one ([`kind_by_name`]). A gzip member's trailer is derived
//! by aqr-flate's module, where it is right.
//!
//! The stream is parsed once, and deconstructed only where that may pay
//! ([`aqr_formula::probe`]): what it saves comes from its data compressing better than DEFLATE
//! did, and splitting and joining it costs about as much as compressing it with the encoder
//! did.

use std::ops::Range;

use aqr_flate::formula::Derived;
use aqr_flate::inflate::{self, Sink};
use aqr_flate::{Hints, Parsed};
use aqr_formula::{Formula, Kind, Node};

use crate::{Cx, Options, kind_by_name};

/// A gzip member (RFC 1952), its data of the kind of its name, or else `contents`; what follows
/// its trailer is sliced as it is.
pub(crate) fn gzip(d: &[u8], contents: Kind, opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let start = gzip_start(d)?;
    let kind = gzip_name(d).map_or(Kind::Unknown, kind_by_name);
    let kind = if kind == Kind::Unknown { contents } else { kind };
    let hints = Hints::gzip_xfl(d[8]);
    wrap("gzip", d, start, |_, end| d.get(end..end + 8).is_some(), kind, &hints, opt, depth, cx)
}

/// A zlib stream (RFC 1950), its data of kind `contents`; what follows its checksum is sliced
/// as it is.
pub(crate) fn zlib(d: &[u8], contents: Kind, opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let checked = |data: &[u8], end: usize| {
        d.get(end..).and_then(|t| t.first_chunk()).is_some_and(|&s| adler32(data) == u32::from_be_bytes(s))
    };
    let hints = Hints::zlib_header([d[0], d[1]]);
    wrap("zlib", d, 2, checked, contents, &hints, opt, depth, cx)
}

/// The name of the file (FNAME) of the gzip member at the start of `d`, if it has one.
fn gzip_name(d: &[u8]) -> Option<&[u8]> {
    let flg = *d.get(3)?;
    if flg & 8 == 0 {
        return None;
    }
    let p = 10 + if flg & 4 != 0 { 2 + u16::from_le_bytes(*d.get(10..)?.first_chunk()?) as usize } else { 0 };
    let name = d.get(p..)?;
    Some(&name[..name.iter().position(|&b| b == 0)?])
}

/// The DEFLATE stream of the gzip member at the start of `d`, with its 8-byte trailer after it.
pub(crate) fn gzip_deflate(d: &[u8]) -> Option<Range<usize>> {
    let p = gzip_start(d)?;
    let end = p + inflate(d.get(p..)?, &mut Skip)?;
    d.get(end..end + 8)?;
    Some(p..end)
}

/// Where the DEFLATE stream of the gzip member at the start of `d` starts, after its header.
fn gzip_start(d: &[u8]) -> Option<usize> {
    let flg = *d.get(3)?;
    if flg & 0xe0 != 0 {
        return None;
    }
    let mut p = 10;
    if flg & 4 != 0 {
        p += 2 + u16::from_le_bytes(*d.get(p..)?.first_chunk()?) as usize;
    }
    // FNAME, FCOMMENT
    for bit in [8, 16] {
        if flg & bit != 0 {
            p += d.get(p..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flg & 2 != 0 {
        p += 2;
    }
    Some(p)
}

/// The DEFLATE stream of the zlib stream at the start of `d`, with its checksum after it.
/// Two bytes of header are a weak sign: the checksum confirms.
pub(crate) fn zlib_deflate(d: &[u8]) -> Option<Range<usize>> {
    let mut data = Data(Vec::new());
    let end = 2 + inflate(d.get(2..)?, &mut data)?;
    let sum = u32::from_be_bytes(*d.get(end..)?.first_chunk()?);
    (adler32(&data.0) == sum).then_some(2..end)
}

/// `d` with aqr-flate on the DEFLATE stream from `start`, its data of `kind` (`depth` formats
/// deep), if it fits in the budget, the container takes it (`checked`, given its data and where
/// it ends) and it may pay ([`crate::probe`]: its record estimated by aqr-flate on a prefix).
#[allow(clippy::too_many_arguments)]
fn wrap(
    class: &'static str,
    d: &[u8],
    start: usize,
    checked: impl FnOnce(&[u8], usize) -> bool,
    kind: Kind,
    hints: &Hints,
    opt: &Options,
    depth: u32,
    cx: &Cx,
) -> Option<Formula> {
    let fopt = aqr_flate::Options {
        threads: opt.threads,
        max_data: usize::try_from(cx.inflate_left()).unwrap_or(usize::MAX),
        ..Default::default()
    };
    let parsed = Parsed::head(d.get(start..)?, &fopt).ok()?;
    let raw = start..start + parsed.len();
    if !checked(parsed.data(), raw.end) {
        return None;
    }
    if !cx.inflate(parsed.data().len() as u64) {
        return None;
    }
    if let Some(probe) = crate::probe(opt, depth)
        && !probe.pays(
            &cx.enc,
            parsed.len(),
            || parsed.estimate(&fopt).ok(),
            parsed.data(),
            || cx.held_by_others(parsed.data(), d) as u8 as f64,
            kind,
        )
    {
        return None;
    }
    #[cfg(test)]
    crate::tests::DECONSTRUCTED.with(|n| n.set(n.get() + 1));
    let (data, record, _) = parsed.deconstruct(&fopt, hints).ok()?;
    if record.len() >= raw.len() {
        return None;
    }
    // a gzip member's trailer as aqr-flate's module derives it, where it is that
    let derived = match class {
        "gzip" => {
            let mut t = Vec::new();
            Derived::Gzip.put(&data, raw.len(), &mut t);
            match d.get(raw.end..raw.end + 8) == Some(&t[..]) {
                true => Derived::Gzip,
                false => Derived::None,
            }
        }
        _ => Derived::None,
    };
    let rest = raw.end + derived.bytes();
    let mut f = Formula::default();
    f.push(class, [&d[..raw.start], &d[rest..]].concat());
    let record = f.push("flate/record", record);
    let data = f.push_as("raw", kind, data);
    f.root = Node::concat([
        Node::slice(0, 0, raw.start as u64),
        aqr_flate::formula::apply_one(fopt.profile, data, record, raw.len() as u64, derived).ok()?,
        Node::slice(0, raw.start as u64, (d.len() - rest) as u64),
    ]);
    Some(f)
}

/// Bytes of the DEFLATE stream at the start of `d`, its data given to `sink`.
fn inflate(d: &[u8], sink: &mut impl Sink) -> Option<usize> {
    // what follows the stream may be the rest of a large file: not copied
    Some(inflate::parse_head(d, usize::MAX, sink).ok()?.end)
}

/// A sink of nothing.
struct Skip;

impl Sink for Skip {
    fn lit(&mut self, _: u8) {}
    fn mat(&mut self, _: u16, _: u16, _: bool) {}
    fn stored(&mut self, _: &[u8]) {}
}

/// A sink of the data.
struct Data(Vec<u8>);

impl Sink for Data {
    fn lit(&mut self, b: u8) {
        self.0.push(b);
    }

    fn mat(&mut self, len: u16, dist: u16, _: bool) {
        let from = self.0.len() - dist as usize;
        for i in from..from + len as usize {
            self.0.push(self.0[i]);
        }
    }

    fn stored(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // The most bytes before `b` can overflow.
    for c in d.chunks(5552) {
        for &x in c {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}
