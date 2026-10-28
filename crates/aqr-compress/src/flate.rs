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
        d.get(end..)
            .and_then(|t| t.first_chunk())
            .is_some_and(|&s| aqr_common::adler32::hash(data) == u32::from_be_bytes(s))
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
    zlib_inflated(d).map(|z| z.0)
}

/// [`zlib_deflate`], with the data.
fn zlib_inflated(d: &[u8]) -> Option<(Range<usize>, Vec<u8>)> {
    let mut data = Data(Vec::new());
    let end = 2 + inflate(d.get(2..)?, &mut data)?;
    let sum = u32::from_be_bytes(*d.get(end..)?.first_chunk()?);
    (aqr_common::adler32::hash(&data.0) == sum).then_some((2..end, data.0))
}

/// Bytes of the header of a git object at the start of `data` (`blob`, `tree`, `commit` or
/// `tag`, a space, the size of the rest in decimal, a NUL), as a loose object's zlib stream
/// holds it, if it is one.
pub(crate) fn git_object_header(data: &[u8]) -> Option<usize> {
    let sp = data.iter().take(7).position(|&b| b == b' ')?;
    if !matches!(&data[..sp], b"blob" | b"tree" | b"commit" | b"tag") {
        return None;
    }
    let digits = &data[sp + 1..];
    let n = digits.iter().take(21).position(|&b| b == 0)?;
    let size = std::str::from_utf8(&digits[..n]).ok()?;
    let h = sp + 2 + n;
    // (git's own decimal: no sign, no leading zero)
    let canonical =
        !size.is_empty() && size.bytes().all(|b| b.is_ascii_digit()) && (size == "0" || !size.starts_with('0'));
    (canonical && size.parse::<u64>().ok()? == (data.len() - h) as u64).then_some(h)
}

/// The CRC-32 and size of the data of the git object that `d` is a loose object of (a zlib
/// stream of its header and data), if it is one of at least `min` bytes: what the file checked
/// out of a blob is. `d` is inflated only where its first bytes inflate to such a header.
pub(crate) fn git_loose_sum(head: &[u8], d: impl FnOnce() -> Option<Vec<u8>>, min: u64) -> Option<(u32, u64)> {
    let mut start = Data(Vec::new());
    let _ = inflate(head.get(2..)?, &mut start);
    let sp = start.0.iter().take(7).position(|&b| b == b' ')?;
    let n = start.0[sp + 1..].iter().take(21).position(|&b| b == 0)?;
    let size: u64 = std::str::from_utf8(&start.0[sp + 1..][..n]).ok()?.parse().ok()?;
    if size < min || !matches!(&start.0[..sp], b"blob" | b"tree" | b"commit" | b"tag") {
        return None;
    }
    let (_, data) = zlib_inflated(&d()?)?;
    let h = git_object_header(&data)?;
    Some((aqr_common::crc32::hash(&data[h..]), size))
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
    let (profile, zlib) = opt.flate();
    let fopt = aqr_flate::Options {
        threads: opt.threads,
        max_data: usize::try_from(cx.inflate_left()).unwrap_or(usize::MAX),
        zlib,
        ..aqr_flate::Options::for_profile(profile).ok()?
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
    // (the coding of the stream told of as the first half of it: the second is its data's, made
    // in turn, [`crate::recompress_with`])
    let _watch = aqr_common::progress::alias(parsed.data(), &d[raw.start..raw.start + raw.len() / 2]);
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
    // A git object's header, of a loose object, is with the container's bytes: the rest is a
    // slice of its own, which the archive finds where it is a file too, checked out, and takes
    // as a file of its format (text, say, which the header hid).
    let git = if class == "zlib" { git_object_header(&data) } else { None };
    let mut outside = [&d[..raw.start], &d[rest..]].concat();
    if let Some(h) = git {
        outside.extend_from_slice(&data[..h]);
    }
    let around = outside.len() as u64 - git.unwrap_or(0) as u64;
    f.push(class, outside);
    let record = f.push("flate/record", record);
    let data = match git {
        Some(h) => {
            let mut data = data;
            data.drain(..h);
            let body = f.push_as("raw", kind, data);
            Node::concat([Node::slice(0, around, h as u64), body])
        }
        None => f.push_as("raw", kind, data),
    };
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

#[cfg(test)]
mod tests {
    use super::git_object_header;

    #[test]
    fn git_object_headers() {
        assert_eq!(git_object_header(b"blob 3\0abc"), Some(7));
        assert_eq!(git_object_header(b"commit 0\0"), Some(9));
        for d in [&b"blob 4\0abc"[..], b"blob 03\0abc", b"blob +3\0abc", b"blob 3abc", b"blub 3\0abc", b"blob \0"] {
            assert_eq!(git_object_header(d), None, "{d:?}");
        }
    }
}
