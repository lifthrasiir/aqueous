//! aqr-lzma as modules of entry formulas (see aqr-formula): LZMA streams and the segments of
//! LZMA2 streams rebuilt from their data and records. `FORMAT.md` specifies them. The data is
//! an input of its own, which the formula may recompress in turn (a tar archive's members,
//! which other files may hold).

use aqr_formula::{Apply, Formula, Module, Node};

use aqr_bcj::formula::{self as bcj, Filter};

use crate::{alone, lzma2, recons, xz};

pub use crate::module::{decode, decode2, index};

pub const MODULE: Module = Module::new("lzma-recons", 1).with_memory(memory);

/// The module rebuilding the segments of LZMA2 streams, one after another.
pub const LZMA2: Module = Module::new("lzma2-recons", 1).with_memory(memory);

/// The memory of the module, a stream rebuilt at a time: the inputs and the streams; the match
/// finder's chains, 12 bytes a byte of the data at most (their rings no longer than it) and their
/// heads (12.5 MiB); the literal model, 6 MiB at most (lc + lp up to 12); and the encoder's
/// output, reserved for a quarter of the data and growing twice at a time.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[data, ..] = u.inputs else { return None };
    let m = u.output();
    Some(u.whole(12 * data + (25 << 19) + (6 << 20) + (data / 4).max(3 * m), 6))
}

/// The most data a stream is decoded to (and an `.xz` file's streams, in all).
pub const LIMIT: usize = 1 << 31;

/// Whether `d` starts like a `.lzma` file: a header of valid properties, a dictionary size as
/// encoders write it (2^n or 3 × 2^n bytes, 4 KiB at least), a data size that is unknown or below
/// 2^48, and a stream that decodes for its first 1 KiB ([`crate::lzma::plausible`]). Only for the
/// start of a file.
pub fn starts(d: &[u8]) -> bool {
    let Some(h) = alone::parse(d) else { return false };
    let dict = h.dict;
    dict >= 1 << 12
        && (dict.is_power_of_two() || dict % 3 == 0 && (dict / 3).is_power_of_two())
        && h.size.is_none_or(|s| s < 1 << 48)
        && crate::lzma::plausible(&d[alone::HEADER..], h.props, 1 << 10)
}

/// The data of the `.lzma` file `d` (as far as its stream goes), or None where it does not
/// decode: what [`encode`]'s formula holds as data, for telling what it may share with others.
pub fn data(d: &[u8]) -> Option<Vec<u8>> {
    let h = alone::parse(d)?;
    crate::lzma::decode(&d[alone::HEADER..], h.props, h.size, LIMIT).ok().map(|dec| dec.data)
}

/// The formula rebuilding `input`, a `.lzma` file, or None where it is not one or its stream
/// does not decode. What follows the stream is kept as it is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if !starts(input) {
        return None;
    }
    let h = alone::parse(input)?;
    let s = &input[alone::HEADER..];
    let sp = recons::split(s, h.props, h.dict, h.size, LIMIT).ok()?;
    let rest = s.len() - sp.used;
    let mut f = Formula::default();
    f.push("lzma", [&input[..alone::HEADER], &s[sp.used..]].concat());
    let data = f.push("raw", sp.data);
    let record = f.push("lzma/record", sp.record);
    let index = f.push("lzma/index", Vec::new());
    f.root = Node::concat([
        Node::slice(0, 0, alone::HEADER as u64),
        Node::apply(MODULE, Vec::new(), vec![data, record, index], sp.used as u64),
        Node::slice(0, alone::HEADER as u64, rest as u64),
    ]);
    Some(f)
}

/// Whether `d` starts like an `.xz` file: a stream header of the right CRC-32.
pub fn starts_xz(d: &[u8]) -> bool {
    xz::starts(d)
}

/// The bytes of the streams of the `.xz` file at the start of `d`.
pub fn extent_xz(d: &[u8]) -> Option<usize> {
    xz::parse(d).map(|s| s.end)
}

/// The filters of an `.xz` block before its LZMA2, if aqr-bcj has them all.
fn filters(b: &xz::Block) -> Option<Vec<Filter>> {
    b.filters.iter().map(|(id, p)| Filter::from_xz(*id, p)).collect()
}

/// The data of the blocks of the `.xz` file `d` ending with LZMA2 (after their filters, those
/// of aqr-bcj), those that decode, one after another: what [`encode_xz`]'s formula holds as
/// data, mostly. None where there is none.
pub fn data_xz(d: &[u8]) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    for b in xz::parse(d)?.blocks {
        let Some(fs) = filters(&b) else { continue };
        let Ok(dec) = lzma2::decode(&d[b.stream], LIMIT - data.len()) else { continue };
        let mut x: Vec<u8> = dec.segments.into_iter().flat_map(|s| s.data).collect();
        fs.iter().rev().for_each(|f| f.decode(&mut x));
        data.extend_from_slice(&x);
    }
    (!data.is_empty()).then_some(data)
}

/// The formula rebuilding `input`, an `.xz` file: its blocks ending with LZMA2 (as far as they
/// split, of [`LIMIT`] bytes of data in all) by one application of [`LZMA2`], an output each,
/// the rest of the file as it is. The data of a block of filters (aqr-bcj's) is that before
/// them, their modules applied to it, where encoding it gives back the filtered data (else the
/// filtered data). None where it is not one or no block splits.
pub fn encode_xz(input: &[u8], threads: usize) -> Option<Formula> {
    let blocks = xz::parse(input)?.blocks;
    // (a block's segments on the threads where it is alone, else the blocks)
    let inner = if blocks.len() == 1 { threads } else { 1 };
    let splits = aqr_common::pool::par_map(blocks.len(), threads, |i| {
        let b = &blocks[i];
        let fs = filters(b)?;
        let sp = lzma2::split(&input[b.stream.clone()], b.dict, LIMIT, inner).ok()?;
        let unfiltered = match fs.is_empty() {
            true => None,
            false => {
                let x: Vec<u8> = sp.iter().flat_map(|x| x.data.iter().copied()).collect();
                bcj::unfilter(&fs, &x).map(|d| (fs, d))
            }
        };
        Some((sp, unfiltered))
    });
    let (mut container, mut data, mut records, mut lens) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    // the pieces of the file: bytes of the container, or the output of a block; and the data of
    // each block taken: its span in the data, and its filters
    let mut pieces: Vec<Result<(usize, usize), usize>> = Vec::new();
    let mut spans: Vec<(usize, usize, Vec<Filter>)> = Vec::new();
    let (mut sizes, mut at) = (Vec::new(), 0);
    for (b, sp) in blocks.iter().zip(splits) {
        let Some((sp, unfiltered)) =
            sp.filter(|sp| data.len() + sp.0.iter().map(|x| x.data.len()).sum::<usize>() <= LIMIT)
        else {
            continue;
        };
        pieces.push(Ok((container.len(), b.stream.start - at)));
        container.extend_from_slice(&input[at..b.stream.start]);
        pieces.push(Err(sizes.len()));
        sizes.push(b.stream.len() as u64);
        let start = data.len();
        for x in &sp {
            lens.push((x.data.len(), x.record.len()));
            records.extend_from_slice(&x.record);
        }
        match unfiltered {
            Some((fs, d)) => {
                data.extend_from_slice(&d);
                spans.push((start, d.len(), fs));
            }
            None => {
                sp.iter().for_each(|x| data.extend_from_slice(&x.data));
                spans.push((start, data.len() - start, Vec::new()));
            }
        }
        at = b.stream.end;
    }
    if sizes.is_empty() {
        return None;
    }
    pieces.push(Ok((container.len(), input.len() - at)));
    container.extend_from_slice(&input[at..]);
    let mut f = Formula::default();
    f.push("xz", container);
    f.push("raw", data);
    let data =
        Node::concat(spans.into_iter().map(|(a, n, fs)| bcj::apply(&fs, Node::slice(1, a as u64, n as u64), n as u64)));
    let inputs = vec![data, f.push("lzma2/record", records), f.push("lzma2/index", index(&lens))];
    let mut outputs: Vec<Option<Node>> = match sizes.len() {
        1 => vec![Some(Node::apply(LZMA2, Vec::new(), inputs, sizes[0]))],
        _ => {
            let a = Apply {
                module: LZMA2,
                resources: vec![],
                params: vec![],
                inputs,
                len: 0,
                memory_limit: aqr_formula::NO_LIMIT,
            };
            f.split(a, sizes).into_iter().map(Some).collect()
        }
    };
    f.root = Node::concat(pieces.into_iter().map(|p| match p {
        Ok((a, n)) => Node::slice(0, a as u64, n as u64),
        Err(i) => outputs[i].take().expect("an output once"),
    }));
    Some(f)
}
