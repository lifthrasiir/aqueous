//! aqr-fasta as a module of entry formulas (see aqr-formula): the file rebuilt from its parts
//! (FORMAT.md, "The module"), the headers, the layout, the case and the other letters as they
//! are, for the archive to compress, and the bases coded ([`crate::bases`]).

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use crate::bases;
use crate::fasta;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("fasta-recons", 2).with_plan(plan);

/// The memory of the module: its plan ([`crate::plan::decode`]).
fn plan(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(crate::plan::decode(u.initial(), u.params, u.inputs, u.output()))
}

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 5] = ["headers", "layout", "case", "other", "bases"];

/// Whether `d` starts like a FASTA file of nucleotides: a header line, then a line of letters.
pub fn starts(d: &[u8]) -> bool {
    let Some(rest) = d.strip_prefix(b">") else { return false };
    let Some(eol) = rest.iter().take(4096).position(|&b| b == b'\n') else { return false };
    let seq = &rest[eol + 1..];
    let line = &seq[..seq.iter().take(1024).position(|&b| b == b'\n').unwrap_or(seq.len().min(1024))];
    !line.is_empty() && line.iter().all(|c| b"ACGTNacgtn".contains(c))
}

/// The bytes of a segment, about: a file longer than this is cut into segments of equal parts of
/// about it ([`cuts`]), each an application of its own. An application needs about twice its
/// segment, the bases' model and twice its inputs (the coded bases a quarter of it at most):
/// about 245 MiB on Wasm at this length (random bases, whose codes are a quarter of it).
pub const SEGMENT: usize = 96 << 20;

/// The formula of FASTA file `input`, or `None` where it is not one this crate takes
/// ([`fasta`]). It is the same whatever `threads` is (the segments, if more than one, are coded
/// that many at a time).
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_in(input, threads, SEGMENT)
}

/// Where the segments of `d` begin, of about `segment` bytes: the file in equal parts, cut at
/// the first line's end past each that is not preceded or followed by another and is followed by
/// a sequence line (so that the segment after it is the rest of a record, [`fasta::parse_part`]).
/// Where there is none, the rest is one segment.
fn cuts(d: &[u8], segment: usize) -> Vec<usize> {
    let size = d.len().div_ceil(d.len().div_ceil(segment.max(1)).max(1)).max(1);
    let mut starts = vec![0];
    let mut from = size;
    while from < d.len() {
        let start = *starts.last().unwrap();
        let Some(at) = memchr::memchr_iter(b'\n', &d[from..]).map(|i| from + i).find(|&i| {
            i > start && d[i - 1] != b'\n' && d.get(i + 1).is_some_and(|c| !matches!(c, b'>' | b'\n' | b'\r'))
        }) else {
            break;
        };
        starts.push(at);
        from = starts.len() * size;
    }
    starts
}

/// [`encode`] in segments of `segment` bytes. A file in one segment is an application of the
/// five streams; more are applications of slices of five streams in which the segments' parts
/// follow one another (so that the archive compresses them as it would the file's in one), the
/// bases' coded forms written to the store as they come.
fn encode_in(input: &[u8], threads: usize, segment: usize) -> Option<Formula> {
    let width = fasta::width_of(input)?;
    let starts = cuts(input, segment);
    let range = |k: usize| starts[k]..starts.get(k + 1).copied().unwrap_or(input.len());
    let mut f = Formula::default();
    let mut coded = aqr_formula::store::Writer::new(None);
    // the streams but the bases', and each segment's params, lengths in the streams (of the four,
    // and the bases') and length
    let mut texts: [Vec<u8>; 4] = Default::default();
    let mut segments: Vec<(Vec<u8>, [u64; 5], u64)> = Vec::with_capacity(starts.len());
    for first in (0..starts.len()).step_by(threads.max(1)) {
        let n = threads.max(1).min(starts.len() - first);
        let done = aqr_common::pool::par_map(n, threads, |i| {
            let r = range(first + i);
            let p = fasta::parse_part(&input[r.clone()], width, first + i > 0)?;
            let c = bases::encode(&p.bases);
            aqr_common::progress::done(&input[r]);
            Some((p, c))
        });
        for (k, d) in done.into_iter().enumerate() {
            let (p, c) = d?;
            let mut params = Vec::new();
            varint::put(&mut params, p.width);
            params.push(p.final_newline as u8 | (p.headless as u8) << 1);
            coded.write(&c);
            let lens = [p.headers.len(), p.layout.len(), p.case.len(), p.other.len(), c.len()].map(|n| n as u64);
            for (t, d) in texts.iter_mut().zip([&p.headers, &p.layout, &p.case, &p.other]) {
                t.extend_from_slice(d);
            }
            segments.push((params, lens, range(first + k).len() as u64));
        }
    }
    let [headers, layout, case, other] = texts;
    if let [(params, _, len)] = &segments[..] {
        let inputs = vec![
            f.push_as("fasta/headers", Kind::Text, headers),
            f.push("fasta/layout", layout),
            f.push("fasta/case", case),
            f.push("fasta/other", other),
            f.push_as("fasta/bases", Kind::Compressed, coded),
        ];
        f.root = Node::apply(MODULE, params.clone(), inputs, *len);
        return Some(f);
    }
    let base = f.streams.len();
    f.push_as("fasta/headers", Kind::Text, headers);
    f.push("fasta/layout", layout);
    f.push("fasta/case", case);
    f.push("fasta/other", other);
    f.push_as("fasta/bases", Kind::Compressed, coded);
    let mut at = [0u64; 5];
    let nodes: Vec<Node> = (segments.into_iter())
        .map(|(params, lens, len)| {
            let inputs: Vec<Node> = (0..5).map(|j| Node::slice(base + j, at[j], lens[j])).collect();
            at.iter_mut().zip(lens).for_each(|(a, n)| *a += n);
            Node::apply(MODULE, params, inputs, len)
        })
        .collect();
    f.root = Node::concat(nodes);
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file of formula `f` (an application, or segments' one after another).
    fn eval(f: &Formula) -> Vec<u8> {
        let apply = |n: &Node| {
            let Node::Apply(a) = n else { panic!("not an application") };
            let inputs: Vec<&[u8]> = (a.inputs.iter())
                .map(|n| match n {
                    &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
                    _ => panic!("not a slice"),
                })
                .collect();
            decode(&a.params, &inputs, a.len, 1).unwrap()
        };
        match &f.root {
            Node::Concat(ns) => ns.iter().flat_map(apply).collect(),
            n => apply(n),
        }
    }

    /// A file of records of random bases, lower case, runs of N and other letters, lines of the
    /// width and not, with or without the last line's end.
    fn file(seed: u64, records: usize, length: u64, end: bool) -> Vec<u8> {
        let mut rng = aqr_common::rng::Rng::new(seed);
        let mut d = Vec::new();
        for r in 0..records {
            d.extend_from_slice(format!(">rec{r} some description\n").as_bytes());
            let n = rng.below(length) + 1;
            let width = if r % 5 == 4 { 50 } else { 60 };
            let mut line = 0;
            for _ in 0..n {
                let c = match rng.below(1000) {
                    0..=3 => b'N',
                    4 => b'R',
                    5..=100 => b"acgt"[rng.below(4) as usize],
                    _ => b"ACGT"[rng.below(4) as usize],
                };
                d.push(c);
                line += 1;
                if line == width || (r % 7 == 6 && rng.below(80) == 0) {
                    d.push(b'\n');
                    line = 0;
                }
            }
            if line > 0 || r % 3 == 0 {
                d.push(b'\n');
            }
        }
        if !end && d.last() == Some(&b'\n') {
            d.pop();
        }
        d
    }

    #[test]
    fn segments() {
        for (seed, records, length, end) in
            [(1, 1, 40_000, true), (2, 6, 9000, false), (3, 30, 1500, true), (4, 4, 20_000, true)]
        {
            let d = file(seed, records, length, end);
            let whole = encode(&d, 2).unwrap();
            assert!(matches!(whole.root, Node::Apply(_)));
            assert_eq!(eval(&whole), d);
            for segment in [d.len() / 2 + 1, 10_000, 3000, 700] {
                let f = encode_in(&d, 3, segment).unwrap();
                let n = d.len().div_ceil(segment);
                let Node::Concat(nodes) = &f.root else { panic!("{seed}/{segment}") };
                // (cut where a line's end is, so a little more than the parts)
                assert!(nodes.len() >= n && nodes.len() <= n + 1, "{seed}/{segment}: {} of {n}", nodes.len());
                for node in nodes {
                    let Node::Apply(a) = node else { panic!() };
                    assert!(a.len <= segment as u64 + 200, "{seed}/{segment}: {}", a.len);
                }
                assert_eq!(f.streams.len(), 5);
                assert_eq!(eval(&f), d, "{seed}/{segment}");
            }
        }
    }

    #[test]
    fn cut_at_sequence_lines() {
        // no line's end followed by a sequence line, the line before not empty: one segment
        assert_eq!(cuts(b">a\n>b\n>c\n>d\n", 3), [0]);
        // (not at a line's end next to an empty line)
        let d = b">a\nAC\n\nGT\n\n\nAC\n";
        assert!(cuts(d, 4).iter().skip(1).all(|&i| d[i - 1] != b'\n' && d[i + 1] != b'\n'));
        let d = b">a\nACGT\nACGT\nACGT\nAC\n";
        let c = cuts(d, 8);
        assert!(c.len() > 1 && c.iter().skip(1).all(|&i| d[i] == b'\n' && d[i + 1] != b'>'), "{c:?}");
    }
}
