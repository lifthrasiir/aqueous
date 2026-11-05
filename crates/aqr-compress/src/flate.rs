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

/// A gzip member (RFC 1952), its data of the kind of its name, or else `contents`, and the
/// members after it (as gzip, pigz's `--independent` and aqr-pack write a stream in parts): a run
/// of them from each member that pays (probed), those after it in the run not probed (parts of a
/// stream, they pay as it does), the data of a run one stream rebuilt by one application of
/// aqr-flate's. A member not taken is sliced as it is, and so is what follows the last.
pub(crate) fn gzip(d: &[u8], contents: Kind, opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    if crate::inflated::on(opt) {
        return crate::inflated::gzip(d);
    }
    let kind = gzip_name(d).map_or(Kind::Unknown, kind_by_name);
    let kind = if kind == Kind::Unknown { contents } else { kind };
    let magic = |at: usize| d.get(at..at + 3) == Some(&[0x1f, 0x8b, 8]);
    let of = |at: usize, probed: bool| {
        let m = d.get(at..)?;
        let hints = Hints::gzip_xfl(*m.get(8)?);
        let start = gzip_start(m)?;
        let checked = |_: &[u8], end: usize| m.get(end..end + 8).is_some();
        let (profile, mut p) = member("gzip", m, start, checked, kind, &hints, opt, depth, probed, cx)?;
        p.raw = at + p.raw.start..at + p.raw.end;
        Some((profile, p))
    };
    let mut runs: Vec<(u16, Vec<Member>)> = Vec::new();
    let mut at = 0;
    while magic(at) {
        match of(at, true) {
            Some((profile, first)) => {
                let mut run = vec![first];
                // (those whose trailers aqr-flate's module derives, as one application derives
                // them all)
                while run.last().unwrap().derived == Derived::Gzip
                    && magic(run.last().unwrap().raw.end + 8)
                    && let Some((_, m)) =
                        of(run.last().unwrap().raw.end + 8, false).filter(|m| m.1.derived == Derived::Gzip)
                {
                    run.push(m);
                }
                at = run.last().unwrap().raw.end + 8;
                runs.push((profile, run));
            }
            None => match d.get(at..).and_then(gzip_deflate) {
                Some(r) => at += r.end + 8,
                None => break,
            },
        }
    }
    if let [(_, run)] = &runs[..]
        && run.len() == 1
    {
        let (profile, mut run) = runs.pop().unwrap();
        return assemble("gzip", d, kind, profile, run.pop().unwrap());
    }
    if runs.is_empty() {
        return None;
    }
    // the bytes around the streams one stream, and of each run its data (one content) and
    // records; a run of one member rebuilt alone, else by one application split into them
    let mut outside = Vec::new();
    let mut prev = 0;
    let mut f = Formula::default();
    let o = 0;
    f.push("gzip", Vec::new());
    let mut nodes = Vec::new();
    let gap = |outside: &mut Vec<u8>, nodes: &mut Vec<Node>, to: usize, prev: usize| {
        nodes.push(Node::slice(o, outside.len() as u64, (to - prev) as u64));
        outside.extend_from_slice(&d[prev..to]);
    };
    for (profile, run) in runs {
        let (mut data, mut records, mut lens, mut sizes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut gaps = Vec::with_capacity(run.len());
        for m in &run {
            gaps.push((prev, m.raw.start));
            lens.push((m.data.len(), m.record.len()));
            sizes.push(m.raw.len() as u64);
            data.extend_from_slice(&m.data);
            records.extend_from_slice(&m.record);
            prev = m.raw.end + m.derived.bytes();
        }
        let derived = run[0].derived;
        drop(run);
        let records = f.push("flate/record", records);
        let data = f.push_as("raw", kind, data);
        if let [(from, to)] = gaps[..] {
            gap(&mut outside, &mut nodes, to, from);
            nodes.push(aqr_flate::formula::apply_one(profile, data, records, sizes[0], derived).ok()?);
            continue;
        }
        let index = f.push("flate/index", aqr_flate::module::index(&lens));
        // (its output the streams, then their trailers)
        let n = sizes.len();
        sizes.extend(std::iter::repeat_n(Derived::Gzip.bytes() as u64, n));
        let a = aqr_flate::formula::apply(profile, data, records, index, sizes.iter().sum(), Derived::Gzip).ok()?;
        let outs = f.split(a, sizes);
        for (k, &(from, to)) in gaps.iter().enumerate() {
            gap(&mut outside, &mut nodes, to, from);
            nodes.push(outs[k].clone());
            nodes.push(outs[n + k].clone());
        }
    }
    gap(&mut outside, &mut nodes, d.len(), prev);
    f.streams[o].data = outside.into();
    f.root = Node::concat(nodes);
    Some(f)
}

/// A zlib stream (RFC 1950), its data of kind `contents`; what follows its checksum is sliced
/// as it is.
pub(crate) fn zlib(d: &[u8], contents: Kind, opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    if crate::inflated::on(opt) {
        return crate::inflated::zlib(d);
    }
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

/// The data of the gzip member at the start of `d`, or None where it does not inflate or its
/// data would be over `max` bytes.
pub(crate) fn gunzip(d: &[u8], max: usize) -> Option<Vec<u8>> {
    let isize = u32::from_le_bytes(*d.get(d.len().checked_sub(4)?..)?.first_chunk()?);
    if isize as usize > max {
        return None;
    }
    let mut data = Capped(Data(Vec::with_capacity(isize as usize)), max);
    inflate(d.get(gzip_start(d)?..)?, &mut data)?;
    (data.0.0.len() <= max).then_some(data.0.0)
}

/// Where the DEFLATE stream of the gzip member at the start of `d` starts, after its header.
pub(crate) fn gzip_start(d: &[u8]) -> Option<usize> {
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

/// The data of the zlib stream at the start of `d`, or None where it does not inflate, its
/// checksum is not right or its data would be over `max` bytes.
pub(crate) fn zlib_data(d: &[u8], max: usize) -> Option<Vec<u8>> {
    let mut data = Capped(Data(Vec::new()), max);
    let end = 2 + inflate(d.get(2..)?, &mut data)?;
    let data = data.0.0;
    let sum = u32::from_be_bytes(*d.get(end..)?.first_chunk()?);
    (data.len() <= max && aqr_common::adler32::hash(&data) == sum).then_some(data)
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
pub(crate) fn git_loose_sum(
    head: &[u8],
    d: impl FnOnce() -> Option<aqr_formula::Bytes>,
    min: u64,
) -> Option<(u32, u64)> {
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
    let (profile, m) = member(class, d, start, checked, kind, hints, opt, depth, true, cx)?;
    assemble(class, d, kind, profile, m)
}

/// A DEFLATE stream of a container deconstructed: where it is, its data and record, and the
/// container's checksum after it as aqr-flate's module derives it.
struct Member {
    raw: Range<usize>,
    data: Vec<u8>,
    record: Vec<u8>,
    derived: Derived,
}

/// The DEFLATE stream from `start` of `d` deconstructed by aqr-flate, as [`wrap`] takes it (but
/// for the probe, where not `probed`), and the profile it was at.
#[allow(clippy::too_many_arguments)]
fn member(
    class: &'static str,
    d: &[u8],
    start: usize,
    checked: impl FnOnce(&[u8], usize) -> bool,
    kind: Kind,
    hints: &Hints,
    opt: &Options,
    depth: u32,
    probed: bool,
    cx: &Cx,
) -> Option<(u16, Member)> {
    let (profile, zlib) = opt.flate();
    let fopt = aqr_flate::Options {
        threads: opt.threads,
        max_data: usize::try_from(cx.inflate_left()).unwrap_or(usize::MAX),
        zlib,
        ..aqr_flate::Options::for_profile(profile).ok()?
    };
    let parsed = Parsed::head(d.get(start..)?, &fopt);
    let parsed = parsed.ok()?;
    let raw = start..start + parsed.len();
    if !checked(parsed.data(), raw.end) {
        return None;
    }
    if !cx.inflate(parsed.data().len() as u64) {
        return None;
    }
    if let Some(probe) = crate::probe(opt, depth).filter(|_| probed)
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
    let r = parsed.deconstruct(&fopt, hints);
    let (data, record, _) = r.ok()?;
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
    Some((fopt.profile, Member { raw, data, record, derived }))
}

/// `d` with its DEFLATE stream `m` as aqr-flate's module at `profile` makes it again, its data of
/// `kind`.
fn assemble(class: &'static str, d: &[u8], kind: Kind, profile: u16, m: Member) -> Option<Formula> {
    let Member { raw, data, record, derived } = m;
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
        aqr_flate::formula::apply_one(profile, data, record, raw.len() as u64, derived).ok()?,
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

/// A sink of the data that stops taking it past so many bytes (then holding more than them).
struct Capped(Data, usize);

impl Sink for Capped {
    fn lit(&mut self, b: u8) {
        if self.0.0.len() <= self.1 {
            self.0.lit(b);
        }
    }

    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        if self.0.0.len() <= self.1 {
            self.0.mat(len, dist, alt258);
        }
    }

    fn stored(&mut self, bytes: &[u8]) {
        if self.0.0.len() <= self.1 {
            self.0.stored(bytes);
        }
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
