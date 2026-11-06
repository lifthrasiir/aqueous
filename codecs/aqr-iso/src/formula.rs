//! aqr-iso as an entry formula (see aqr-formula): the image as the concatenation of its pieces
//! ([`crate::pieces`]), each file's contents a stream of its own (of its kind's class), the
//! rest a slice of the stream `iso/rest`. `FORMAT.md` tells of the streams.

use std::borrow::Cow;

use aqr_formula::store::Writer;
use aqr_formula::{Formula, Kind, Node};

use crate::{File, Piece};

/// The class of the stream of the rest of the image, stream 1.
pub const REST: &str = "iso/rest";

#[derive(Debug, Clone)]
pub struct Options {
    /// What a file's contents are by its path: the contents of each [`Kind`] are a content
    /// stream of their own (those of [`Kind::Unknown`] the stream `raw`, the others after it,
    /// `iso/{kind}`, or `iso/{name}` for a format). By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
}

impl Default for Options {
    fn default() -> Options {
        Options { kind: |_| Kind::Unknown }
    }
}

/// The formula of image `input`, or None where it is none or no file of it has contents. The
/// same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    encode_with(input, &Options::default())
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    if !crate::starts(input) {
        return None;
    }
    let files = crate::files(input);
    let pieces = crate::pieces(input, &files);
    (!pieces.is_empty()).then(|| build(input, &files, &pieces, opt))
}

/// The formula of `input` as `pieces` of `files`.
pub fn build(input: &[u8], files: &[File], pieces: &[Piece], opt: &Options) -> Formula {
    // Each file's contents a stream of its own, of its kind's class: the streams of a class are
    // put together as the archive is encoded (in the order of their indices, by the first of
    // them), as one stream of the class would be. The contents are shared with `input` where it
    // is known to the store (the file mapped, not copied), else copied. The streams are in the
    // order of the classes' first contents, the first of `raw` (or an empty one) before the rest,
    // stream 1, as `raw` is the first class.
    const REST_AT: usize = 1;
    // the kinds in the order they come, `raw` first, and the pieces of each
    let mut kinds: Vec<(Kind, Vec<usize>)> = vec![(Kind::Unknown, Vec::new())];
    let mut rest_len = 0;
    for (j, p) in pieces.iter().enumerate() {
        let Some(i) = p.file else {
            rest_len += p.span.len();
            continue;
        };
        let kind = (opt.kind)(&files[i].name);
        match kinds.iter_mut().find(|k| k.0 == kind) {
            Some(k) => k.1.push(j),
            None => kinds.push((kind, vec![j])),
        }
    }
    let class = |kind: Kind| -> Cow<'static, str> {
        match kind {
            Kind::Unknown => "raw".into(),
            Kind::Format(name) => format!("iso/{name}").into(),
            kind => format!("iso/{kind}").into(),
        }
    };
    let contents = |j: usize| {
        let d = &input[pieces[j].span.clone()];
        aqr_formula::store::find(d).unwrap_or_else(|| {
            // (of the store where one is set: aqr-compress's spill, see aqr_formula::store)
            let mut b = Writer::new(Some(d.len()));
            b.write(d);
            b.into_bytes()
        })
    };
    let mut f = Formula::default();
    let mut stream_of = vec![usize::MAX; pieces.len()];
    let raws = &kinds[0].1;
    match raws.first() {
        Some(&j) => {
            f.push_content("raw", Kind::Unknown, contents(j));
            stream_of[j] = 0;
        }
        None => _ = f.push_content("raw", Kind::Unknown, Vec::new()),
    }
    // (filled below, in the order of the pieces)
    let mut rest = Writer::new(Some(rest_len));
    f.push(REST, Vec::new());
    for (n, (kind, js)) in kinds.iter().enumerate() {
        // (the first of `raw` stream 0, if any)
        for &j in js.iter().skip((n == 0) as usize) {
            f.push_content(class(*kind), *kind, contents(j));
            stream_of[j] = f.streams.len() - 1;
        }
    }
    let mut at = 0;
    let parts = pieces.iter().zip(&stream_of).map(|(p, &s)| match p.file {
        None => {
            let n = p.span.len();
            rest.write(&input[p.span.clone()]);
            at += n;
            Node::slice(REST_AT, (at - n) as u64, n as u64)
        }
        Some(_) => Node::slice(s, 0, p.span.len() as u64),
    });
    f.root = Node::concat(parts.collect::<Vec<_>>());
    f.streams[REST_AT].data = rest.into_bytes();
    debug_assert_eq!(f.root.len(), input.len() as u64);
    f
}
