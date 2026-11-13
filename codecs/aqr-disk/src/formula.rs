//! aqr-disk as an entry formula (see aqr-formula): the image as the concatenation of its pieces
//! ([`crate::pieces`]), each file's contents a stream of its own (of its kind's class), the
//! rest a slice of the stream `disk/rest`. `FORMAT.md` tells of the streams.

use std::borrow::Cow;

use aqr_formula::store::Writer;
use aqr_formula::{Bytes, Formula, Kind, Node};

use crate::compressed::{By, STREAM_DATA};
use crate::{File, FileSystem, Piece};

/// The class of the stream of the rest of the image, stream 1 (of the prefix `disk`: [`rest`]).
pub const REST: &str = "disk/rest";

/// The class of the stream of the rest, of the classes' prefix `prefix` ([`Options::prefix`]).
pub fn rest(prefix: &str) -> String {
    format!("{prefix}/rest")
}

/// Whether `class` is that of the stream of the rest ([`rest`]) of one of `prefixes`.
pub fn is_rest(class: &str, prefixes: &[&str]) -> bool {
    class.strip_suffix("/rest").is_some_and(|p| prefixes.contains(&p))
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The file systems read, the first that starts a volume taking it.
    pub file_systems: &'static [FileSystem],
    /// What a file's contents are by its path: the contents of each [`Kind`] are a content
    /// stream of their own (those of [`Kind::Unknown`] the stream `raw`, the others after it,
    /// `{prefix}/{kind}`, or `{prefix}/{name}` for a format). By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
    /// Threads the files stored compressed are taken apart on.
    pub threads: usize,
    /// The prefix of the streams' classes (`{prefix}/rest`, `{prefix}/kept`, of the kinds):
    /// by default `disk`; aqr-bundle's packs give their format's name.
    pub prefix: &'static str,
}

impl Default for Options {
    fn default() -> Options {
        Options { file_systems: &[], kind: |_| Kind::Unknown, threads: 1, prefix: "disk" }
    }
}

/// The class of the streams of the bytes kept of files stored compressed (WOF's chunk tables),
/// of the prefix `disk`.
pub const KEPT: &str = "disk/kept";

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
    let mut rest_len = 0;
    for (j, p) in pieces.iter().enumerate() {
        match p.file {
            Some((i, _)) => of[i].push(j),
            None => rest_len += p.span.len(),
        }
    }
    of.iter_mut().for_each(|js| js.sort_by_key(|&j| pieces[j].file.map(|f| f.1)));
    // the files stored compressed taken apart (their contents their data), on up to `threads`
    let packed: Vec<usize> = (0..files.len())
        .filter(|&i| {
            !of[i].is_empty()
                && files[i].compressed.as_ref().is_some_and(|c| !matches!(c, crate::Compressed::Opaque { .. }))
        })
        .collect();
    let made = aqr_common::pool::par_map(packed.len(), opt.threads.max(1), |k| {
        let i = packed[k];
        let s: Vec<u8> = of[i].iter().flat_map(|&j| input[pieces[j].span.clone()].iter().copied()).collect();
        let mut u = crate::compressed::unpack(&s, files[i].compressed.as_ref().expect("compressed"))?;
        // (the data to the store as it is made, where one is set: of thousands of files, not
        // all held at once)
        let mut data: Bytes = std::mem::take(&mut u.data).into();
        aqr_formula::store::spill(&mut data);
        Some((data, u))
    });
    let mut unpacked: Vec<Option<crate::compressed::Unpacked>> = (0..files.len()).map(|_| None).collect();
    let mut datas: Vec<Option<Bytes>> = vec![None; files.len()];
    for (i, u) in packed.into_iter().zip(made) {
        if let Some((d, u)) = u {
            (datas[i], unpacked[i]) = (Some(d), Some(u));
        }
    }
    // the kinds in the order of the files' first pieces, `raw` first, and the files of each (a
    // file stored compressed and not taken apart of compressed data, whatever its name)
    let mut kinds: Vec<(Kind, Vec<usize>)> = vec![(Kind::Unknown, Vec::new())];
    let mut seen = vec![false; files.len()];
    for p in pieces {
        let Some((i, _)) = p.file else { continue };
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        let kind = match files[i].compressed.is_some() && datas[i].is_none() {
            true => Kind::Compressed,
            false => (opt.kind)(&files[i].name),
        };
        match kinds.iter_mut().find(|k| k.0 == kind) {
            Some(k) => k.1.push(i),
            None => kinds.push((kind, vec![i])),
        }
    }
    let class = |kind: Kind| -> Cow<'static, str> {
        match kind {
            Kind::Unknown => "raw".into(),
            Kind::Format(name) => format!("{}/{name}", opt.prefix).into(),
            kind => format!("{}/{kind}", opt.prefix).into(),
        }
    };
    let mut contents = |i: usize| {
        if let Some(d) = datas[i].take() {
            return d;
        }
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
    f.push(self::rest(opt.prefix), Vec::new());
    for (n, (kind, is)) in kinds.iter().enumerate() {
        // (the first of `raw` stream 0, if any)
        for &i in is.iter().skip((n == 0) as usize) {
            f.push_content(class(*kind), *kind, contents(i));
            stream_of[i] = f.streams.len() - 1;
        }
    }
    // the outputs of each file of several pieces, by piece; of each file of one, the node of it
    // (where stored compressed: an application rebuilding its bytes from its data)
    let mut split: Vec<Vec<Option<Node>>> = vec![Vec::new(); files.len()];
    let mut whole: Vec<Option<Node>> = vec![None; files.len()];
    for (i, js) in of.iter().enumerate().filter(|(_, js)| !js.is_empty()) {
        let sizes: Vec<u64> = js.iter().map(|&j| pieces[j].span.len() as u64).collect();
        let all = match unpacked[i].take() {
            Some(u) => {
                let len = files[i].compressed.as_ref().expect("compressed").len();
                // applications of streams of chunks, each of 32 MiB of data at most (as the module
                // holds it whole), the chunk table kept before the first stream
                let mut kept = Some((f.push(format!("{}/kept", opt.prefix), u.table.clone()), u.table.len() as u64));
                let (mut at, mut apps) = (0, Vec::new());
                let mut streams = u.streams.into_iter().peekable();
                while streams.peek().is_some() {
                    let (mut group, mut data_len, mut bytes) = (Vec::new(), 0, 0);
                    while let Some(p) = streams.next_if(|p| group.is_empty() || data_len + p.data <= STREAM_DATA) {
                        let (kept, n) = kept.take().unwrap_or_else(|| (Node::concat(Vec::new()), 0));
                        let data = Node::slice(stream_of[i], at, p.data as u64);
                        at += p.data as u64;
                        (data_len, bytes) = (data_len + p.data, bytes + n + p.bytes as u64);
                        group.push((kept, data, p.record));
                    }
                    let tail = Node::concat(Vec::new());
                    apps.push(match u.by {
                        By::Lzx => {
                            let group = group.into_iter().map(|(kept, data, record)| aqr_lzx::formula::Stream {
                                kept,
                                data,
                                record,
                            });
                            aqr_lzx::formula::apply(&mut f, group.collect(), tail, bytes)
                        }
                        By::Xpress => {
                            let group = group.into_iter().map(|(kept, data, record)| aqr_xpress::formula::Stream {
                                kept,
                                data,
                                record,
                            });
                            aqr_xpress::formula::apply(&mut f, group.collect(), tail, bytes)
                        }
                        By::Lznt1 => {
                            let group = group.into_iter().map(|(kept, data, record)| aqr_lznt1::formula::Stream {
                                kept,
                                data,
                                record,
                            });
                            aqr_lznt1::formula::apply(&mut f, group.collect(), tail, bytes)
                        }
                    });
                }
                debug_assert_eq!(at, len);
                Node::concat(apps)
            }
            None => Node::slice(stream_of[i], 0, sizes.iter().sum()),
        };
        match js.len() {
            1 => whole[i] = Some(all),
            _ => split[i] = f.split_node(all, sizes).into_iter().map(Some).collect(),
        }
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
            Some((i, _)) if of[i].len() == 1 => whole[i].take().expect("its node"),
            Some((i, k)) => split[i][k].take().expect("each piece once"),
        });
    }
    f.root = Node::concat(parts);
    f.streams[REST_AT].data = rest.into_bytes();
    debug_assert_eq!(f.root.len(), input.len() as u64);
    f
}
