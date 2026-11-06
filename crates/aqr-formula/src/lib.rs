//! Entry formulas (docs/xform-design.md §4.2): what a recompressor makes of a file, as an
//! expression over the data it extracted rather than one opaque byte string.
//!
//! A [`Formula`] is a tree of [`Node`]s over [`Stream`]s:
//! - `slice`: a byte range of a stream;
//! - `concat`: nodes one after another;
//! - `pack`: nodes as a [`packed`] byte sequence, for a module taking a list of them as one
//!   input;
//! - `apply`: a [`Module`] (a pure function of its inputs, resources and parameters) applied
//!   to nodes, declaring the size of its output;
//! - `output`: an output of an application of several outputs, [`Shared`], which the formula
//!   lists apart and whose every output is taken by exactly one `output` node: an output of
//!   the module, or one split from its only output by the sizes declared;
//! - `patch`: a [`patch`] of a node against another one, its source.
//!
//! Streams are not placed yet: the archive encoder lays them out into non-leaf blocks, and
//! [`Stream::class`] tells it which streams are alike (to share frames and a dictionary, §3),
//! and [`Stream::kind`] what the recompressor knows of the bytes: whether they may hold files
//! for another recompressor to look for, are one such file, or are not worth looking into.
//! What belongs to the archive is left out: `ref` to other entries, and the resource
//! declarations of `apply` (memory limit, capacities), which the archive encoder
//! measures while verifying (§6). [`Resource`]s (§5.6), the models that modules take, are
//! held by the applications, and the archive encoder stores each once.
//!
//! [`Formula::eval`] is the reference, strict evaluation: every node is materialised, with
//! the modules run by a [`Modules`] implementation.
//!
//! [`aqr`] stores a formula as an Aqueous archive of the file alone.

pub mod aqr;
pub mod chash;
pub mod packed;
pub mod patch;
mod pieces;
pub mod probe;
pub mod store;

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// A module, by name and version of its input format. Eventually a Wasm module identified by
/// its hash; a native implementation stands in for it (§5.7). It is its name and version (as
/// equal and hashed); `memory` estimates the memory of its applications.
#[derive(Debug, Clone, Copy)]
pub struct Module {
    pub name: &'static str,
    pub version: u16,
    pub memory: Option<Estimate>,
}

/// What an application gives its module, for its memory estimate ([`Module::memory_usage_in_pages`]).
#[derive(Clone, Copy)]
pub struct Usage<'a> {
    pub params: &'a [u8],
    /// The lengths of the resources' contents, the inputs and the outputs.
    pub resources: &'a [u64],
    pub inputs: &'a [u64],
    pub outputs: &'a [u64],
    /// The inputs themselves (which whoever declares the memory limit has), for what their
    /// headers and indexes tell of the buffers the module makes: made as they are asked for.
    pub contents: &'a dyn Contents,
    /// The pages of the working memory as the module is instantiated (its data and stack).
    pub initial_pages: u64,
}

impl<'a> Usage<'a> {
    /// Bytes of the parameters, the resources and the inputs: what `aqr_api::whole` copies into
    /// the working memory.
    pub fn copied(&self) -> u64 {
        self.params.len() as u64 + self.resources.iter().sum::<u64>() + self.inputs.iter().sum::<u64>()
    }

    /// Bytes of the outputs.
    pub fn output(&self) -> u64 {
        self.outputs.iter().sum()
    }

    /// The `k`-th input ([`Usage::contents`]).
    pub fn content(&self, k: usize) -> Option<&'a [u8]> {
        self.contents.input(k)
    }

    /// The first `N` inputs, of `N` in all.
    pub fn all_contents<const N: usize>(&self) -> Option<[&'a [u8]; N]> {
        if self.inputs.len() != N {
            return None;
        }
        let mut out = [&[][..]; N];
        for (k, o) in out.iter_mut().enumerate() {
            *o = self.content(k)?;
        }
        Some(out)
    }

    /// Bytes of the working memory as it is instantiated.
    pub fn initial(&self) -> u64 {
        self.initial_pages * PAGE
    }

    /// The [`Estimate`] of a module that spawns no tasks, copies its parameters, resources and
    /// inputs (`aqr_api::whole`) and gives back its outputs from vectors of their sizes, taking
    /// `extra` bytes besides in `buffers` large allocations: all but the small allocations, for
    /// which there is [`SMALL`], and a page for each large one (the allocator's, as it grows the
    /// memory for it), of the inputs and outputs those of a page at least.
    pub fn whole(&self, extra: u64, buffers: u64) -> u64 {
        let large = self.outputs.iter().filter(|&&n| n >= PAGE).count() as u64;
        self.base() + self.output() + extra + (large + buffers) * PAGE
    }

    /// The working memory of a module that copies its parameters, resources and inputs
    /// (`aqr_api::whole`) as it starts (before its outputs and its own buffers), with the pages of
    /// slack of the large ones and [`SMALL`]: what its tasks copy where it spawns them first.
    pub fn base(&self) -> u64 {
        let large = [self.resources, self.inputs].concat().iter().filter(|&&n| n >= PAGE).count() as u64;
        self.initial() + self.copied() + large * PAGE + SMALL
    }
}

/// Bytes of an [`Estimate`] for the small allocations of a module (of no more than a few KiB in
/// all, whatever the input).
pub const SMALL: u64 = 64 << 10;

/// The inputs of an application, as an estimate reads them ([`Usage::contents`]).
pub trait Contents {
    /// The `k`-th input; None where there is none, or it could not be made.
    fn input(&self, k: usize) -> Option<&[u8]>;
}

impl Contents for Vec<&[u8]> {
    fn input(&self, k: usize) -> Option<&[u8]> {
        self.get(k).copied()
    }
}

/// A module's memory estimate: the bytes that the working memories of an application take at
/// most, of its instance and its tasks, with the messages queued in its channels (the memory
/// limit less the other memories); None where it cannot tell.
pub type Estimate = fn(&Usage<'_>) -> Option<u64>;

/// Bytes of a page of WebAssembly memory.
pub const PAGE: u64 = 65536;

impl Module {
    /// The module `name@version`, of no memory estimate.
    pub const fn new(name: &'static str, version: u16) -> Module {
        Module { name, version, memory: None }
    }

    /// The module estimating its memory by `memory`.
    pub const fn with_memory(self, memory: Estimate) -> Module {
        Module { memory: Some(memory), ..self }
    }

    /// The pages that the working memories of an application of `usage` take at most, which
    /// the module may take to be there (see [`Estimate`]); None where it has no estimate.
    pub fn memory_usage_in_pages(&self, usage: &Usage<'_>) -> Option<u64> {
        Some((self.memory?)(usage)?.div_ceil(PAGE))
    }
}

impl PartialEq for Module {
    fn eq(&self, other: &Module) -> bool {
        (self.name, self.version) == (other.name, other.version)
    }
}

impl Eq for Module {}

impl std::hash::Hash for Module {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        (self.name, self.version).hash(h);
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

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
        let open = !matches!(kind, Kind::Format(_)) && len < self.own;
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
    /// slices in turn wherever they are (aqr-compress, as it does an application's content).
    pub content: bool,
    /// Whether the stream is kept apart, never put with others ([`Formula::merge_classes`]):
    /// one that other formulas slice in pieces, each of which is to decode alone (the archive
    /// encoder stores each stream on its own).
    pub apart: bool,
    pub data: Bytes,
}

impl Stream {
    /// The stream of no bytes, of its class and kind.
    fn clone_empty(&self) -> Stream {
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

/// The least pieces that [`Formula::merge_classes_within`] cuts streams into.
pub const PIECE_MIN: u64 = 1 << 20;

/// Where a piece of a stream goes ([`Formula::merge_classes_within`]): where it starts, the
/// merged stream, and where in it.
type At = (u64, usize, u64);

/// The most pieces that [`Formula::merge_classes_within`] puts together, each of a different
/// stream (versions of a program, say).
const GROUP_MOST: usize = 4;

/// The bytes of a [`Stream`]: its own, or shared with whoever else holds them (a file mapped
/// into memory, which the system may drop and read again as it likes, rather than a copy of
/// it). Changing them makes them its own first. Or bytes written to a sink already
/// ([`aqr::sink::Sink`]), of a stream of it: their length only, slices of the stream being
/// where they are in the sink's stream; reading or changing them is a bug. Or bytes not made
/// yet ([`Bytes::lazy`]): their length, and what makes them ([`Bytes::made`]) where they are
/// needed, each time, so that whoever needs them holds them only as long as it does.
#[derive(Clone)]
pub struct Bytes(Repr);

#[derive(Clone)]
enum Repr {
    Owned(Vec<u8>),
    Shared(Arc<dyn AsRef<[u8]> + Send + Sync>),
    Sunk { stream: usize, len: usize },
    Lazy(Arc<dyn Fn() -> Bytes + Send + Sync>, usize),
}

impl Bytes {
    pub fn shared(data: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Bytes {
        Bytes(Repr::Shared(data))
    }

    /// Stream `stream` of a sink, of `len` bytes so far ([`Bytes`]).
    pub fn sunk(stream: usize, len: usize) -> Bytes {
        Bytes(Repr::Sunk { stream, len })
    }

    /// Bytes of `len`, not made yet: `make` makes them where they are needed ([`Bytes::made`]).
    pub fn lazy(len: usize, make: impl Fn() -> Bytes + Send + Sync + 'static) -> Bytes {
        Bytes(Repr::Lazy(Arc::new(make), len))
    }

    /// Whether the bytes are not made yet ([`Bytes::lazy`]).
    pub fn is_lazy(&self) -> bool {
        matches!(self.0, Repr::Lazy(..))
    }

    /// The bytes, made where they were not yet (each time: they are not kept), else shared.
    pub fn made(&self) -> Bytes {
        match &self.0 {
            Repr::Lazy(make, len) => {
                let b = make();
                assert_eq!(b.len(), *len, "bytes made of the length told");
                b
            }
            _ => self.clone(),
        }
    }

    /// The stream of the sink these bytes are, if they are sunk.
    pub fn sunk_in(&self) -> Option<usize> {
        match self.0 {
            Repr::Sunk { stream, .. } => Some(stream),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Owned(v) => v.len(),
            Repr::Shared(d) => (**d).as_ref().len(),
            &Repr::Sunk { len, .. } | &Repr::Lazy(_, len) => len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the bytes are shared rather than its own: changing them would copy them all.
    /// (Bytes sunk are not.)
    pub fn is_shared(&self) -> bool {
        matches!(self.0, Repr::Shared(_))
    }

    /// The bytes shared from now on, without a copy: a clone shares them rather than copies
    /// them.
    pub fn share(&mut self) {
        if let Repr::Owned(v) = &mut self.0 {
            self.0 = Repr::Shared(Arc::new(std::mem::take(v)));
        }
    }

    /// The bytes as its own, copied first where they are shared.
    pub fn to_mut(&mut self) -> &mut Vec<u8> {
        match &self.0 {
            Repr::Shared(d) => self.0 = Repr::Owned(d.as_ref().as_ref().to_vec()),
            &Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
            Repr::Owned(_) => {}
        }
        let Repr::Owned(v) = &mut self.0 else { unreachable!() };
        v
    }

    /// Its first `len` bytes only.
    pub fn prefix(self, len: usize) -> Bytes {
        match self.0 {
            Repr::Owned(mut v) => {
                v.truncate(len);
                Bytes(Repr::Owned(v))
            }
            Repr::Shared(d) if len >= (*d).as_ref().len() => Bytes(Repr::Shared(d)),
            Repr::Shared(d) => Bytes::shared(Arc::new(Slice(d, 0..len))),
            Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }

    /// Bytes `range` of it: sharing them where they are shared, else a copy.
    pub fn slice(&self, range: std::ops::Range<usize>) -> Bytes {
        match &self.0 {
            Repr::Owned(v) => Bytes(Repr::Owned(v[range].to_vec())),
            Repr::Shared(d) => {
                assert!(range.start <= range.end && range.end <= (**d).as_ref().len(), "a slice within");
                Bytes::shared(Arc::new(Slice(d.clone(), range)))
            }
            &Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }

    pub fn into_vec(self) -> Vec<u8> {
        match self.0 {
            Repr::Owned(v) => v,
            Repr::Shared(d) => d.as_ref().as_ref().to_vec(),
            Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }

    pub fn extend_from_slice(&mut self, d: &[u8]) {
        self.to_mut().extend_from_slice(d)
    }

    pub fn truncate(&mut self, len: usize) {
        if len < self.len() {
            self.to_mut().truncate(len)
        }
    }

    pub fn shrink_to_fit(&mut self) {
        if let Repr::Owned(v) = &mut self.0 {
            v.shrink_to_fit()
        }
    }
}

/// Reading bytes sunk ([`Bytes::sunk`]): a bug.
#[cold]
fn sunk(stream: usize) -> ! {
    panic!("the bytes of stream {stream} of the sink are not at hand")
}

/// Reading bytes not made ([`Bytes::lazy`]) as they are: a bug.
#[cold]
fn lazy() -> ! {
    panic!("bytes not made yet read as they are")
}

/// Bytes of bytes shared ([`Bytes::prefix`], [`Bytes::slice`]).
struct Slice(Arc<dyn AsRef<[u8]> + Send + Sync>, std::ops::Range<usize>);

impl AsRef<[u8]> for Slice {
    fn as_ref(&self) -> &[u8] {
        &(*self.0).as_ref()[self.1.clone()]
    }
}

// (bytes shared are only read)
impl std::panic::UnwindSafe for Bytes {}
impl std::panic::RefUnwindSafe for Bytes {}

impl Default for Bytes {
    fn default() -> Bytes {
        Bytes(Repr::Owned(Vec::new()))
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Bytes {
        Bytes(Repr::Owned(v))
    }
}

impl From<Bytes> for Vec<u8> {
    fn from(b: Bytes) -> Vec<u8> {
        b.into_vec()
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Owned(v) => v,
            Repr::Shared(d) => d.as_ref().as_ref(),
            &Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl std::ops::DerefMut for Bytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.to_mut()
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Bytes) -> bool {
        match (&self.0, &other.0) {
            (Repr::Sunk { stream: a, len: m }, Repr::Sunk { stream: b, len: n }) => (a, m) == (b, n),
            (Repr::Sunk { .. }, _) | (_, Repr::Sunk { .. }) => false,
            (Repr::Lazy(a, m), Repr::Lazy(b, n)) => Arc::ptr_eq(a, b) && m == n,
            (Repr::Lazy(..), _) | (_, Repr::Lazy(..)) => false,
            _ => **self == **other,
        }
    }
}

impl Eq for Bytes {}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        **self == *other
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for Bytes {
    fn eq(&self, other: &&[u8; N]) -> bool {
        **self == other[..]
    }
}

impl PartialEq<Vec<u8>> for Bytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        **self == other[..]
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Repr::Sunk { stream, len } => write!(f, "Sunk {{ stream: {stream}, len: {len} }}"),
            Repr::Lazy(_, len) => write!(f, "Lazy {{ len: {len} }}"),
            _ => fmt::Debug::fmt(&**self, f),
        }
    }
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
        }
    }
}

/// What a part of a file is, as the recompressor of its format knows it: for whoever looks
/// into a file that recompressor turned down (aqr-compress, scanning it for files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The bytes of the part, in the file.
    pub span: std::ops::Range<usize>,
    pub kind: Kind,
    /// For a [`Kind::Format`] that holds data (as zlib does), the kind of that data.
    pub contents: Kind,
}

/// Bytes that applications of a module share, typically a model (§5.6): a resource block of
/// an archive. Clones share the bytes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Resource {
    data: Arc<[u8]>,
    /// The data of its block as stored, where it is given ([`Resource::stored`]).
    stored: Option<Arc<[u8]>>,
}

impl Resource {
    pub fn new(data: impl Into<Arc<[u8]>>) -> Resource {
        Resource { data: data.into(), stored: None }
    }

    /// The resource whose block has data `stored` as it is (checksummed Zstandard data, as of a
    /// resource or module block of another archive): for [`Node::ResourceBytes`]. A resource of
    /// the same bytes not stored so is another resource, of a block of its own. Fails where
    /// `stored` is no such data.
    pub fn stored(stored: impl Into<Arc<[u8]>>) -> Result<Resource> {
        let stored = stored.into();
        let data = aqr::stored_decoded(&stored).ok_or_else(|| Error::new("no checksummed Zstandard data"))?;
        Ok(Resource { data: data.into(), stored: Some(stored) })
    }

    /// The data of its block as stored, if given ([`Resource::stored`]).
    pub fn stored_data(&self) -> Option<&[u8]> {
        self.stored.as_deref()
    }
}

impl std::ops::Deref for Resource {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Resource({} bytes, {:016x}", self.len(), xxhash_rust::xxh64::xxh64(self, 0))?;
        match &self.stored {
            Some(s) => write!(f, ", stored in {} bytes)", s.len()),
            None => write!(f, ")"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// `len` bytes of stream `stream` (an index into [`Formula::streams`]) from `start`.
    Slice {
        stream: usize,
        start: u64,
        len: u64,
    },
    Concat(Vec<Node>),
    /// The [`packed`] byte sequence of the nodes.
    Pack(Vec<Node>),
    Apply(Box<Apply>),
    /// Output `index` (of `len` bytes) of the application of several outputs
    /// [`Formula::shared`]`[shared]`. Each output is taken exactly once.
    Output {
        shared: usize,
        index: usize,
        len: u64,
    },
    Patch(Box<Patch>),
    /// The data of the module block of `module` as stored, which is to be these bytes (a
    /// ModuleBytes instruction): those of a module block of another archive, as when it is
    /// archived. The encoder fails where its block of the module is other bytes.
    ModuleBytes(Module, Arc<[u8]>),
    /// The data of the resource block of a resource [`Resource::stored`] as stored (a
    /// ResourceBytes instruction).
    ResourceBytes(Resource),
}

/// The output of `patch` ([`patch`]), of `len` bytes, against `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub source: Node,
    pub patch: Node,
    pub len: u64,
}

/// An application of several outputs, which [`Node::Output`]s take one each (anywhere in the
/// formula: in its root, or in the inputs of a later one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    /// The application, its `len` the outputs' sizes added up.
    pub apply: Apply,
    /// Bytes of each output.
    pub sizes: Vec<u64>,
    /// Whether the module outputs the concatenation of the outputs, which a Split instruction
    /// separates ([`Formula::split`]), for a number of outputs that varies between applications;
    /// otherwise each is an output of the module ([`Formula::share`]).
    pub split: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apply {
    pub module: Module,
    /// Module-defined, in order: models and the like that applications share.
    pub resources: Vec<Resource>,
    /// Module-defined; small values such as a code's origin.
    pub params: Vec<u8>,
    pub inputs: Vec<Node>,
    /// Bytes of the output.
    pub len: u64,
    /// The memory limit its instruction declares ([`NO_LIMIT`] until [`Formula::declare_memory`]
    /// sets it).
    pub memory_limit: u64,
}

/// The memory limit of an application declaring none.
pub const NO_LIMIT: u64 = u64::MAX;

impl Node {
    pub fn slice(stream: usize, start: u64, len: u64) -> Node {
        Node::Slice { stream, start, len }
    }

    /// The concatenation of `nodes`, flattened: nested concatenations are spliced in, empty
    /// slices dropped and adjacent slices of a stream merged; a single node is itself.
    pub fn concat(nodes: impl IntoIterator<Item = Node>) -> Node {
        fn push(out: &mut Vec<Node>, n: Node) {
            match n {
                Node::Concat(ns) => ns.into_iter().for_each(|n| push(out, n)),
                Node::Slice { len: 0, .. } => {}
                Node::Slice { stream, start, len } => match out.last_mut() {
                    Some(Node::Slice { stream: s, start: a, len: l }) if *s == stream && *a + *l == start => *l += len,
                    _ => out.push(n),
                },
                n => out.push(n),
            }
        }
        let mut out = Vec::new();
        nodes.into_iter().for_each(|n| push(&mut out, n));
        if out.len() == 1 { out.pop().unwrap() } else { Node::Concat(out) }
    }

    /// This node with every concatenation below it, itself among them, flattened as
    /// [`Node::concat`] does: one put in place of a slice within another (in
    /// [`Formula::share_slices`], or a formula embedded) is spliced into it. The parts of a pack
    /// stay its parts. The output is the same.
    pub fn flatten(&mut self) {
        match self {
            Node::Concat(ns) => {
                ns.iter_mut().for_each(Node::flatten);
                *self = Node::concat(std::mem::take(ns));
            }
            Node::Pack(ns) => ns.iter_mut().for_each(Node::flatten),
            Node::Apply(a) => a.inputs.iter_mut().for_each(Node::flatten),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(Node::flatten),
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        }
    }

    /// This node, a slice or a concatenation of slices (as [`Formula::push_by_kind`] gives the
    /// contents of an archive's members), cut into nodes of `sizes`, which add up to its bytes.
    /// Panics on another node, or where the sizes add up to more.
    pub fn cut(self, sizes: &[u64]) -> Vec<Node> {
        let slices = match self {
            Node::Concat(ns) => ns,
            n => vec![n],
        };
        let mut slices = slices.into_iter().map(|s| match s {
            Node::Slice { stream, start, len } => (stream, start, len),
            _ => panic!("slices of contents"),
        });
        let mut cur = slices.next();
        sizes
            .iter()
            .map(|&size| {
                let (mut parts, mut want) = (Vec::new(), size);
                while want > 0 {
                    let (s, a, l) = cur.expect("as many bytes as the sizes");
                    let k = l.min(want);
                    parts.push(Node::slice(s, a, k));
                    want -= k;
                    cur = if k == l { slices.next() } else { Some((s, a + k, l - k)) };
                }
                Node::concat(parts)
            })
            .collect()
    }

    /// The [`packed`] byte sequence of `nodes`, as they are.
    pub fn pack(nodes: impl IntoIterator<Item = Node>) -> Node {
        Node::Pack(nodes.into_iter().collect())
    }

    /// An application without resources.
    pub fn apply(module: Module, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::apply_with(module, Vec::new(), params, inputs, len)
    }

    pub fn apply_with(module: Module, resources: Vec<Resource>, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::Apply(Box::new(Apply { module, resources, params, inputs, len, memory_limit: NO_LIMIT }))
    }

    /// The output of `patch`, of `len` bytes, against `source` ([`patch::make`]).
    pub fn patch(source: Node, patch: Node, len: u64) -> Node {
        Node::Patch(Box::new(Patch { source, patch, len }))
    }

    /// Bytes of the output, as declared.
    pub fn len(&self) -> u64 {
        match self {
            Node::Slice { len, .. } => *len,
            Node::Concat(ns) => ns.iter().map(Node::len).sum(),
            Node::Pack(ns) => packed::header_len(ns.len()) + ns.iter().map(Node::len).sum::<u64>(),
            Node::Apply(a) => a.len,
            Node::Output { len, .. } => *len,
            Node::Patch(p) => p.len,
            Node::ModuleBytes(_, d) => d.len() as u64,
            Node::ResourceBytes(r) => r.stored_data().map_or(0, |d| d.len() as u64),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Calls `f` on this node and every node below it, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        match self {
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| n.visit(f)),
            Node::Apply(a) => a.inputs.iter().for_each(|n| n.visit(f)),
            Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| n.visit(f)),
        }
    }

    /// Calls `f` on every slice below this node, as `(stream, start, len)`.
    fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        self.visit_mut(&mut |n| {
            if let Node::Slice { stream, start, len } = n {
                f(stream, start, len)
            }
        });
    }

    /// Replaces every slice below this node with what `f` makes of it, of `(stream, start,
    /// len)`; what it makes is not visited.
    fn map_slices(&mut self, f: &mut impl FnMut(usize, u64, u64) -> Node) {
        match self {
            &mut Node::Slice { stream, start, len } => *self = f(stream, start, len),
            Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| n.map_slices(f)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.map_slices(f)),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| n.map_slices(f)),
        }
    }

    /// Calls `f` on this node and every node below it, parents first.
    fn visit_mut(&mut self, f: &mut impl FnMut(&mut Node)) {
        f(self);
        match self {
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| n.visit_mut(f)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.visit_mut(f)),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| n.visit_mut(f)),
        }
    }

    /// Moves the streams the slices below take by `streams`, and the applications of several
    /// outputs the outputs below take by `shared`.
    fn shift(&mut self, streams: usize, shared: usize) {
        self.visit_mut(&mut |n| match n {
            Node::Slice { stream, .. } => *stream += streams,
            Node::Output { shared: s, .. } => *s += shared,
            _ => {}
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub streams: Vec<Stream>,
    pub root: Node,
    /// The applications of several outputs, in order: the inputs of one take only outputs of
    /// those before it.
    pub shared: Vec<Shared>,
}

impl Default for Formula {
    fn default() -> Formula {
        Formula { streams: Vec::new(), root: Node::Concat(Vec::new()), shared: Vec::new() }
    }
}

impl Formula {
    /// `data` as it is: a slice of a single stream.
    pub fn raw(class: impl Into<Cow<'static, str>>, data: Vec<u8>) -> Formula {
        let mut f = Formula::default();
        f.root = f.push(class, data);
        f
    }

    /// Adds a stream of [`Kind::Unknown`], returning a slice of all of it.
    pub fn push(&mut self, class: impl Into<Cow<'static, str>>, data: impl Into<Bytes>) -> Node {
        self.push_as(class, Kind::Unknown, data)
    }

    /// Adds a stream of `kind`, returning a slice of all of it.
    pub fn push_as(&mut self, class: impl Into<Cow<'static, str>>, kind: Kind, data: impl Into<Bytes>) -> Node {
        self.push_stream(Stream { class: class.into(), kind, content: false, apart: false, data: data.into() })
    }

    /// Adds a stream of [`Kind::Unknown`] that is [`Stream::apart`], returning a slice of all of it.
    pub fn push_apart(&mut self, class: impl Into<Cow<'static, str>>, data: impl Into<Bytes>) -> Node {
        self.push_stream(Stream {
            class: class.into(),
            kind: Kind::Unknown,
            content: false,
            apart: true,
            data: data.into(),
        })
    }

    /// Adds a stream of `kind` that is [`Stream::content`], returning a slice of all of it.
    pub fn push_content(&mut self, class: impl Into<Cow<'static, str>>, kind: Kind, data: impl Into<Bytes>) -> Node {
        self.push_stream(Stream { class: class.into(), kind, content: true, apart: false, data: data.into() })
    }

    fn push_stream(&mut self, s: Stream) -> Node {
        let len = s.data.len() as u64;
        self.streams.push(s);
        Node::slice(self.streams.len() - 1, 0, len)
    }

    /// Adds the application `a` of outputs of `sizes` bytes (its `len` set to their sum),
    /// returning the outputs to be placed in the formula, each once.
    pub fn share(&mut self, a: Apply, sizes: Vec<u64>) -> Vec<Node> {
        self.push_shared(a, sizes, false)
    }

    /// [`Formula::share`] for a module of one output, the concatenation of the outputs of
    /// `sizes` bytes, which are split from it.
    pub fn split(&mut self, a: Apply, sizes: Vec<u64>) -> Vec<Node> {
        self.push_shared(a, sizes, true)
    }

    fn push_shared(&mut self, mut a: Apply, sizes: Vec<u64>, split: bool) -> Vec<Node> {
        a.len = sizes.iter().sum();
        let shared = self.shared.len();
        let outputs = sizes.iter().enumerate().map(|(index, &len)| Node::Output { shared, index, len }).collect();
        self.shared.push(Shared { apply: a, sizes, split });
        outputs
    }

    /// Takes over the streams and the applications of several outputs of `child`, returning
    /// its root to be placed in this formula.
    pub fn embed(&mut self, child: Formula) -> Node {
        let (streams, shared) = (self.streams.len(), self.shared.len());
        let mut root = child.root;
        root.shift(streams, shared);
        self.streams.extend(child.streams);
        for mut s in child.shared {
            s.apply.inputs.iter_mut().for_each(|n| n.shift(streams, shared));
            self.shared.push(s);
        }
        root
    }

    /// Puts the applications of several outputs in `order` (the old index of each, all once),
    /// their outputs taken where they were.
    pub fn reorder_shared(&mut self, order: &[usize]) {
        let mut to = vec![usize::MAX; self.shared.len()];
        for (new, &old) in order.iter().enumerate() {
            debug_assert_eq!(to[old], usize::MAX, "each once");
            to[old] = new;
        }
        debug_assert!(order.len() == to.len());
        let mut old: Vec<Option<Shared>> = std::mem::take(&mut self.shared).into_iter().map(Some).collect();
        self.shared = order.iter().map(|&i| old[i].take().expect("each once")).collect();
        let mut remap = |n: &mut Node| {
            if let Node::Output { shared, .. } = n {
                *shared = to[*shared];
            }
        };
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.visit_mut(&mut remap));
        self.root.visit_mut(&mut remap);
    }

    /// Calls `f` on every node of the inputs of the applications of several outputs, in order,
    /// then of the root, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        self.shared.iter().flat_map(|s| &s.apply.inputs).for_each(|n| n.visit(f));
        self.root.visit(f);
    }

    /// Replaces every slice of the inputs of the applications of several outputs and of the
    /// root with what `f` makes of it, of `(stream, start, len)`; what it makes is not visited.
    pub fn map_slices(&mut self, f: &mut impl FnMut(usize, u64, u64) -> Node) {
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.map_slices(f));
        self.root.map_slices(f);
    }

    /// Calls `f` on every slice of the inputs of the applications of several outputs and of the
    /// root, as `(stream, start, len)`, which it may change.
    pub fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.slices_mut(f));
        self.root.slices_mut(f);
    }

    /// Drops what no slice points to: the bytes of a stream outside every slice of it, and
    /// streams without slices. The output is the same: a stream with a slice out of it is
    /// kept whole, and so is one sunk ([`Bytes::sunk`]), and slices of streams that do not
    /// exist still point to none.
    pub fn compact(&mut self) {
        // Per stream, the ranges of its slices, merged into (start, end, start in the new data).
        let mut ranges = vec![Vec::new(); self.streams.len()];
        self.slices_mut(&mut |&mut s, &mut a, &mut l| {
            if let Some(r) = ranges.get_mut(s) {
                r.push((a, a.saturating_add(l), 0));
            }
        });
        let old = self.streams.len();
        let mut index = vec![usize::MAX; old];
        let mut streams = Vec::new();
        for (i, (s, r)) in std::mem::take(&mut self.streams).into_iter().zip(&mut ranges).enumerate() {
            if r.is_empty() {
                continue;
            }
            index[i] = streams.len();
            // (a stream sunk kept whole: its slices are where its bytes are in the sink)
            if s.data.sunk_in().is_some() || r.iter().any(|&(_, b, _)| b > s.data.len() as u64) {
                *r = vec![(0, u64::MAX, 0)];
                streams.push(s);
                continue;
            }
            r.sort_unstable();
            let mut merged: Vec<(u64, u64, u64)> = Vec::new();
            for &(a, b, _) in r.iter() {
                match merged.last_mut() {
                    Some(m) if a <= m.1 => m.1 = m.1.max(b),
                    _ => merged.push((a, b, 0)),
                }
            }
            let mut at = 0;
            for m in &mut merged {
                m.2 = at;
                at += m.1 - m.0;
            }
            let data = if s.data.is_shared() && at < s.data.len() as u64 {
                // (bytes shared copied only as far as they are kept, to the store where one is set:
                // they were most likely of it)
                let mut d = store::Writer::new(Some(at as usize));
                for m in &merged {
                    d.write(&s.data[m.0 as usize..m.1 as usize]);
                }
                d.into_bytes()
            } else {
                // in place: the bytes kept only move towards the start
                let mut data = s.data;
                for m in &merged {
                    if m.0 != m.2 {
                        data.copy_within(m.0 as usize..m.1 as usize, m.2 as usize);
                    }
                }
                data.truncate(at as usize);
                data.shrink_to_fit();
                data
            };
            *r = merged;
            streams.push(Stream { data, ..s });
        }
        let new = streams.len();
        self.slices_mut(&mut |s, a, _| match ranges.get(*s) {
            Some(r) => {
                let m = r[r.partition_point(|m| m.0 <= *a) - 1];
                *a = m.2 + (*a - m.0);
                *s = index[*s];
            }
            None => *s = *s - old + new,
        });
        self.streams = streams;
    }

    /// Takes the slices of at least `min` bytes from other slices that hold the same bytes,
    /// and drops what is left without a slice ([`Formula::compact`]): a file and the same file
    /// in an archive, say, are then stored once. From the shortest slice on, each is cut into
    /// slices kept before that it starts with, one after another (the longest each time; else
    /// the longest start of one, of at least half of it), and what follows of its own; one cut
    /// nowhere is kept. A piece found again right after itself is a run of the same bytes
    /// (padding, zeros), which compresses to little: the rest of the run is kept of the slice's
    /// own bytes instead of cut into a slice of each copy. Slices past the end of their stream,
    /// and those of streams sunk ([`Bytes::sunk`], which a sink shares as it takes them), are
    /// left as they are. The concatenations below the root's parts are then flattened
    /// ([`Node::flatten`]), the root's parts (a batch's files) staying its parts. The output is
    /// the same.
    pub fn share_slices(&mut self, min: usize) {
        type Sl = (usize, u64, u64);
        let mut slices: Vec<Sl> = Vec::new();
        self.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && len >= min as u64
                && self.streams.get(stream).is_some_and(|s| {
                    s.data.sunk_in().is_none() && start.checked_add(len).is_some_and(|e| e <= s.data.len() as u64)
                })
            {
                slices.push((stream, start, len));
            }
        });
        slices.sort_unstable_by_key(|&(s, a, l)| (l, s, a));
        slices.dedup();
        let bytes = |&(s, a, l): &Sl| &self.streams[s].data[a as usize..][..l as usize];
        let head = |b: &[u8]| xxhash_rust::xxh3::xxh3_64(&b[..min]);
        // the slices kept, by their first `min` bytes
        let mut kept: std::collections::HashMap<u64, Vec<Sl>> = Default::default();
        let mut to: std::collections::HashMap<Sl, Vec<Sl>> = Default::default();
        for x in slices {
            let b = bytes(&x);
            let (mut at, mut pieces) = (0usize, Vec::new());
            while b.len() - at >= min {
                let rest = &b[at..];
                // (a kept slice the rest starts with, the longest; else the start of one, at least
                // half of it: of a text coded alike, its end cut by a token)
                let ks = kept.get(&head(rest)).map_or(&[][..], |ks| &ks[..]);
                let whole = ks.iter().filter(|k| rest.starts_with(bytes(k))).max_by_key(|k| k.2).copied();
                let found = whole.or_else(|| {
                    let common = |k: &Sl| bytes(k).iter().zip(rest).position(|(a, b)| a != b).unwrap_or(0);
                    ks.iter()
                        .map(|k| (k, common(k)))
                        .filter(|&(k, n)| n >= min && 2 * n as u64 >= k.2)
                        .max_by_key(|&(_, n)| n)
                        .map(|(k, n)| (k.0, k.1, n as u64))
                });
                let Some(k) = found else { break };
                if pieces.last() == Some(&k) {
                    // (the run of its copies, of its length as the period, of the slice's own)
                    let p = k.2 as usize;
                    let run = b[at..].iter().zip(&b[at - p..]).take_while(|(a, b)| a == b).count();
                    pieces.push((x.0, x.1 + at as u64, run as u64));
                    at += run;
                    continue;
                }
                pieces.push(k);
                at += k.2 as usize;
            }
            if at == 0 {
                kept.entry(head(b)).or_default().push(x);
                continue;
            }
            if at < b.len() {
                let rest = (x.0, x.1 + at as u64, x.2 - at as u64);
                match to.get(&rest) {
                    Some(ps) => pieces.extend_from_slice(&ps.clone()),
                    None => {
                        if rest.2 >= min as u64 {
                            kept.entry(head(bytes(&rest))).or_default().push(rest);
                        }
                        pieces.push(rest);
                    }
                }
            }
            to.insert(x, pieces);
        }
        if to.is_empty() {
            self.flatten_parts();
            return;
        }
        // (the pieces are slices kept, not cut again)
        let mut cut = |n: &mut Node| {
            if let &mut Node::Slice { stream, start, len } = n
                && let Some(ps) = to.get(&(stream, start, len))
            {
                *n = Node::concat(ps.iter().map(|&(s, a, l)| Node::slice(s, a, l)));
            }
        };
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.visit_mut(&mut cut));
        self.root.visit_mut(&mut cut);
        self.flatten_parts();
        self.compact();
    }

    /// [`Node::flatten`] of the inputs of the applications of several outputs and of the root's
    /// parts, or of the root where it is not a concatenation: the root's parts stay its parts.
    fn flatten_parts(&mut self) {
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(Node::flatten);
        match &mut self.root {
            Node::Concat(ns) => ns.iter_mut().for_each(Node::flatten),
            n => n.flatten(),
        }
    }

    /// Puts the streams of each class and kind one after another, in one stream (in the place
    /// of the first): they are alike, and archive smaller together (the archive encoder
    /// compresses each stream on its own). The streams of [`Kind::Compressed`] (mostly stored as
    /// they are), the contents, those [`Stream::apart`], and those that a slice goes past the end
    /// of are left apart. The output is the same.
    pub fn merge_classes(&mut self) {
        self.merge_classes_within(u64::MAX);
    }

    /// [`Formula::merge_classes`], into streams of `limit` bytes at most: a stream that would
    /// take one past it starts another of its class and kind (one longer is left as it is).
    /// Where a class and kind has several streams longer than half of it, `k` of them (at most
    /// [`GROUP_MOST`]), they are cut into pieces of a `k`th of it (of [`PIECE_MIN`] at least),
    /// and each piece put with those of the others most like it, if any, up to `k` pieces of
    /// different streams (versions of a program's code, say): the pieces of a stream not so
    /// put are one stream, and a stream none of whose pieces is put so is left whole.
    pub fn merge_classes_within(&mut self, limit: u64) {
        let (to, groups) = self.merge_plan(limit, false);
        let mut old: Vec<Option<Stream>> = std::mem::take(&mut self.streams).into_iter().map(Some).collect();
        // (the pieces of each stream not copied yet: it is let go after its last)
        let mut left = vec![0usize; old.len()];
        groups.iter().flatten().for_each(|p| left[p.0] += 1);
        let mut streams = Vec::with_capacity(groups.len());
        for g in &groups {
            let (s, a, b) = g[0];
            let o = old[s].as_ref().expect("a stream in one group");
            // (a whole stream first taken, of its length at once, the others let go as they are copied)
            let (mut first, rest) = if a == 0 && b == o.data.len() as u64 {
                left[s] -= 1;
                (old[s].take().unwrap(), &g[1..])
            } else {
                (
                    Stream {
                        class: o.class.clone(),
                        kind: o.kind,
                        content: o.content,
                        apart: o.apart,
                        data: Bytes::default(),
                    },
                    &g[..],
                )
            };
            if !rest.is_empty() {
                let total: u64 = g.iter().map(|p| p.2 - p.1).sum();
                let data = first.data.to_mut();
                data.reserve_exact(total as usize - data.len());
                for &(s, a, b) in rest {
                    data.extend_from_slice(
                        &old[s].as_ref().expect("a stream in one group").data[a as usize..b as usize],
                    );
                    left[s] -= 1;
                    if left[s] == 0 {
                        old[s] = None;
                    }
                }
            }
            streams.push(first);
        }
        self.remap(&to, groups.len());
        self.streams = streams;
    }

    /// [`Formula::merge_classes`] as the formula is archived, without moving the bytes: the
    /// contents merged too, as nothing recompresses them any more (a stream of each file of an
    /// image, say), as a tree's are. The formula of the streams merged (their bytes empty), and
    /// of each of its streams, the pieces of those of `self` it is the concatenation of, in
    /// order. Streams sunk ([`Bytes::sunk`]) are of no group: their slices are of the sink's
    /// streams, stream `k` of it after the groups (the formula's streams `groups.len() + k`,
    /// which whoever writes it adds).
    pub fn merged(&self) -> (Formula, Vec<Vec<Piece>>) {
        let (to, groups) = self.merge_plan(u64::MAX, true);
        let streams = groups
            .iter()
            .map(|g| {
                let s = &self.streams[g[0].0];
                let content = g.iter().all(|p| self.streams[p.0].content);
                Stream { class: s.class.clone(), kind: s.kind, content, apart: s.apart, data: Bytes::default() }
            })
            .collect();
        let mut f = Formula { streams: Vec::new(), root: self.root.clone(), shared: self.shared.clone() };
        f.remap(&to, groups.len());
        f.streams = streams;
        (f, groups)
    }

    /// Of each stream, its pieces in order, each as where it starts, the merged stream it goes
    /// to and where in it; and of each merged stream, the pieces it is made of, in order
    /// ([`Formula::merge_classes_within`]); the contents merged as well where `contents`.
    fn merge_plan(&self, limit: u64, contents: bool) -> (Vec<Vec<At>>, Vec<Vec<Piece>>) {
        let n = self.streams.len();
        let lens: Vec<u64> = self.streams.iter().map(|s| s.data.len() as u64).collect();
        let mut past = vec![false; n];
        let mut visit = |n: &Node| {
            n.visit(&mut |n| {
                if let &Node::Slice { stream, start, len } = n
                    && let Some(p) = past.get_mut(stream)
                {
                    *p |= start.checked_add(len).is_none_or(|e| e > lens[stream]);
                }
            })
        };
        self.shared.iter().flat_map(|s| &s.apply.inputs).for_each(&mut visit);
        visit(&self.root);
        let alike = |i: usize| {
            let s = &self.streams[i];
            s.kind != Kind::Compressed && (contents || !s.content) && !s.apart && !past[i] && s.data.sunk_in().is_none()
        };

        // the streams longer than half the limit, of each class and kind, cut into pieces of a
        // share of it (of each stream, their indices) and grouped, each group of the first
        // piece of it (with the others of it, if any)
        let (mut cut, mut pieces, mut groups_of) = (vec![Vec::new(); n], Vec::<Piece>::new(), Vec::new());
        let mut members: Vec<Vec<usize>> = Vec::new();
        let mut long: Vec<(&str, Kind, Vec<usize>)> = Vec::new();
        for (i, s) in self.streams.iter().enumerate().filter(|&(i, _)| alike(i) && lens[i] > limit / 2) {
            match long.iter_mut().find(|l| l.0 == s.class && l.1 == s.kind) {
                Some(l) => l.2.push(i),
                None => long.push((&s.class, s.kind, vec![i])),
            }
        }
        for (_, _, ss) in long.iter().filter(|l| l.2.len() > 1) {
            let most = ss.len().min(GROUP_MOST);
            let size = limit / most as u64;
            if size < PIECE_MIN {
                continue;
            }
            let from = pieces.len();
            for &i in ss {
                for a in (0..lens[i]).step_by(size as usize) {
                    cut[i].push(pieces.len());
                    pieces.push((i, a, (a + size).min(lens[i])));
                }
            }
            let of: Vec<(usize, &[u8])> =
                pieces[from..].iter().map(|&(s, a, b)| (s, &self.streams[s].data[a as usize..b as usize])).collect();
            groups_of.extend(pieces::group(&of, most).into_iter().map(|g| from + g));
            members.resize(pieces.len(), Vec::new());
            (from..pieces.len()).for_each(|p| members[groups_of[p]].push(p));
        }

        let mut firsts: Vec<(Cow<'static, str>, Kind, usize)> = Vec::new();
        let (mut groups, mut glens): (Vec<Vec<Piece>>, Vec<u64>) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let mut placed = vec![false; pieces.len()];
        for (i, s) in self.streams.iter().enumerate() {
            // (a stream sunk is of the sink's stream, after the groups)
            if s.data.sunk_in().is_some() {
                continue;
            }
            if cut[i].iter().any(|&p| members[groups_of[p]].len() > 1) {
                // each piece with those of its group (of streams after: one before took them
                // already), and the rest one after another
                let mut rest: Option<usize> = None;
                for &p in &cut[i] {
                    if placed[p] {
                        continue;
                    }
                    let (_, a, b) = pieces[p];
                    match (&members[groups_of[p]][..], rest) {
                        (ps @ [_, _, ..], _) => {
                            ps.iter().for_each(|&q| placed[q] = true);
                            groups.push(ps.iter().map(|&q| pieces[q]).collect());
                            glens.push(ps.iter().map(|&q| pieces[q].2 - pieces[q].1).sum());
                        }
                        (_, Some(r)) => {
                            match groups[r].last_mut() {
                                Some(l) if l.2 == a => l.2 = b,
                                _ => groups[r].push(pieces[p]),
                            }
                            glens[r] += b - a;
                        }
                        (_, None) => {
                            rest = Some(groups.len());
                            groups.push(vec![pieces[p]]);
                            glens.push(b - a);
                        }
                    }
                }
                continue;
            }
            let whole = (i, 0, lens[i]);
            match firsts.iter_mut().find(|f| alike(i) && f.0 == s.class && f.1 == s.kind) {
                Some(&mut (_, _, j)) if glens[j] + lens[i] <= limit => {
                    groups[j].push(whole);
                    glens[j] += lens[i];
                }
                Some(first) => {
                    first.2 = groups.len();
                    groups.push(vec![whole]);
                    glens.push(lens[i]);
                }
                None => {
                    if alike(i) {
                        firsts.push((s.class.clone(), s.kind, groups.len()));
                    }
                    groups.push(vec![whole]);
                    glens.push(lens[i]);
                }
            }
        }
        let mut to = vec![Vec::new(); n];
        for (g, ps) in groups.iter().enumerate() {
            ps.iter()
                .scan(0, |at, &(s, a, b)| Some((s, a, std::mem::replace(at, *at + b - a))))
                .for_each(|(s, a, at)| to[s].push((a, g, at)));
        }
        to.iter_mut().for_each(|t| t.sort_unstable());
        for (t, s) in to.iter_mut().zip(&self.streams) {
            if let Some(k) = s.data.sunk_in() {
                *t = vec![(0, groups.len() + k, 0)];
            }
        }
        (to, groups)
    }

    /// Slices of each stream made slices of the merged streams its pieces went to, at `to`
    /// ([`Formula::merge_plan`]), a slice over several pieces their concatenation; of a stream
    /// past them, of the same past `new` merged streams.
    fn remap(&mut self, to: &[Vec<At>], new: usize) {
        let n = to.len();
        let mut map = |s: usize, mut a: u64, mut l: u64| {
            let Some(ps) = to.get(s) else { return Node::slice(s - n + new, a, l) };
            let mut k = ps.partition_point(|p| p.0 <= a) - 1;
            let mut out = Vec::new();
            loop {
                let (start, g, at) = ps[k];
                let m = l.min(ps.get(k + 1).map_or(u64::MAX, |p| p.0) - a);
                out.push(Node::slice(g, at + a - start, m));
                (a, l, k) = (a + m, l - m, k + 1);
                if l == 0 {
                    break;
                }
            }
            if out.len() == 1 { out.pop().unwrap() } else { Node::concat(out) }
        };
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.map_slices(&mut map));
        self.root.map_slices(&mut map);
    }

    /// Bytes of the output, as declared.
    pub fn len(&self) -> u64 {
        self.root.len()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// `data`, the contents of the entries of an archive, as streams by kind: `ends` gives the
    /// kind of each entry's contents and their end in `data`. Returns the node of their slices,
    /// in order: a slice for each entry of a [`Kind::Format`] or of at least `own` bytes, those
    /// of other kinds one after another being one. The contents of [`Kind::Unknown`] are stream
    /// `raw` (of this formula, empty), cut out of `data` in place; those of other kinds are
    /// streams added after the others, of class `{prefix}/{kind}` (`{prefix}/{name}` for a
    /// format).
    pub fn push_by_kind(
        &mut self,
        raw: usize,
        data: Vec<u8>,
        ends: &[(Kind, usize)],
        prefix: &str,
        own: usize,
    ) -> Node {
        let mut by = ByKind::new(own);
        let mut start = 0;
        for &(kind, end) in ends {
            by.add(kind, &data[start..end]);
            start = end;
        }
        drop(data);
        by.push(self, raw, prefix)
    }

    /// The modules and resources of the formula's blocks, each once: those of its applications
    /// ([`Formula::applies`]) in order, then those whose bytes it takes ([`Node::ModuleBytes`],
    /// [`Node::ResourceBytes`]).
    pub fn blocks(&self) -> (Vec<Module>, Vec<&Resource>) {
        let (mut modules, mut resources): (Vec<Module>, Vec<&Resource>) = (Vec::new(), Vec::new());
        let mut module = |m: Module| {
            if !modules.contains(&m) {
                modules.push(m);
            }
        };
        let mut resource = |r| {
            if !resources.contains(&r) {
                resources.push(r);
            }
        };
        for a in self.applies() {
            module(a.module);
            a.resources.iter().for_each(&mut resource);
        }
        for n in self.shared.iter().flat_map(|s| &s.apply.inputs).chain([&self.root]) {
            n.visit(&mut |n| match n {
                &Node::ModuleBytes(m, _) => module(m),
                Node::ResourceBytes(r) => resource(r),
                _ => {}
            });
        }
        (modules, resources)
    }

    /// The applications, those of the root parents first, then those of several outputs.
    pub fn applies(&self) -> Vec<&Apply> {
        let mut out = Vec::new();
        self.root.visit(&mut |n| {
            if let Node::Apply(a) = n {
                out.push(&**a);
            }
        });
        for i in self.shared.iter().flat_map(|s| &s.apply.inputs) {
            i.visit(&mut |n| {
                if let Node::Apply(a) = n {
                    out.push(&**a);
                }
            });
        }
        out.extend(self.shared.iter().map(|s| &s.apply));
        out
    }

    /// Checks the outputs of the applications of several outputs: each taken exactly once, of
    /// the size declared, and in the inputs of an application only from those before it.
    /// Also checks that each resource whose bytes it takes is [`Resource::stored`].
    pub fn check_outputs(&self) -> Result<()> {
        let mut taken: Vec<Vec<bool>> = self.shared.iter().map(|s| vec![false; s.sizes.len()]).collect();
        let mut check = |n: &Node, before: usize| -> Result<()> {
            let mut r = Ok(());
            n.visit(&mut |n| {
                if let &Node::Output { shared, index, len } = n
                    && r.is_ok()
                {
                    r = match self.shared.get(shared).and_then(|s| s.sizes.get(index)) {
                        _ if shared >= before => {
                            Err(Error::new(format!("output #{shared}.{index} taken before it is")))
                        }
                        Some(&size) if size == len && !taken[shared][index] => {
                            taken[shared][index] = true;
                            Ok(())
                        }
                        Some(&size) if size != len => {
                            Err(Error::new(format!("output #{shared}.{index} of {len} bytes, not {size}")))
                        }
                        Some(_) => Err(Error::new(format!("output #{shared}.{index} taken twice"))),
                        None => Err(Error::new(format!("no output #{shared}.{index}"))),
                    };
                }
            });
            r
        };
        for (i, s) in self.shared.iter().enumerate() {
            if s.sizes.is_empty() || s.apply.len != s.sizes.iter().sum::<u64>() {
                return Err(Error::new(format!("application #{i} of several outputs of bad sizes")));
            }
            s.apply.inputs.iter().try_for_each(|n| check(n, i))?;
        }
        check(&self.root, self.shared.len())?;
        let mut stored = Ok(());
        for n in self.shared.iter().flat_map(|s| &s.apply.inputs).chain([&self.root]) {
            n.visit(&mut |n| {
                if let Node::ResourceBytes(r) = n
                    && r.stored_data().is_none()
                {
                    stored = Err(Error::new("the bytes of a resource not stored"));
                }
            });
        }
        stored?;
        match taken.iter().position(|t| t.contains(&false)) {
            Some(i) => Err(Error::new(format!("an output of #{i} not taken"))),
            None => Ok(()),
        }
    }

    /// The output, with every module run by `m`. Fails on slices out of their stream, on
    /// modules whose output is not of the declared size, and on outputs not taken once
    /// ([`Formula::check_outputs`]).
    pub fn eval(&self, m: &(impl Modules + Sync)) -> Result<Vec<u8>> {
        aqr::eval::eval_formula(self, m, None, 1)
    }

    /// A formula of node `n` of this one (of no outputs of applications of several outputs) and
    /// the streams it slices, each the bytes `data` gives of it (by its index here).
    pub fn part(&self, n: &Node, data: &dyn Fn(usize) -> Bytes) -> Formula {
        let mut n = n.clone();
        let mut of: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut own: Vec<Stream> = Vec::new();
        n.slices_mut(&mut |s, _, _| {
            if let Some(st) = self.streams.get(*s) {
                *s = *of.entry(*s).or_insert_with(|| {
                    own.push(Stream { data: data(*s), ..st.clone_empty() });
                    own.len() - 1
                });
            }
        });
        Formula { streams: own, root: n, shared: Vec::new() }
    }

    /// [`Formula::eval`], the output given to `part` a piece at a time as it is pulled, rather than
    /// held whole (by the evaluator of formula blocks), the applications among what it
    /// concatenates made ahead on up to `threads` threads.
    pub fn eval_parts(
        &self,
        m: &(impl Modules + Sync),
        threads: usize,
        part: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        aqr::eval::eval_formula(self, m, Some(part), threads).map(drop)
    }

    /// Sets every application's memory limit ([`Apply::memory_limit`]) to what `limit` gives of
    /// it and its inputs, which are made (by `m`) only as `limit` reads them. Fails where it gives
    /// none, or an input asked for cannot be made.
    pub fn declare_memory<M: Modules + Sync>(
        &mut self,
        m: &M,
        limit: &dyn Fn(&Apply, &dyn Contents) -> Option<u64>,
    ) -> Result<()> {
        fn visit<'f>(n: &'f Node, out: &mut Vec<&'f Apply>) {
            match n {
                Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| visit(n, out)),
                Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| visit(n, out)),
                Node::Apply(a) => {
                    out.push(a);
                    a.inputs.iter().for_each(|n| visit(n, out));
                }
                Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        fn visit_mut(n: &mut Node, limits: &mut impl Iterator<Item = u64>) {
            match n {
                Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| visit_mut(n, limits)),
                Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| visit_mut(n, limits)),
                Node::Apply(a) => {
                    a.memory_limit = limits.next().expect("a limit for each application");
                    a.inputs.iter_mut().for_each(|n| visit_mut(n, limits));
                }
                Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        let limits = {
            let mut applies = Vec::new();
            for s in &self.shared {
                applies.push(&s.apply);
                s.apply.inputs.iter().for_each(|n| visit(n, &mut applies));
            }
            visit(&self.root, &mut applies);
            let limits = aqr::eval::limits_of(self, m, &applies, limit)?;
            (applies.iter().zip(limits))
                .map(|(a, l)| l.ok_or_else(|| Error::new(format!("no memory limit for {}", a.module))))
                .collect::<Result<Vec<u64>>>()?
        };
        let mut limits = limits.into_iter();
        for s in &mut self.shared {
            s.apply.memory_limit = limits.next().expect("a limit for each application");
            s.apply.inputs.iter_mut().for_each(|n| visit_mut(n, &mut limits));
        }
        visit_mut(&mut self.root, &mut limits);
        Ok(())
    }
}

/// Indented, a node a line: `slice #stream [start, +len) CLASS` (and the kind of the stream
/// unless [`Kind::Unknown`] and not [`Stream::content`], with `content` if it is), `concat LEN`, `pack LEN`, `apply MODULE LEN` with the lengths of its
/// resources and its parameters in hex, `output #SHARED.INDEX LEN`, `patch LEN` (its source then
/// its patch), `module bytes MODULE LEN`, `resource bytes LEN`: first each application of
/// several outputs, `shared #I MODULE SIZES...` and its inputs, then the root.
impl fmt::Display for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn node(fm: &Formula, n: &Node, depth: usize, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{:1$}", "", depth * 2)?;
            match n {
                &Node::Slice { stream, start, len } => match fm.streams.get(stream) {
                    Some(s) if s.kind != Kind::Unknown || s.content => {
                        let content = if s.content { ", content" } else { "" };
                        writeln!(f, "slice #{stream} [{start}, +{len}) {} ({}{content})", s.class, s.kind)
                    }
                    s => writeln!(f, "slice #{stream} [{start}, +{len}) {}", s.map_or("?", |s| &s.class)),
                },
                Node::Concat(ns) | Node::Pack(ns) => {
                    writeln!(f, "{} {}", if let Node::Pack(_) = n { "pack" } else { "concat" }, n.len())?;
                    ns.iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
                Node::Apply(a) => {
                    write!(f, "apply {} {}", a.module, a.len)?;
                    attrs(a, f)?;
                    a.inputs.iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
                &Node::Output { shared, index, len } => writeln!(f, "output #{shared}.{index} {len}"),
                Node::ModuleBytes(m, d) => writeln!(f, "module bytes {m} {}", d.len()),
                Node::ResourceBytes(_) => writeln!(f, "resource bytes {}", n.len()),
                Node::Patch(p) => {
                    writeln!(f, "patch {}", p.len)?;
                    [&p.source, &p.patch].into_iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
            }
        }
        // the resources' lengths and the parameters, then the end of the line
        fn attrs(a: &Apply, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if !a.resources.is_empty() {
                write!(f, " resources")?;
                a.resources.iter().try_for_each(|r| write!(f, " {}", r.len()))?;
            }
            if !a.params.is_empty() {
                write!(f, " params ")?;
                a.params.iter().try_for_each(|b| write!(f, "{b:02x}"))?;
            }
            writeln!(f)
        }
        for (i, s) in self.shared.iter().enumerate() {
            write!(f, "shared #{i} {}{}", s.apply.module, if s.split { " split" } else { "" })?;
            s.sizes.iter().try_for_each(|n| write!(f, " {n}"))?;
            attrs(&s.apply, f)?;
            s.apply.inputs.iter().try_for_each(|n| node(self, n, 1, f))?;
        }
        node(self, &self.root, 0, f)
    }
}

/// Runs modules for [`Formula::eval`].
pub trait Modules {
    /// The output of `a` on `inputs`, the evaluated `a.inputs` (with `a.resources`).
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>>;

    /// The outputs of `s` on `inputs`, the evaluated `s.apply.inputs`: by default those split
    /// from the output of [`Modules::apply`] ([`Shared::split`]), or that output if `s` has one.
    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        match s.sizes[..] {
            _ if s.split => {
                let all = self.apply(&s.apply, inputs)?;
                if all.len() as u64 != s.apply.len {
                    return Err(Error::new(format!(
                        "{} made {} bytes, not {}",
                        s.apply.module,
                        all.len(),
                        s.apply.len
                    )));
                }
                let mut rest = &all[..];
                Ok(s.sizes.iter().map(|&n| rest.split_off(..n as usize).unwrap().to_vec()).collect())
            }
            [_] => Ok(vec![self.apply(&s.apply, inputs)?]),
            _ => Err(Error::new(format!("{}: {} outputs", s.apply.module, s.sizes.len()))),
        }
    }
}

/// A function as the modules, e.g. a `match` on `a.module` in tests.
impl<F: Fn(&Apply, &[&[u8]]) -> Result<Vec<u8>>> Modules for F {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        self(a, inputs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    const REV: Module = Module::new("rev", 1);

    /// `rev` reverses the concatenation of its inputs.
    struct Rev;

    impl Modules for Rev {
        fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
            assert_eq!(a.module, REV);
            Ok(inputs.concat().into_iter().rev().collect())
        }
    }

    #[test]
    fn concat_flattens() {
        let n = Node::concat([
            Node::slice(0, 0, 2),
            Node::concat([Node::slice(0, 2, 3), Node::slice(1, 0, 0)]),
            Node::slice(0, 6, 1),
        ]);
        assert_eq!(n, Node::Concat(vec![Node::slice(0, 0, 5), Node::slice(0, 6, 1)]));
        assert_eq!(Node::concat([Node::slice(0, 0, 1), Node::slice(0, 1, 1)]), Node::slice(0, 0, 2));
    }

    #[test]
    fn cut_by_sizes() {
        let n = Node::Concat(vec![Node::slice(0, 0, 5), Node::slice(1, 10, 3)]);
        assert_eq!(
            n.cut(&[2, 0, 4, 2]),
            [
                Node::slice(0, 0, 2),
                Node::Concat(Vec::new()),
                Node::Concat(vec![Node::slice(0, 2, 3), Node::slice(1, 10, 1)]),
                Node::slice(1, 11, 2),
            ]
        );
        assert_eq!(Node::slice(0, 4, 6).cut(&[6]), [Node::slice(0, 4, 6)]);
    }

    #[test]
    fn eval_parts() {
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        f.root = Node::Concat(vec![
            Node::apply(REV, vec![], vec![a.clone()], 5),
            Node::Concat(vec![Node::slice(1, 0, 2), Node::apply(REV, vec![], vec![b], 3)]),
            a,
        ]);
        let mut parts = Vec::new();
        f.eval_parts(&Rev, 1, &mut |p| {
            parts.push(p.to_vec());
            Ok(())
        })
        .unwrap();
        // (a part each, what the root concatenates and what that does in turn)
        assert_eq!(parts, [&b"olleh"[..], b"xy", b"zyx", b"hello"]);
        assert_eq!(parts.concat(), f.eval(&Rev).unwrap());
        // (a failure of the part's taker stops it)
        let mut n = 0;
        let r = f.eval_parts(&Rev, 1, &mut |_| {
            n += 1;
            Err(Error::new("enough"))
        });
        assert!(r.is_err() && n == 1);
    }

    #[test]
    fn eval_embedded() {
        let child = Formula::raw("b", b"xyz".to_vec());
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.embed(child);
        assert_eq!(b, Node::slice(1, 0, 3));
        f.root = Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![], vec![a, b], 8)]);
        assert_eq!(f.len(), 10);
        assert_eq!(f.eval(&Rev).unwrap(), b"hezyxolleh");
        assert_eq!(f.applies().len(), 1);
        let shown = f.to_string();
        assert_eq!(
            shown,
            "concat 10\n  slice #0 [0, +2) a\n  apply rev@1 8\n    slice #0 [0, +5) a\n    slice #1 [0, +3) b\n"
        );
        f.streams[1].kind = Kind::Format("zlib");
        assert!(f.to_string().ends_with("slice #1 [0, +3) b (format=zlib)\n"), "{f}");
        f.streams[1].content = true;
        assert!(f.to_string().ends_with("slice #1 [0, +3) b (format=zlib, content)\n"), "{f}");
    }

    #[test]
    fn eval_pack() {
        let mut f = Formula::raw("a", b"hello".to_vec());
        f.root = Node::pack([
            Node::slice(0, 1, 2),
            Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 3),
            Node::slice(0, 0, 0),
        ]);
        assert_eq!(f.len(), 32 + 5);
        let out = f.eval(&Rev).unwrap();
        assert_eq!(packed::unpack(&out).unwrap(), [&b"el"[..], b"leh", b""]);
        assert_eq!(
            f.to_string(),
            "pack 37\n  slice #0 [1, +2) a\n  apply rev@1 3\n    slice #0 [0, +3) a\n  slice #0 [0, +0) a\n"
        );
    }

    /// Contents of a class are merged as the formula is archived ([`Formula::merged`]), as
    /// nothing recompresses them any more, but not as recompressors merge classes.
    #[test]
    fn contents_merged_as_archived() {
        let mut f = Formula::default();
        let a = f.push_content("raw", Kind::Unknown, b"one".to_vec());
        let b = f.push_content("raw", Kind::Unknown, b"two".to_vec());
        f.root = Node::concat([a, b]);
        let (m, groups) = f.merged();
        assert_eq!(groups, [vec![(0, 0, 3), (1, 0, 3)]]);
        assert!(m.streams.len() == 1 && m.streams[0].content);
        let mut g = f.clone();
        g.merge_classes();
        assert_eq!(g.streams.len(), 2);
    }

    #[test]
    fn compact() {
        let mut f = Formula::default();
        f.push("a", b"0123456789".to_vec());
        f.push("unused", b"xyz".to_vec());
        f.push("b", b"abcdef".to_vec());
        f.root = Node::concat([
            Node::slice(0, 6, 2),
            Node::apply(REV, vec![], vec![Node::slice(0, 1, 3), Node::slice(0, 2, 3), Node::slice(0, 9, 0)], 4),
            Node::slice(2, 4, 3),
            Node::slice(5, 0, 0),
        ]);
        let mut c = f.clone();
        c.compact();
        // (failing alike: the streams it names are numbered anew)
        assert_eq!(c.eval(&Rev).ok(), f.eval(&Rev).ok());
        // shared bytes: only those kept copied, the same as of its own
        let mut s = f.clone();
        s.streams[0].data = Bytes::shared(Arc::new(b"0123456789".to_vec()));
        s.compact();
        assert!(!s.streams[0].data.is_shared());
        assert_eq!(s, c);
        // The slice past the end of "b" keeps it whole; the slice of no stream stays so.
        assert_eq!(
            c.streams.iter().map(|s| (&*s.class, &s.data[..])).collect::<Vec<_>>(),
            [("a", &b"123467"[..]), ("b", &b"abcdef"[..])]
        );
        assert_eq!(
            c.root,
            Node::concat([
                Node::slice(0, 4, 2),
                Node::apply(REV, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 1, 3), Node::slice(0, 6, 0)], 4),
                Node::slice(1, 4, 3),
                Node::slice(4, 0, 0),
            ])
        );
        f.root = Node::slice(2, 1, 2);
        f.compact();
        assert_eq!(f.eval(&Rev).unwrap(), b"bc");
        assert_eq!(f.streams.len(), 1);
        assert_eq!(f.streams[0].data, b"bc");
        // a stream sliced whole is not copied
        let mut f = Formula::raw("a", b"abc".to_vec());
        let whole = f.streams[0].data.as_ptr();
        f.compact();
        assert_eq!(f.streams[0].data.as_ptr(), whole);
    }

    const CUT: Module = Module::new("cut", 1);

    /// `rev`, and `cut`, cutting the concatenation of its inputs into its outputs.
    struct RevCut;

    impl Modules for RevCut {
        fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
            Rev.apply(a, inputs)
        }

        fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
            assert_eq!(s.apply.module, CUT);
            let (all, mut at) = (inputs.concat(), 0);
            let mut next = |n: u64| {
                at += n as usize;
                all[at - n as usize..at].to_vec()
            };
            Ok(s.sizes.iter().map(|&n| next(n)).collect())
        }
    }

    fn cut(inputs: Vec<Node>) -> Apply {
        Apply { module: CUT, resources: vec![], params: vec![], inputs, len: 0, memory_limit: crate::NO_LIMIT }
    }

    /// Outputs of applications of several outputs, taken by the root and by later ones; shown,
    /// embedded and compacted.
    #[test]
    fn eval_shared() {
        let mut f = Formula::raw("a", b"abcdefgh".to_vec());
        let a = f.root.clone();
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
        let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1]), vec![1, 4])).unwrap();
        f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![], vec![p0], 1)]);
        assert_eq!(f.len(), 9);
        assert_eq!(f.eval(&RevCut).unwrap(), b"efghhabcd");
        assert_eq!(f.applies().len(), 3);
        assert_eq!(
            f.to_string(),
            "shared #0 cut@1 3 5\n  slice #0 [0, +8) a\nshared #1 cut@1 1 4\n  output #0.1 5\n\
             concat 9\n  output #1.1 4\n  slice #0 [7, +1) a\n  output #0.0 3\n  apply rev@1 1\n    output #1.0 1\n"
        );
        // embedded after a formula of its own application of several outputs
        let mut g = Formula::raw("b", b"xy".to_vec());
        let b = g.root.clone();
        let [x, y] = <[Node; 2]>::try_from(g.share(cut(vec![b]), vec![1, 1])).unwrap();
        let child = g.embed(f.clone());
        g.root = Node::concat([y, child, x]);
        assert_eq!(g.eval(&RevCut).unwrap(), b"yefghhabcdx");
        // compacted: the slices of the applications are kept
        let mut c = f.clone();
        c.streams[0].data.extend_from_slice(b"unused");
        c.compact();
        assert_eq!(c.eval(&RevCut).unwrap(), b"efghhabcd");
        assert_eq!(c.streams[0].data, b"abcdefgh");

        // split from the one output of a module
        let mut s = Formula::raw("a", b"abcdefgh".to_vec());
        let rev = Apply {
            module: REV,
            resources: vec![],
            params: vec![],
            inputs: vec![s.root.clone()],
            len: 0,
            memory_limit: crate::NO_LIMIT,
        };
        let [x, y] = <[Node; 2]>::try_from(s.split(rev, vec![5, 3])).unwrap();
        s.root = Node::concat([y, x]);
        assert_eq!(s.eval(&Rev).unwrap(), b"cbahgfed");
    }

    /// A patch of a slice against another one, shown.
    #[test]
    fn eval_patch() {
        let (source, target) = (b"the quick brown fox jumps".to_vec(), b"the quick red fox jumps!".to_vec());
        let mut f = Formula::raw("a", source.clone());
        let d = f.push_as("d", Kind::Compressed, patch::make(&source, &target, 3).unwrap());
        let n = target.len() as u64;
        f.root = Node::concat([Node::slice(0, 0, 4), Node::patch(Node::slice(0, 0, 25), d, n)]);
        assert_eq!(f.eval(&Rev).unwrap(), [&b"the "[..], &target].concat());
        assert!(
            f.to_string().starts_with("concat 28\n  slice #0 [0, +4) a\n  patch 24\n    slice #0 [0, +25) a\n"),
            "{f}"
        );
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        let Node::Patch(p) = &mut ns[1] else { unreachable!() };
        p.len += 1;
        assert!(f.eval(&Rev).is_err());
    }

    /// Streams of a class and kind merged, but those compressed, the contents, those apart and
    /// those a slice goes past; the output the same.
    #[test]
    fn merge_classes() {
        let mut f = Formula::default();
        let a0 = f.push("a", b"012".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        let a1 = f.push("a", b"345".to_vec());
        let c = f.push_as("a", Kind::Compressed, b"ccc".to_vec());
        let t = f.push_content("a", Kind::Unknown, b"ttt".to_vec());
        f.push("a", b"678".to_vec());
        let a3 = f.push("a", b"9".to_vec());
        let p = f.push_apart("a", b"ppp".to_vec());
        f.root = Node::concat([
            a1,
            Node::apply(REV, vec![], vec![a0, b, Node::slice(5, 1, 2)], 8),
            c,
            t,
            a3,
            Node::slice(5, 0, 1),
            p,
        ]);
        let want = f.eval(&Rev).unwrap();
        let mut g = f.clone();
        g.merge_classes();
        assert_eq!(g.eval(&Rev), Ok(want));
        let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
        assert_eq!(
            classes,
            [("a", &b"0123456789"[..]), ("b", &b"xyz"[..]), ("a", &b"ccc"[..]), ("a", &b"ttt"[..]), ("a", &b"ppp"[..])]
        );
        // within 6 bytes a stream
        let mut g = f.clone();
        g.merge_classes_within(6);
        assert_eq!(g.eval(&Rev), f.eval(&Rev));
        let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
        assert_eq!(
            classes,
            [
                ("a", &b"012345"[..]),
                ("b", &b"xyz"[..]),
                ("a", &b"ccc"[..]),
                ("a", &b"ttt"[..]),
                ("a", &b"6789"[..]),
                ("a", &b"ppp"[..])
            ]
        );
        // a slice past the end of the last keeps it apart
        let mut past = f.clone();
        let Node::Concat(ns) = &mut past.root else { unreachable!() };
        ns.push(Node::slice(6, 0, 3));
        past.merge_classes();
        assert_eq!(past.streams.len(), 6);
        assert_eq!((&past.streams[0].data[..], &past.streams[4].data[..]), (&b"012345678"[..], &b"9"[..]));
    }

    /// Streams longer than half the limit cut into pieces, those of different streams most alike
    /// put together, the rest of each stream one; the output the same.
    #[test]
    fn merge_pieces() {
        const P: usize = PIECE_MIN as usize;
        let noise = |n: usize, seed: u64| -> Vec<u8> {
            let mut x = seed;
            (0..n)
                .map(|_| (x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407), (x >> 56) as u8).1)
                .collect()
        };
        let a = noise(3 * P, 1);
        // a's first two pieces swapped, then others
        let b = [&a[P..2 * P], &a[..P], &noise(P / 2, 2)].concat();
        let mut f = Formula::default();
        let na = f.push("c", a.clone());
        let nb = f.push("c", b.clone());
        let s = f.push("c", b"small".to_vec());
        let o = f.push("d", b"other".to_vec());
        f.root = Node::concat([
            Node::apply(REV, vec![], vec![Node::slice(0, P as u64 - 10, P as u64 + 20)], P as u64 + 20),
            nb,
            s,
            o,
            Node::slice(1, 10, 2 * P as u64),
            na,
        ]);
        let want = f.eval(&Rev).unwrap();
        let mut g = f.clone();
        g.merge_classes_within(2 * PIECE_MIN);
        assert_eq!(g.eval(&Rev), Ok(want));
        let streams: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
        let (a, b) = (&a[..], &b[..]);
        let (ab, ba) = ([&a[..P], &b[P..2 * P]].concat(), [&a[P..2 * P], &b[..P]].concat());
        assert_eq!(
            streams,
            [("c", &ab[..]), ("c", &ba[..]), ("c", &a[2 * P..]), ("c", &b[2 * P..]), ("c", b"small"), ("d", b"other")]
        );
        // not within a limit of less than twice the least piece
        let mut g = f.clone();
        g.merge_classes_within(2 * PIECE_MIN - 1);
        assert_eq!(g.streams.len(), 4);
    }

    /// Slices cut into the shorter ones they start with, wherever those lie, but those shorter
    /// than the least, and those past the end of their stream; the output the same.
    #[test]
    fn share_slices() {
        let long: Vec<u8> = (0..40u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut f = Formula::default();
        let a = f.push("a", long[..100].to_vec());
        let b = f.push_content("b", Kind::Unknown, long.clone());
        let c = f.push("c", long[..100].to_vec());
        let d = f.push("d", long[..8].to_vec());
        f.push("e", long[..120].to_vec());
        let mut other = long[..100].to_vec();
        other[99] ^= 1;
        let o = f.push("o", other);
        // long twice within, after other bytes
        f.push("z", [&b"xyz"[..], &long[..], &long[..]].concat());
        // long's start, but less than half of what a keeps after the 20 bytes of c
        let mut q = long[..100].to_vec();
        q[50..].iter_mut().for_each(|x| *x ^= 0x55);
        let q = f.push("q", q);
        f.root = Node::concat([
            a,
            Node::apply(REV, vec![], vec![Node::slice(2, 0, 20), b], 180),
            c,
            d,
            o,
            Node::slice(4, 100, 20),
            Node::slice(6, 3, 120),
            Node::slice(6, 163, 160),
            Node::slice(6, 0, 4),
            q,
        ]);
        let want = f.eval(&Rev).unwrap();
        let mut g = f.clone();
        g.share_slices(16);
        assert_eq!(g.eval(&Rev), Ok(want));
        let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, s.data.len())).collect();
        // long[..20] of c, long[20..100] of a, long[100..120] of e, long[120..] of b (each
        // of those alone first); o its own but for its first 99 (of c, and the start of a's);
        // q but for the first 20
        assert_eq!(classes, [("a", 80), ("b", 40), ("c", 20), ("d", 8), ("e", 20), ("o", 1), ("z", 4), ("q", 80)]);
        // a slice past the end of one is left as it is, and keeps it whole
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        ns.push(Node::slice(2, 90, 20));
        f.share_slices(16);
        let classes: Vec<_> = f.streams.iter().map(|s| (&*s.class, s.data.len())).collect();
        assert_eq!(classes, [("a", 80), ("b", 40), ("c", 100), ("d", 8), ("e", 20), ("o", 1), ("z", 4), ("q", 80)]);
        assert!(matches!(&f.root, Node::Concat(ns) if ns.last() == Some(&Node::slice(2, 90, 20))));
    }

    /// A slice cut within a concatenation below the root is spliced into it, not nested; the
    /// root's parts stay its parts, each cut where it is.
    #[test]
    fn share_slices_flat() {
        let long: Vec<u8> = (0..40u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut f = Formula::default();
        let a = f.push("a", long[..32].to_vec());
        let b = f.push("b", long.clone());
        let c = f.push("c", b"x".repeat(20));
        f.root = Node::Concat(vec![Node::Concat(vec![c, b.clone()]), b.clone(), a]);
        let want = f.eval(&Rev).unwrap();
        f.share_slices(16);
        assert_eq!(f.eval(&Rev), Ok(want));
        let Node::Concat(parts) = &f.root else { panic!("{f}") };
        assert_eq!(parts.len(), 3, "{f}");
        let nested = |n: &Node| matches!(n, Node::Concat(ns) if ns.iter().any(|n| matches!(n, Node::Concat(_))));
        assert!(!parts.iter().any(nested), "{f}");
        assert!(matches!(&parts[0], Node::Concat(ns) if ns.len() == 3), "{f}");
        assert!(matches!(&parts[1], Node::Concat(ns) if ns.len() == 2), "{f}");
    }

    /// A slice whose bytes repeat a piece over and over (padding, zeros) is cut at its first
    /// copy only: the rest of the run is kept of its own bytes, which compress to little, not
    /// cut into a slice of each copy.
    #[test]
    fn share_slices_runs() {
        let mut f = Formula::default();
        let z = f.push("z", vec![0; 16]);
        let run = f.push("run", [&[0; 200][..], b"some tail of its own"].concat());
        f.root = Node::Concat(vec![z, run]);
        let want = f.eval(&Rev).unwrap();
        f.share_slices(16);
        assert_eq!(f.eval(&Rev), Ok(want));
        let Node::Concat(parts) = &f.root else { panic!("{f}") };
        // the zeros' first 16 bytes of z, the rest of run's own
        assert_eq!(parts[1], Node::concat([Node::slice(0, 0, 16), Node::slice(1, 0, 204)]), "{f}");
        assert_eq!(f.streams[1].data.len(), 204);
    }

    /// Bytes shared are cloned without a copy, and copied once changed.
    #[test]
    fn shared_bytes() {
        let mut b = Bytes::from(vec![1, 2, 3]);
        let at = b.as_ptr();
        b.share();
        let mut c = b.clone();
        assert!(b.is_shared() && c.as_ptr() == at && *c == [1, 2, 3]);
        c[0] = 4;
        assert!(!c.is_shared() && *b == [1, 2, 3] && *c == [4, 2, 3]);
    }

    /// Outputs not taken once each, of other sizes, or taken by an application before theirs.
    #[test]
    fn shared_errors() {
        let mut f = Formula::raw("a", b"abcdefgh".to_vec());
        let a = f.root.clone();
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
        for root in [
            Node::concat([o0.clone()]),
            Node::concat([o0.clone(), o1.clone(), o0.clone()]),
            Node::concat([o0.clone(), Node::Output { shared: 0, index: 1, len: 4 }]),
            Node::concat([o0.clone(), o1.clone(), Node::Output { shared: 0, index: 2, len: 0 }]),
        ] {
            f.root = root;
            assert!(f.check_outputs().is_err() && f.eval(&RevCut).is_err(), "{f}");
        }
        f.root = Node::concat([o0, o1]);
        assert_eq!(f.eval(&RevCut).unwrap(), b"abcdefgh");
        f.shared[0].apply.inputs = vec![Node::Output { shared: 0, index: 0, len: 3 }];
        assert!(f.check_outputs().is_err());
        // a module of one output only, by default
        let mut g = Formula::raw("a", b"ab".to_vec());
        let a = g.root.clone();
        g.root = Node::concat(g.share(Apply { module: REV, ..cut(vec![a]) }, vec![1, 1]));
        assert!(g.eval(&Rev).is_err());
    }

    /// Applications of several outputs reordered: their outputs are taken where they were.
    #[test]
    fn shared_reordered() {
        let mut f = Formula::raw("a", b"abcdefgh".to_vec());
        let a = f.root.clone();
        let first = f.share(cut(vec![a]), vec![3, 5]);
        let second = f.share(cut(first), vec![4, 4]);
        f.root = Node::concat(second.into_iter().rev());
        let want = f.eval(&RevCut).unwrap();
        f.reorder_shared(&[1, 0]);
        assert!(f.check_outputs().is_err());
        f.reorder_shared(&[1, 0]);
        f.check_outputs().unwrap();
        assert_eq!(f.eval(&RevCut).unwrap(), want);
        let mut g = f.clone();
        g.reorder_shared(&[0, 1]);
        assert_eq!(g, f);
    }

    #[test]
    fn eval_errors() {
        let mut f = Formula::raw("a", b"abc".to_vec());
        f.root = Node::slice(0, 1, 3);
        assert!(f.eval(&Rev).is_err());
        f.root = Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 2);
        assert!(f.eval(&Rev).is_err());
    }
}
