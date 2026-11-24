//! Hints given to files by their attributes (docs/hints-design.md §3, [`Hints`]): the format
//! a file is of (`aqr-format`), its kind (`aqr-kind`), the formats not to look for in it
//! (`aqr-not`), the parameters of its format (`aqr-PARAM`), and from `.gitattributes` alone
//! whether it is text (`text`, `binary`, `eol`, `filter=lfs`), weaker than its bytes.
//!
//! A file of hints is of the kind [`Kind::Hint`], an index of a table of them kept for the
//! process ([`get`]), so that the hint goes with the file's stream as its kind does, from a
//! container's recompressor to its entries too ([`Hints::member`]); its data is made by
//! [`made`] ([`Nest::Hint`]), and its streams are of the kind it would have been of before the
//! archive is encoded ([`settle`]). Hints steer the encoder alone: a format told that turns the
//! file down leaves it to detection, as it would be without the hint, and is told of
//! ([`Hints::report`]).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use aqr_formula::{Formula, Kind, Kinds};

use crate::attributes::{Attributes, Origin, Resolved, State};
use crate::{Cx, Format, Made, Nest, Options, detect, kind_by_name};

/// The attributes given for an archive ([`Attributes`]), and what came of the hints they gave
/// ([`Hints::report`]).
pub struct Hints {
    attrs: Attributes,
    report: Mutex<Report>,
}

#[derive(Default)]
struct Report {
    warnings: BTreeSet<String>,
    /// The paths whose format told turned them down, with the format, and whether it made a
    /// formula that was no smaller than the data as it is (zstd's).
    turned_down: BTreeSet<(String, &'static str, bool)>,
    /// The paths whose format told made a formula about as large as the data, kept beside it for
    /// the batch to choose ([`Made::close`]), with the format.
    close: BTreeSet<(String, &'static str)>,
}

/// The format a file is told it is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Told {
    /// None told: as its kind tells.
    No,
    /// None: not recompressed (`-aqr-format`).
    Off,
    /// This one, tried first (`aqr-format=NAME`).
    As(Format),
}

/// What a file is told ([`Hints`]).
pub(crate) struct Hint {
    pub(crate) hints: Arc<Hints>,
    /// Its path: in the archive, then in each container holding it ([`Attributes::resolve`]).
    pub(crate) chain: Vec<Vec<u8>>,
    /// Its kind by its name ([`kind_by_name`]).
    pub(crate) by_name: Kind,
    pub(crate) format: Told,
    /// Its kind told (`aqr-kind`).
    pub(crate) kind: Option<Kind>,
    /// The formats not looked for in it (`aqr-not`).
    pub(crate) not: Vec<Format>,
    /// The parameters of its format told (`aqr-PARAM=VALUE`, by `PARAM`), and the directory
    /// that values naming files are relative to.
    pub(crate) params: Vec<(String, String)>,
    pub(crate) dir: Option<PathBuf>,
    /// Whether it is text, as `.gitattributes` tells.
    pub(crate) text: Option<bool>,
    /// Whether the attributes may tell of what it holds ([`Resolved::descends`]).
    pub(crate) descends: bool,
}

/// The names of the attributes of hints that are not parameters.
const OWN: &[&str] = &["aqr-format", "aqr-kind", "aqr-not"];

impl Hints {
    pub fn new(attrs: Attributes) -> Arc<Hints> {
        Arc::new(Hints { attrs, report: Default::default() })
    }

    pub fn attributes(&self) -> &Attributes {
        &self.attrs
    }

    /// The kind of a file of the tree at `path` (from the root of the archive, of `/`
    /// separators), of kind `by_name` by its name: [`Kind::Hint`] where its attributes tell
    /// anything.
    pub fn kind(self: &Arc<Self>, path: &[u8], by_name: Kind) -> Kind {
        self.kind_of(vec![path.to_vec()], by_name)
    }

    fn kind_of(self: &Arc<Self>, chain: Vec<Vec<u8>>, by_name: Kind) -> Kind {
        let refs: Vec<&[u8]> = chain.iter().map(|c| &c[..]).collect();
        let r = self.attrs.resolve(&refs);
        if r.is_empty() {
            return by_name;
        }
        let h = self.hint(chain, by_name, &r);
        match h.plain() {
            true => by_name,
            false => Kind::Hint(intern(h)),
        }
    }

    /// The kind of member `name` (a path from the root of the container, empty for the one
    /// content of what holds no names) of the data of hint `parent`, of kind `by_name` without
    /// hints.
    pub(crate) fn member(parent: &Hint, name: &[u8], by_name: Kind) -> Kind {
        let mut chain = parent.chain.clone();
        chain.push(name.to_vec());
        parent.hints.kind_of(chain, by_name)
    }

    /// The hint of the file at `chain` of kind `by_name`, of what `r` tells.
    fn hint(self: &Arc<Self>, chain: Vec<Vec<u8>>, by_name: Kind, r: &Resolved) -> Hint {
        let mut h = Hint {
            hints: self.clone(),
            chain,
            by_name,
            format: Told::No,
            kind: None,
            not: Vec::new(),
            params: Vec::new(),
            dir: None,
            text: None,
            descends: r.descends,
        };
        let path = h_path(&h.chain);
        let warn = |w: String| self.warn(format!("{path}: {w}"));
        // (by name: `text` after `eol` and `filter`, so that it wins)
        for (name, state, origin, file) in &r.attrs {
            let git = *origin == Origin::Git;
            match (name.as_str(), state) {
                ("aqr-format", State::Unset) => h.format = Told::Off,
                ("aqr-format", State::Value(v)) => match Format::from_name(v) {
                    Some(f) => {
                        h.format = Told::As(f);
                        h.dir = self.attrs.data_dir(*file).map(Path::to_owned);
                    }
                    None => warn(format!("aqr-format={v}: no such format")),
                },
                ("aqr-kind", State::Value(v)) => match kind_named(v) {
                    Some(k) => h.kind = Some(k),
                    None => warn(format!("aqr-kind={v}: no such kind")),
                },
                ("aqr-not", State::Value(v)) => {
                    for n in v.split(',').filter(|n| !n.is_empty()) {
                        match Format::from_name(n) {
                            Some(f) => h.not.push(f),
                            None => warn(format!("aqr-not={n}: no such format")),
                        }
                    }
                }
                (n, State::Value(v)) if n.starts_with("aqr-") && !OWN.contains(&n) => {
                    h.params.push((n["aqr-".len()..].to_owned(), v.clone()));
                }
                (n, _) if n.starts_with("aqr-") => warn(format!("{n}: takes a value")),
                // (git's own, from .gitattributes alone, weaker than the bytes)
                ("text", State::Set) if git => h.text = Some(true),
                ("text", State::Unset) if git => h.text = Some(false),
                ("eol", State::Value(_)) if git => h.text = Some(true),
                ("filter", State::Value(v)) if git && v == "lfs" => h.text = Some(true),
                _ => {}
            }
        }
        // (`raw` of a layout told: the samples of no header)
        if h.format == Told::As(Format::Raw)
            && ["bits", "channels", "endian", "sign"].iter().any(|p| h.param(p).is_some())
        {
            h.format = Told::As(Format::Pcm);
        }
        h
    }

    pub(crate) fn warn(&self, w: String) {
        self.report.lock().unwrap().warnings.insert(w);
    }

    /// What came of the hints so far, for `--stats` (each line a thing to tell): the attributes
    /// that were not understood, the files whose format told turned them down, the lines that
    /// matched no path.
    pub fn report(&self) -> Vec<String> {
        let r = self.report.lock().unwrap();
        let mut out: Vec<String> = r.warnings.iter().cloned().collect();
        out.extend(r.turned_down.iter().map(|(p, f, lost)| match lost {
            false => format!("{p}: aqr-format={f} turned it down"),
            true => format!("{p}: aqr-format={f} made it no smaller than it is"),
        }));
        out.extend(r.close.iter().map(|(p, f)| format!("{p}: aqr-format={f} made it about as small as it is")));
        out.extend(self.attrs.unmatched().into_iter().map(|l| format!("{l}: matched no file")));
        out
    }
}

/// `chain` as the user would write it: its parts joined by `>`.
fn h_path(chain: &[Vec<u8>]) -> String {
    let parts: Vec<String> = chain.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    parts.join(">")
}

/// The kind of [`Kind`]'s `Display` name `name`, but formats (as `aqr-format` tells them).
fn kind_named(name: &str) -> Option<Kind> {
    [Kind::Unknown, Kind::Pixels, Kind::Text, Kind::Font, Kind::Table, Kind::Compressed]
        .into_iter()
        .find(|k| k.to_string() == name)
}

impl Hint {
    /// Whether it tells nothing that its kind would not ([`Hints::kind`]).
    fn plain(&self) -> bool {
        self.format == Told::No && self.kind.is_none() && self.not.is_empty() && self.text.is_none() && !self.descends
    }

    /// The kind it is of without its format: told, else as its name tells.
    pub(crate) fn kind(&self) -> Kind {
        match (self.kind, self.text) {
            (Some(k), _) => k,
            (None, Some(true)) if self.by_name == Kind::Unknown => Kind::Text,
            (None, _) => self.by_name,
        }
    }

    /// The path, as the user would write it.
    pub(crate) fn path(&self) -> String {
        h_path(&self.chain)
    }

    /// The value of parameter `name` (`aqr-NAME`) of its format.
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|p| p.0 == name).map(|p| p.1.as_str())
    }

    /// Notes that its value of parameter `name` is not one its format takes: the hint is left
    /// (the file to detection).
    pub(crate) fn bad_param(&self, name: &str, why: &str) {
        let v = self.param(name).unwrap_or("");
        self.hints.warn(format!("{}: aqr-{name}={v}: {why}", self.path()));
    }

    /// The file `value` names, relative to the directory of the attributes that told its format.
    pub(crate) fn file(&self, value: &str) -> PathBuf {
        match &self.dir {
            Some(d) => d.join(value),
            None => PathBuf::from(value),
        }
    }
}

/// The hints of the process, by index ([`Kind::Hint`]); each is kept, as is what it was of.
#[derive(Default)]
struct Table {
    hints: Vec<Arc<Hint>>,
    /// By the address of its [`Hints`] (kept alive by the hint, so not taken again) and chain.
    by: HashMap<(usize, Vec<Vec<u8>>), u32>,
}

static TABLE: LazyLock<RwLock<Table>> = LazyLock::new(Default::default);

fn intern(h: Hint) -> u32 {
    let key = (Arc::as_ptr(&h.hints) as usize, h.chain.clone());
    if let Some(&i) = TABLE.read().unwrap().by.get(&key) {
        return i;
    }
    let mut t = TABLE.write().unwrap();
    if let Some(&i) = t.by.get(&key) {
        return i;
    }
    let i = u32::try_from(t.hints.len()).expect("fewer hints than 2^32");
    t.hints.push(Arc::new(h));
    t.by.insert(key, i);
    i
}

/// Hint `i` ([`Kind::Hint`]).
pub(crate) fn get(i: u32) -> Arc<Hint> {
    TABLE.read().unwrap().hints[i as usize].clone()
}

/// How data of [`Kind::Hint`] `i` is recompressed where it would be as `outer` ([`Nest::of`]).
pub(crate) fn nest(i: u32, outer: Nest) -> Option<Nest> {
    let h = get(i);
    match h.format {
        Told::Off => None,
        Told::As(_) => Some(Nest::Hint(i)),
        Told::No => {
            let base = outer.of(h.kind())?;
            match h.not.is_empty() && h.text.is_none() && !h.descends {
                true => Some(base),
                false => Some(Nest::Hint(i)),
            }
        }
    }
}

/// Data of [`Kind::Hint`] `i`, made: as its format told where that takes it, else as it would be
/// without it, the formats it is not to be looked for as not there. Its hint is the one the
/// recompressor of its format, and of the containers it is, see ([`within`]).
pub(crate) fn made(i: u32, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    let h = get(i);
    let outer = cx.within.replace(Some((d.as_ptr() as usize, i)));
    let m = made_hinted(&h, d, opt, depth, cx);
    cx.within.set(outer);
    m
}

fn made_hinted(h: &Hint, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    if let Told::As(format) = h.format {
        // (`aqr-offset`: the file of the format from there on, the bytes before it data)
        let at = match h.param("offset").map(offset) {
            None => Some(0),
            Some(Some(o)) if o < d.len() => Some(o),
            Some(_) => {
                h.bad_param("offset", "not an offset within the file");
                None
            }
        };
        let outer = cx.lost.replace(false);
        if let Some(at) = at.filter(|_| crate::format_on(format, opt)) {
            let m = match at {
                0 => crate::made_alone(d, opt, depth, Nest::As(format), cx),
                at => made_from(at, format, d, opt, depth, cx),
            };
            if let Some(m) = m {
                cx.lost.set(outer);
                if m.close.is_some() {
                    h.hints.report.lock().unwrap().close.insert((h.path(), format.name()));
                }
                return Some(m);
            }
        }
        let lost = cx.lost.replace(outer);
        let mut r = h.hints.report.lock().unwrap();
        r.turned_down.insert((h.path(), format.name(), lost));
    }
    let mut not = h.not.clone();
    // (not text: no format of text looked for)
    if h.text == Some(false) {
        not.extend(Format::ALL.into_iter().filter(|f| f.spec().text));
    }
    let nest = match h.text {
        Some(true) if detect(d).is_none_or(|f| f.spec().text) => Some(Nest::As(Format::Text)),
        // (a format told turned it down: detected, whatever its name tells)
        _ if matches!(h.format, Told::As(_)) && matches!(h.kind(), Kind::Format(_)) => Some(Nest::Scan),
        _ => Nest::Scan.of(h.kind()),
    }?;
    // (a format not to look for, as its name tells: looked for as any other)
    let nest = match nest {
        Nest::As(f) if not.contains(&f) => Nest::Scan,
        n => n,
    };
    detect::without(&not, || crate::made_alone(d, opt, depth, nest, cx))
}

/// `aqr-offset`'s value `v`, decimal or hexadecimal (`0x`).
fn offset(v: &str) -> Option<usize> {
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => usize::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
}

/// `d` as data up to `at` (scanned as any) and a file of `format` from there on ([`made_hinted`]),
/// where the format takes it: the hint the file's (its recompressor's parameters, [`within`]).
fn made_from(at: usize, format: Format, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    let (head, tail) = d.split_at(at);
    let hint = cx.within.get().map(|w| w.1);
    let outer = cx.within.replace(hint.map(|i| (tail.as_ptr() as usize, i)));
    let m = crate::made_alone(tail, opt, depth, Nest::As(format), cx);
    cx.within.set(outer);
    let tail = m?.cut(tail);
    let mut f = Formula::default();
    let head = match crate::made_alone(head, opt, depth, Nest::Scan, cx) {
        Some(m) => f.embed(m.cut(head)),
        None => f.push("raw", head.to_vec()),
    };
    let tail = f.embed(tail);
    f.root = aqr_formula::Node::concat([head, tail]);
    Some(Made::from(f))
}

/// The hint of `d` where it is the data of one being made ([`made`]): for its format's
/// recompressor (its parameters) and a container's (its members' hints, [`Hints::member`]).
pub(crate) fn within(d: &[u8], cx: &Cx) -> Option<Arc<Hint>> {
    cx.within.get().filter(|w| w.0 == d.as_ptr() as usize).map(|w| get(w.1))
}

thread_local! {
    /// The hint of the data being recompressed on this thread, where the attributes may tell of
    /// what it holds ([`enter`]).
    static CURRENT: RefCell<Option<Arc<Hint>>> = const { RefCell::new(None) };
}

/// The hint of `d` being recompressed ([`within`]) made the one its recompressor's entries are
/// told of ([`kinds`]) until this is dropped.
pub(crate) struct Entered(Option<Arc<Hint>>);

pub(crate) fn enter(d: &[u8], cx: &Cx) -> Entered {
    let h = within(d, cx).filter(|h| h.descends);
    Entered(CURRENT.with(|c| c.replace(h)))
}

impl Entered {
    /// The hint entered, if any.
    pub(crate) fn hint(&self) -> Option<Arc<Hint>> {
        CURRENT.with(|c| c.borrow().clone())
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = self.0.take());
    }
}

/// What the entries of the container being recompressed on this thread are by their names: as
/// its hint tells ([`enter`]), else as their names do ([`kind_by_name`]).
pub(crate) fn kinds() -> Kinds {
    match CURRENT.with(|c| c.borrow().clone()) {
        Some(h) => Kinds::new(move |n| Hints::member(&h, n, kind_by_name(n))),
        None => Kinds::new(kind_by_name),
    }
}

/// The formats whose data is one content of no name (`PATH>` of the attributes).
const NAMELESS: &[Format] =
    &[Format::Gzip, Format::Zlib, Format::Lz4, Format::Lzma, Format::Xz, Format::Lzo, Format::Bvx, Format::Bzip2];

/// The contents of `f`, a formula of data of `format` whose hint `entered` tells of what it
/// holds, of the kinds the attributes give them, where it is one content of no name
/// ([`NAMELESS`]).
pub(crate) fn nameless(f: &mut Formula, format: Format, entered: &Entered) {
    let Some(h) = entered.hint().filter(|_| NAMELESS.contains(&format)) else { return };
    for s in f.streams.iter_mut().filter(|s| s.content && !matches!(s.kind, Kind::Hint(_))) {
        s.kind = Hints::member(&h, b"", s.kind);
    }
}

/// The streams of `f` of [`Kind::Hint`] made of the kinds they would have been of
/// ([`Hint::kind`]), before the archive is encoded.
pub(crate) fn settle(f: &mut Formula) {
    for s in &mut f.streams {
        if let Kind::Hint(i) = s.kind {
            s.kind = get(i).kind();
        }
    }
}
