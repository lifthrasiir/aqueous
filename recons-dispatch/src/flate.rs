//! gzip and zlib: the container sliced around flate-recons on its DEFLATE stream, whose
//! data is a content of the application, recompressed in turn (composition, §4.5).

use flate_recons::Hints;
use flate_recons::inflate::{self, Sink};
use recons_formula::{Formula, Node};

use crate::native::FLATE;
use crate::{Options, verified};

/// A gzip member (RFC 1952); what follows its trailer is sliced as it is.
pub(crate) fn gzip(d: &[u8], opt: &Options) -> Option<Formula> {
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
    let end = p + deflate_len(d.get(p..)?)?;
    d.get(end..end + 8)?;
    wrap("gzip", d, p..end, &Hints::gzip_xfl(d[8]), |_| true, opt)
}

/// A zlib stream (RFC 1950); what follows its checksum is sliced as it is.
pub(crate) fn zlib(d: &[u8], opt: &Options) -> Option<Formula> {
    let end = 2 + deflate_len(d.get(2..)?)?;
    let sum = u32::from_be_bytes(*d.get(end..)?.first_chunk()?);
    // Two bytes are a weak sign: the checksum confirms.
    let hints = Hints::zlib_header([d[0], d[1]]);
    wrap("zlib", d, 2..end, &hints, |data| adler32(data) == sum, opt)
}

/// `d` with flate-recons on the DEFLATE stream at `raw`, if `accept` takes its data.
fn wrap(
    class: &'static str,
    d: &[u8],
    raw: std::ops::Range<usize>,
    hints: &Hints,
    accept: impl FnOnce(&[u8]) -> bool,
    opt: &Options,
) -> Option<Formula> {
    let fopt = flate_recons::Options { threads: opt.threads, ..Default::default() };
    let (data, record, _) = flate_recons::deconstruct_hinted(&d[raw.clone()], &fopt, hints).ok()?;
    if !accept(&data) || record.len() >= raw.len() || !verified(opt, FLATE, &[], &[&data, &record], &d[raw.clone()]) {
        return None;
    }
    let mut f = Formula::default();
    f.push(class, [&d[..raw.start], &d[raw.end..]].concat());
    let record = f.push("flate/record", record);
    let data = f.push("raw", data);
    f.root = Node::concat([
        Node::slice(0, 0, raw.start as u64),
        Node::apply(FLATE, vec![], vec![data, record], raw.len() as u64),
        Node::slice(0, raw.start as u64, (d.len() - raw.end) as u64),
    ]);
    Some(f)
}

/// Bytes of the DEFLATE stream at the start of `d`.
fn deflate_len(d: &[u8]) -> Option<usize> {
    struct Skip;
    impl Sink for Skip {
        fn lit(&mut self, _: u8) {}
        fn mat(&mut self, _: u16, _: u16, _: bool) {}
        fn stored(&mut self, _: &[u8]) {}
    }
    let layout = inflate::parse_into(d, usize::MAX, &mut Skip).ok()?;
    Some(d.len() - layout.stream.trailing.len())
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
