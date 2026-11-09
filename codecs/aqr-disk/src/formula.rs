//! aqr-disk as an entry formula (see aqr-formula): the image as the concatenation of its pieces
//! ([`crate::pieces`]), each file's contents a stream of its own (of its kind's class), the
//! rest a slice of the stream `disk/rest`. `FORMAT.md` tells of the streams.

use std::borrow::Cow;

use aqr_formula::store::Writer;
use aqr_formula::{Formula, Kind, Node};

use crate::{File, FileSystem, Piece};

/// The class of the stream of the rest of the image, stream 1.
pub const REST: &str = "disk/rest";

#[derive(Debug, Clone)]
pub struct Options {
    /// The file systems read, the first that starts a volume taking it.
    pub file_systems: &'static [FileSystem],
    /// What a file's contents are by its path: the contents of each [`Kind`] are a content
    /// stream of their own (those of [`Kind::Unknown`] the stream `raw`, the others after it,
    /// `disk/{kind}`, or `disk/{name}` for a format). By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
}

impl Default for Options {
    fn default() -> Options {
        Options { file_systems: &[], kind: |_| Kind::Unknown }
    }
}

/// The formula of image `input` with `opt`, or None where none of its file systems is read or
/// no file of them has contents.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    if !crate::starts(input, opt.file_systems) {
        return None;
    }
    let files = crate::files(input, opt.file_systems);
    let pieces = crate::pieces(input, &files);
    (!pieces.is_empty()).then(|| build(input, &files, &pieces, opt))
}

/// The formula of `input` as `pieces` of `files` ([`crate::pieces`]).
pub fn build(input: &[u8], files: &[File], pieces: &[Piece], opt: &Options) -> Formula {
    // Each file's contents a stream of its own, of its kind's class: the streams of a class are
    // put together as the archive is encoded (in the order of their indices, by the first of
    // them), as one stream of the class would be. The contents of a file of one piece are shared
    // with `input` where it is known to the store (the file mapped, not copied), else copied, as
    // are those of several pieces, put together; these are split into their pieces, each in its
    // place. The streams are in the order of the classes' first files, and of the files' first
    // pieces, the first of `raw` (or an empty one) before the rest, stream 1, as `raw` is the
    // first class.
    const REST_AT: usize = 1;
    // each file's pieces, in its order
    let mut of: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    // the kinds in the order they come, `raw` first, and the files of each
    let mut kinds: Vec<(Kind, Vec<usize>)> = vec![(Kind::Unknown, Vec::new())];
    let mut rest_len = 0;
    for (j, p) in pieces.iter().enumerate() {
        let Some((i, _)) = p.file else {
            rest_len += p.span.len();
            continue;
        };
        if of[i].is_empty() {
            let kind = (opt.kind)(&files[i].name);
            match kinds.iter_mut().find(|k| k.0 == kind) {
                Some(k) => k.1.push(i),
                None => kinds.push((kind, vec![i])),
            }
        }
        of[i].push(j);
    }
    of.iter_mut().for_each(|js| js.sort_by_key(|&j| pieces[j].file.map(|f| f.1)));
    let class = |kind: Kind| -> Cow<'static, str> {
        match kind {
            Kind::Unknown => "raw".into(),
            Kind::Format(name) => format!("disk/{name}").into(),
            kind => format!("disk/{kind}").into(),
        }
    };
    let contents = |i: usize| {
        let spans = || of[i].iter().map(|&j| &input[pieces[j].span.clone()]);
        if let [j] = of[i][..]
            && let Some(b) = aqr_formula::store::find(&input[pieces[j].span.clone()])
        {
            return b;
        }
        // (of the store where one is set: aqr-compress's spill, see aqr_formula::store)
        let mut b = Writer::new(Some(spans().map(<[u8]>::len).sum()));
        spans().for_each(|d| b.write(d));
        b.into_bytes()
    };
    let mut f = Formula::default();
    let mut stream_of = vec![usize::MAX; files.len()];
    let raws = &kinds[0].1;
    match raws.first() {
        Some(&i) => {
            f.push_content("raw", Kind::Unknown, contents(i));
            stream_of[i] = 0;
        }
        None => _ = f.push_content("raw", Kind::Unknown, Vec::new()),
    }
    // (filled below, in the order of the pieces)
    let mut rest = Writer::new(Some(rest_len));
    f.push(REST, Vec::new());
    for (n, (kind, is)) in kinds.iter().enumerate() {
        // (the first of `raw` stream 0, if any)
        for &i in is.iter().skip((n == 0) as usize) {
            f.push_content(class(*kind), *kind, contents(i));
            stream_of[i] = f.streams.len() - 1;
        }
    }
    // the outputs of each file of several pieces, by piece
    let mut split: Vec<Vec<Option<Node>>> = vec![Vec::new(); files.len()];
    for (i, js) in of.iter().enumerate().filter(|(_, js)| js.len() > 1) {
        let sizes: Vec<u64> = js.iter().map(|&j| pieces[j].span.len() as u64).collect();
        let all = Node::slice(stream_of[i], 0, sizes.iter().sum());
        split[i] = f.split_node(all, sizes).into_iter().map(Some).collect();
    }
    let mut at = 0;
    let mut parts = Vec::with_capacity(pieces.len());
    for p in pieces {
        parts.push(match p.file {
            None => {
                let n = p.span.len();
                rest.write(&input[p.span.clone()]);
                at += n;
                Node::slice(REST_AT, (at - n) as u64, n as u64)
            }
            Some((i, _)) if of[i].len() == 1 => Node::slice(stream_of[i], 0, p.span.len() as u64),
            Some((i, k)) => split[i][k].take().expect("each piece once"),
        });
    }
    f.root = Node::concat(parts);
    f.streams[REST_AT].data = rest.into_bytes();
    debug_assert_eq!(f.root.len(), input.len() as u64);
    f
}
