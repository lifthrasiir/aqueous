//! gzip and zlib: the container sliced around aqr-flate on its DEFLATE stream, whose
//! data is a content of the application, recompressed in turn (composition, §4.5).

use std::ops::Range;

use aqr_decompress::FLATE;
use aqr_flate::Hints;
use aqr_flate::inflate::{self, Sink};
use aqr_formula::{Formula, Node};

use crate::{Cx, Options};

/// A gzip member (RFC 1952); what follows its trailer is sliced as it is.
pub(crate) fn gzip(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let raw = gzip_deflate(d)?;
    wrap("gzip", d, raw, &Hints::gzip_xfl(d[8]), opt, cx)
}

/// A zlib stream (RFC 1950); what follows its checksum is sliced as it is.
pub(crate) fn zlib(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let raw = zlib_deflate(d)?;
    wrap("zlib", d, raw, &Hints::zlib_header([d[0], d[1]]), opt, cx)
}

/// The DEFLATE stream of the gzip member at the start of `d`, with its 8-byte trailer after it.
pub(crate) fn gzip_deflate(d: &[u8]) -> Option<Range<usize>> {
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
    let end = p + inflate(d.get(p..)?, &mut Skip)?;
    d.get(end..end + 8)?;
    Some(p..end)
}

/// The DEFLATE stream of the zlib stream at the start of `d`, with its checksum after it.
/// Two bytes of header are a weak sign: the checksum confirms.
pub(crate) fn zlib_deflate(d: &[u8]) -> Option<Range<usize>> {
    let mut data = Data(Vec::new());
    let end = 2 + inflate(d.get(2..)?, &mut data)?;
    let sum = u32::from_be_bytes(*d.get(end..)?.first_chunk()?);
    (adler32(&data.0) == sum).then_some(2..end)
}

/// `d` with aqr-flate on the DEFLATE stream at `raw`, if its data fits in the budget.
fn wrap(class: &'static str, d: &[u8], raw: Range<usize>, hints: &Hints, opt: &Options, cx: &Cx) -> Option<Formula> {
    // measured only where it might not fit
    let left = cx.inflate_left();
    if (raw.len() as u64).saturating_mul(aqr_zip::zip::MAX_EXPANSION) > left
        && aqr_zip::zip::inflated_len(&d[raw.clone()])? as u64 > left
    {
        return None;
    }
    let fopt = aqr_flate::Options { threads: opt.threads, ..Default::default() };
    let (data, record, _) = aqr_flate::deconstruct_hinted(&d[raw.clone()], &fopt, hints).ok()?;
    if !cx.inflate(data.len() as u64) {
        return None;
    }
    let resources = aqr_flate::formula::resources(fopt.profile).ok()?;
    if record.len() >= raw.len() {
        return None;
    }
    let mut f = Formula::default();
    f.push(class, [&d[..raw.start], &d[raw.end..]].concat());
    let record = f.push("flate/record", record);
    let data = f.push("raw", data);
    f.root = Node::concat([
        Node::slice(0, 0, raw.start as u64),
        Node::apply_with(FLATE, resources, vec![], vec![data, record], raw.len() as u64),
        Node::slice(0, raw.start as u64, (d.len() - raw.end) as u64),
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
