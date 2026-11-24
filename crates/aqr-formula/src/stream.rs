//! The streams of a formula ([`Stream`]), what is known of their bytes ([`Kind`]), and the
//! streams of entries by kind ([`ByKind`]).

use super::*;

/// The contents of the entries of an archive given one after another, as
/// [`Formula::push_by_kind`] makes streams of them: each kind's written as it comes
/// ([`store::Writer`]), so that whoever gives them holds none.
pub struct ByKind {
    own: usize,
    /// The kinds in the order of their first entry (Unknown first), each's writer and length.
    order: Vec<Kind>,
    writers: Vec<store::Writer>,
    lens: Vec<usize>,
    /// Each part's kind (index), start and length, and whether the next of its kind joins it.
    parts: Vec<(usize, usize, usize, bool)>,
    /// Whether all are of [`Kind::Unknown`] and shorter than `own`.
    simple: bool,
}

impl ByKind {
    /// Entries of at least `own` bytes are slices of their own.
    pub fn new(own: usize) -> ByKind {
        ByKind {
            own,
            order: vec![Kind::Unknown],
            writers: vec![store::Writer::new(None)],
            lens: vec![0],
            parts: Vec::new(),
            simple: true,
        }
    }

    /// The contents `d` of the next entry, of `kind`.
    pub fn add(&mut self, kind: Kind, d: &[u8]) {
        let len = d.len();
        self.simple &= kind == Kind::Unknown && len < self.own;
        let k = match self.order.iter().position(|&o| o == kind) {
            Some(k) => k,
            None => {
                self.order.push(kind);
                self.writers.push(store::Writer::new(None));
                self.lens.push(0);
                self.order.len() - 1
            }
        };
        let from = self.lens[k];
        self.writers[k].write(d);
        self.lens[k] += len;
        let open = !matches!(kind, Kind::Format(_) | Kind::Hint(_)) && len < self.own;
        match self.parts.last_mut() {
            _ if len == 0 => {}
            Some(p) if p.0 == k && p.3 && open => p.2 += len,
            _ => self.parts.push((k, from, len, open)),
        }
    }

    /// The streams made (those of [`Kind::Unknown`] stream `raw` of `f`, the others added after
    /// its streams, of class `{prefix}/{kind}`), and the node of their slices in order.
    pub fn push(self, f: &mut Formula, raw: usize, prefix: &str) -> Node {
        let mut kinds = self.order.into_iter().zip(self.writers);
        let (_, unknown) = kinds.next().expect("the unknown kind");
        f.streams[raw].data = unknown.into_bytes();
        if self.simple {
            return Node::slice(raw, 0, self.lens[0] as u64);
        }
        let base = f.streams.len() - 1;
        for (kind, w) in kinds {
            let class: Cow<'static, str> = match kind {
                Kind::Format(name) => format!("{prefix}/{name}").into(),
                kind => format!("{prefix}/{kind}").into(),
            };
            f.push_as(class, kind, w);
        }
        let stream = |k| if k == 0 { raw } else { base + k };
        let mut parts: Vec<Node> =
            self.parts.into_iter().map(|(k, start, len, _)| Node::slice(stream(k), start as u64, len as u64)).collect();
        if parts.len() == 1 { parts.pop().unwrap() } else { Node::Concat(parts) }
    }
}

/// Data extracted by a recompressor, which `slice` nodes point into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// What the bytes are, e.g. `x86/OP` or `flate/record`; streams of a class are alike.
    pub class: Cow<'static, str>,
    /// What the bytes are to whoever recompresses them in turn, alike for streams of a class.
    pub kind: Kind,
    /// Whether the bytes are data as they are, a file or what a file holds, rather than a
    /// representation of the recompressor's own: whoever takes the formula may recompress its
    /// slices in turn wherever they are (aqr-compress). A recompressor gives it, or an
    /// application's module, of the streams it takes as such ([`Formula::mark_contents`]).
    pub content: bool,
    /// Whether the stream is kept apart, never put with others ([`Formula::merge_classes`]):
    /// one that other formulas slice in pieces, each of which is to decode alone (the archive
    /// encoder stores each stream on its own).
    pub apart: bool,
    pub data: Bytes,
}

impl Stream {
    /// The stream of no bytes, of its class and kind.
    pub(crate) fn clone_empty(&self) -> Stream {
        Stream {
            class: self.class.clone(),
            kind: self.kind,
            content: self.content,
            apart: self.apart,
            data: Bytes::default(),
        }
    }
}

/// A piece of a stream: the stream, and where it starts and ends in it.
pub type Piece = (usize, u64, u64);

aqr_common::zopt_namespace!("merge", "streams of a class merged within the limit (aqr-formula)");

aqr_common::zopt! {
    /// The least pieces that [`Formula::merge_classes_within`] cuts streams into.
    pub static PIECE_MIN: Opt<usize> = Opt::size("merge:piece-min", 1 << 20, "least bytes of a piece that long streams are cut into to be merged");
}

/// Where a piece of a stream goes ([`Formula::merge_classes_within`]): where it starts, the
/// merged stream, and where in it.
pub(crate) type At = (u64, usize, u64);

aqr_common::zopt! {
    /// The most pieces that [`Formula::merge_classes_within`] puts together, each of a different
    /// stream (versions of a program, say).
    pub(crate) static GROUP_MOST: Opt<usize> = Opt::value("merge:group-most", 4, "most pieces of different streams merged together");
}

/// What a recompressor knows of a stream's bytes, for whoever takes them as data to recompress
/// in turn (aqr-compress, where they are an application's content) and for the archive
/// encoder. It says what the bytes are, not what is to be done with them: that is for whoever
/// takes them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Anything: files may be anywhere in it.
    #[default]
    Unknown,
    /// Samples of an image, uncompressed (rows perhaps filtered by a predictor).
    Pixels,
    /// Text, or a syntax in text such as PDF objects.
    Text,
    /// A font program (TrueType, OpenType, CFF, Type 1).
    Font,
    /// Binary records of fixed layout, such as a cross-reference table.
    Table,
    /// Of high entropy, such as compressed or encrypted data that no recompressor takes: holds
    /// no files and hardly compresses. The archive encoder stores it as it is unless zstd
    /// level 1 over all of it at once saves 1/32 (what repeats from afar, as a video's
    /// keyframes may), and keeps it apart from the streams of other kinds.
    Compressed,
    /// A file of the format named (as aqr-compress names them: `zlib`, `png`, ...) from the
    /// first byte: recompressed as such without looking for it, and kept as it is where its
    /// recompressor turns it down (but for the parts of it the recompressor tells, [`Region`]).
    /// A name not known is taken as [`Kind::Unknown`].
    Format(&'static str),
    /// What the encoder was told of the data (aqr-compress's hints, docs/hints-design.md), by its
    /// index in a table of the encoder's own: given by the encoder to a recompressor for the kinds
    /// of its entries, it is the encoder's to take. Whoever does not know the table takes it as
    /// [`Kind::Unknown`]; the archive encoder is not given it.
    Hint(u32),
}

/// What the entries of an archive are by their names (paths in it), as a recompressor of
/// archives is told: by default all of [`Kind::Unknown`].
#[derive(Clone)]
pub struct Kinds(std::sync::Arc<dyn Fn(&[u8]) -> Kind + Send + Sync>);

impl Kinds {
    pub fn new(f: impl Fn(&[u8]) -> Kind + Send + Sync + 'static) -> Kinds {
        Kinds(std::sync::Arc::new(f))
    }

    /// The kind of the entry named `name`.
    pub fn of(&self, name: &[u8]) -> Kind {
        (self.0)(name)
    }
}

impl Default for Kinds {
    fn default() -> Kinds {
        Kinds::new(|_| Kind::Unknown)
    }
}

impl fmt::Debug for Kinds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Kinds")
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Unknown => f.write_str("unknown"),
            Kind::Pixels => f.write_str("pixels"),
            Kind::Text => f.write_str("text"),
            Kind::Font => f.write_str("font"),
            Kind::Table => f.write_str("table"),
            Kind::Compressed => f.write_str("compressed"),
            Kind::Format(name) => write!(f, "format={name}"),
            Kind::Hint(_) => f.write_str("hint"),
        }
    }
}
