//! aqr-iso as an entry formula (see aqr-formula): the image as the concatenation of its pieces
//! ([`crate::pieces`]), each file's contents a slice of the content stream of its kind, the
//! rest a slice of the stream `iso/rest`. `FORMAT.md` tells of the streams.

use std::borrow::Cow;

use aqr_formula::store::Buffer;
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
    const RAW_AT: usize = 0;
    const REST_AT: usize = 1;
    let mut f = Formula::default();
    f.push_content("raw", Kind::Unknown, Vec::new());
    f.push(REST, Vec::new());
    // the stream of each piece, the streams by kind, and their lengths
    let mut lens: Vec<usize> = vec![0, 0];
    let mut kinds: Vec<(Kind, usize)> = vec![(Kind::Unknown, RAW_AT)];
    let mut parts = Vec::with_capacity(pieces.len());
    let mut of = Vec::with_capacity(pieces.len());
    for p in pieces {
        let s = match p.file {
            None => REST_AT,
            Some(i) => {
                let kind = (opt.kind)(&files[i].name);
                match kinds.iter().find(|k| k.0 == kind) {
                    Some(k) => k.1,
                    None => {
                        let class: Cow<'static, str> = match kind {
                            Kind::Format(name) => format!("iso/{name}").into(),
                            kind => format!("iso/{kind}").into(),
                        };
                        f.push_content(class, kind, Vec::new());
                        lens.push(0);
                        kinds.push((kind, lens.len() - 1));
                        lens.len() - 1
                    }
                }
            }
        };
        parts.push(Node::slice(s, lens[s] as u64, p.span.len() as u64));
        of.push((s, lens[s]));
        lens[s] += p.span.len();
    }
    // (of the store where one is set: aqr-compress's spill, see aqr_formula::store)
    let mut data: Vec<Buffer> = lens.iter().map(|&n| Buffer::new(n)).collect();
    for (p, &(s, at)) in pieces.iter().zip(&of) {
        data[s][at..at + p.span.len()].copy_from_slice(&input[p.span.clone()]);
    }
    for (s, d) in data.into_iter().enumerate() {
        f.streams[s].data = d.into_bytes();
    }
    f.root = Node::concat(parts);
    debug_assert_eq!(f.root.len(), input.len() as u64);
    f
}
