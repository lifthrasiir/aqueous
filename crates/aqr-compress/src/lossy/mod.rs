//! Equivalent lossy compression ([`Options::lossy`], `aqr -c --lossy`; docs/lossy-design.md): a file
//! X is archived as another file R that every program reading it takes as the same (view(R) =
//! view(X)), what only the encoder of X chose dropped. The archive gives back R, byte for byte,
//! and is verified against it; the entry is marked ([`Mark`], an extension key).
//!
//! R is made beforehand and archived losslessly (the design's way A): its streams made by an
//! encoder the recompressors predict exactly, its record then next to nothing. Each format's
//! rewriter is of a [`Class`]:
//!
//! - `coding`: the DEFLATE streams of ZIP archives (`zip`) and gzip files (`gzip`) that zlib
//!   does not give back made again by zlib's emulation ([`aqr_flate::zlib::compress`], level 6);
//!   sizes, offsets and the rest of the container written again. Their records are then a few
//!   bytes at any level ([`Rewritten`]); from level 4 on, where DEFLATE streams are rebuilt.
//! - `slack`: the bytes of a Minecraft region file (`mca`) that no chunk takes (freed sectors,
//!   the ends of chunks' last sectors) zeroed in place; and those of a SQLite database
//!   (`sqlite`) that SQLite never reads: the free space of b-tree pages (freeblocks but their
//!   headers), the freelist's pages but their lists, the ends of last overflow pages (never
//!   its header nor the bytes each page reserves). A database with its journal archived along
//!   is kept as it is ([`Rewriting::journaled`]).
//!
//! Each R is checked against X by code of its own ([`check`]: flate2 rather than aqr-flate,
//! parsers apart from the rewriters'); where the check fails, X is archived and the failure told
//! ([`Rewrite::Failed`]). Only whole files are rewritten (no file found in other data), and only
//! files read as a content of [`crate::compress_tree`]: [`crate::compress`] archives the file as
//! it is.

mod check;
mod gzip;
mod mca;
mod sqlite;
mod zip;

use std::collections::HashMap;
use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use aqr_formula::Bytes;

use crate::{Format, Options, Source, detect};

/// What a rewriting may lose, by its kind (`--lossy=CLASS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// Compressed streams made again by an encoder the recompressors predict exactly: what
    /// tells the encoder of the original is lost.
    Coding,
    /// Bytes that no structure of the file points to zeroed in place: deleted data is lost.
    Slack,
    /// The structure laid out again (none yet).
    Layout,
}

impl Class {
    pub const ALL: [Class; 3] = [Class::Coding, Class::Slack, Class::Layout];

    pub fn name(self) -> &'static str {
        match self {
            Class::Coding => "coding",
            Class::Slack => "slack",
            Class::Layout => "layout",
        }
    }

    pub fn from_name(s: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.name() == s)
    }
}

/// A rewriter: of the files of `format`, losing what `class` does. `rewrite` gives R, or None
/// where nothing is to change, or why the file is kept as it is; `check` tells whether R is
/// what X is, by code of its own.
struct Rewriter {
    class: Class,
    format: Format,
    rewrite: RewriteFn,
    check: fn(&[u8], &[u8]) -> Result<(), String>,
}

/// A rewriter's work ([`Rewriter::rewrite`]).
type RewriteFn = fn(&[u8], &Options) -> Result<Option<Vec<u8>>, String>;

/// Every rewriter, in the order they are applied to a file.
const REWRITERS: &[Rewriter] = &[
    Rewriter { class: Class::Coding, format: Format::Zip, rewrite: zip::coding, check: check::zip },
    Rewriter { class: Class::Coding, format: Format::Gzip, rewrite: gzip::coding, check: check::gzip },
    Rewriter { class: Class::Slack, format: Format::Mca, rewrite: mca::slack, check: check::mca },
    Rewriter { class: Class::Slack, format: Format::Sqlite, rewrite: sqlite::slack, check: check::sqlite },
];

/// What may be lost (`--lossy`): by class, the formats it is let to rewrite (none named: all).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lossy {
    allowed: Vec<(Class, Option<Vec<Format>>)>,
}

impl Lossy {
    /// `--lossy=SPEC`: classes separated by commas, each `CLASS` or `CLASS:FORMAT+FORMAT...`,
    /// or `all` (every class); empty, `coding` (the only one losing no deleted data). A format
    /// must be one that the class rewrites; a class rewriting nothing yet is turned down.
    pub fn parse(spec: &str) -> Result<Lossy, String> {
        let spec = if spec.is_empty() { "coding" } else { spec };
        let mut allowed: Vec<(Class, Option<Vec<Format>>)> = Vec::new();
        for item in spec.split(',') {
            let (name, formats) = match item.split_once(':') {
                Some((n, f)) => (n, Some(f)),
                None => (item, None),
            };
            let classes: Vec<Class> = match name {
                "all" if formats.is_none() => Class::ALL.to_vec(),
                "all" => return Err(format!("--lossy: all takes no formats ({item})")),
                n => vec![Class::from_name(n).ok_or_else(|| {
                    let names: Vec<&str> = Class::ALL.iter().map(|c| c.name()).collect();
                    format!("--lossy: not a class ({}, all): {n}", names.join(", "))
                })?],
            };
            for class in classes {
                let of: Vec<Format> = REWRITERS.iter().filter(|r| r.class == class).map(|r| r.format).collect();
                if of.is_empty() {
                    if name == "all" {
                        continue;
                    }
                    return Err(format!("--lossy: {} rewrites no format yet", class.name()));
                }
                let formats = match formats {
                    None => None,
                    Some(f) => Some(
                        f.split('+')
                            .map(|n| match Format::from_name(n) {
                                Some(f) if of.contains(&f) => Ok(f),
                                _ => {
                                    let names: Vec<&str> = of.iter().map(|f| f.name()).collect();
                                    Err(format!("--lossy: {} rewrites {}, not {n}", class.name(), names.join(", ")))
                                }
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                };
                match allowed.iter_mut().find(|a| a.0 == class) {
                    None => allowed.push((class, formats)),
                    Some(a) => match (&mut a.1, formats) {
                        (Some(v), Some(f)) => v.extend(f.into_iter().filter(|f| !v.contains(f)).collect::<Vec<_>>()),
                        (x, _) => *x = None,
                    },
                }
            }
        }
        allowed.sort_by_key(|a| a.0);
        Ok(Lossy { allowed })
    }

    /// Whether files of `format` may lose what `class` does.
    pub fn allows(&self, class: Class, format: Format) -> bool {
        self.allowed.iter().any(|(c, f)| *c == class && f.as_ref().is_none_or(|f| f.contains(&format)))
    }

    /// The classes allowed, in order.
    pub fn classes(&self) -> impl Iterator<Item = Class> + '_ {
        self.allowed.iter().map(|a| a.0)
    }
}

/// The extension key of the [`Mark`] of an entry archived as another file than it was.
pub const KEY: &str = "org.mearie.aqr.lossy";

/// The mark of an entry archived as another file than it was (docs/lossy-design.md §4.1):
/// what was lost, and the size of the file it was. Its value is the CBOR map `{0: [+ tstr], 1:
/// uint}` (classes by name, size), under [`KEY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    /// The classes of what was lost (their names: [`Class::name`], or of a later aqr).
    pub classes: Vec<String>,
    /// Bytes of the file as it was.
    pub size: u64,
}

impl Mark {
    /// The extension key (`aqr_archive::Extension`) of the mark.
    pub fn extension(&self) -> aqr_archive::Extension {
        let mut v = vec![0xa2, 0x00];
        cbor_head(&mut v, 4, self.classes.len() as u64);
        for c in &self.classes {
            cbor_head(&mut v, 3, c.len() as u64);
            v.extend_from_slice(c.as_bytes());
        }
        v.push(0x01);
        cbor_head(&mut v, 0, self.size);
        (KEY.as_bytes().into(), v.into())
    }

    /// The mark among the extension keys `x`, where there is one that reads as a mark.
    pub fn of(x: &[aqr_archive::Extension]) -> Option<Mark> {
        let (_, v) = x.iter().find(|(k, _)| &k[..] == KEY.as_bytes())?;
        Mark::decode(v)
    }

    fn decode(v: &[u8]) -> Option<Mark> {
        let mut p = 0;
        let (major, n) = cbor_read(v, &mut p)?;
        if major != 5 {
            return None;
        }
        let (mut classes, mut size) = (None, None);
        for _ in 0..n {
            let (major, key) = cbor_read(v, &mut p)?;
            if major != 0 {
                return None;
            }
            match key {
                0 => {
                    let (major, n) = cbor_read(v, &mut p)?;
                    if major != 4 || n == 0 {
                        return None;
                    }
                    let mut c = Vec::new();
                    for _ in 0..n {
                        let (major, len) = cbor_read(v, &mut p)?;
                        let end = p.checked_add(usize::try_from(len).ok()?)?;
                        if major != 3 || end > v.len() {
                            return None;
                        }
                        c.push(String::from_utf8(v[p..end].to_vec()).ok()?);
                        p = end;
                    }
                    classes = Some(c);
                }
                1 => {
                    let (major, n) = cbor_read(v, &mut p)?;
                    if major != 0 {
                        return None;
                    }
                    size = Some(n);
                }
                // (`hash`, and what a later aqr adds: a byte string, skipped)
                _ => {
                    let (major, len) = cbor_read(v, &mut p)?;
                    if major != 2 {
                        return None;
                    }
                    p = p.checked_add(usize::try_from(len).ok()?)?;
                    if p > v.len() {
                        return None;
                    }
                }
            }
        }
        (p == v.len()).then_some(())?;
        Some(Mark { classes: classes?, size: size? })
    }
}

/// The head of a CBOR data item of major type `major` and argument `n`, shortest.
fn cbor_head(v: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..24 => v.push(m | n as u8),
        24..0x100 => v.extend([m | 24, n as u8]),
        0x100..0x1_0000 => {
            v.push(m | 25);
            v.extend((n as u16).to_be_bytes());
        }
        0x1_0000..0x1_0000_0000 => {
            v.push(m | 26);
            v.extend((n as u32).to_be_bytes());
        }
        _ => {
            v.push(m | 27);
            v.extend(n.to_be_bytes());
        }
    }
}

/// The head of the CBOR data item at `p` of `v` (definite lengths only): its major type and
/// argument; `p` moved past it.
fn cbor_read(v: &[u8], p: &mut usize) -> Option<(u8, u64)> {
    let b = *v.get(*p)?;
    *p += 1;
    let (major, info) = (b >> 5, b & 31);
    let len = match info {
        0..24 => return Some((major, info as u64)),
        24 => 1,
        25 => 2,
        26 => 4,
        27 => 8,
        _ => return None,
    };
    let d = v.get(*p..*p + len)?;
    *p += len;
    Some((major, d.iter().fold(0u64, |n, &b| n << 8 | b as u64)))
}

aqr_common::zopt_namespace!("lossy", "equivalent lossy compression, --lossy (aqr-compress)");

aqr_common::zopt! {
    /// The most bytes of data of a DEFLATE stream made again ([`redeflate`]): one of more is kept
    /// as it is (its data held whole twice, with the stream made of it).
    static MAX_DATA: Opt<usize> = Opt::size("lossy:max-data", 1 << 30, "most bytes of a DEFLATE stream's data made again");
}

/// Bytes of record of a DEFLATE stream (by aqr-flate) at most, besides [`RECORD_PER`]-ths of
/// the stream, for it to be kept without making it again ([`redeflate`]): zlib's streams are a
/// few bytes (its emulation predicts every token).
const RECORD_KEPT: usize = 64;
const RECORD_PER: usize = 1024;

/// zlib's parameters of the DEFLATE streams `coding` makes ([`redeflate`]): its defaults (level 6).
pub(crate) const PARAMS: aqr_flate::zlib::Params =
    aqr_flate::zlib::Params { level: 6, strategy: aqr_flate::zlib::Strategy::Default, wbits: 15, mem: 8 };

/// Why the DEFLATE streams of a file are kept as they are at the level of `opt`, if they are:
/// below level 4, where they are never rebuilt (`crate::inflated`).
fn rebuilds(opt: &Options) -> Result<(), String> {
    match crate::inflated::on(opt) {
        false => Ok(()),
        true => Err("below level 4, DEFLATE streams are kept as they are".into()),
    }
}

/// Where the files that `--lossy` rewrote are, among the data of a batch: their DEFLATE streams
/// are zlib's at level 6 ([`PARAMS`]), but those kept as they were ([`redeflate`]), so they are
/// deconstructed at the adaptive profile with its parameters hinted, at whatever level
/// ([`Rewritten::flate`]): emulated, none searched for, their records are a few bytes. Below
/// level 7 aqr-flate does not emulate zlib otherwise (its static profiles' record of a stream of
/// zlib is some 10% of it, as of another encoder's): there the files that `--lossy` did not
/// rewrite keep their streams' coding, which decodes faster.
pub(crate) struct Rewritten(Vec<Range<*const u8>>);

// SAFETY: the addresses are only compared, never read through
unsafe impl Send for Rewritten {}
unsafe impl Sync for Rewritten {}

impl Rewritten {
    /// Of the files of the data at `regions`, if any.
    pub(crate) fn of(regions: impl Iterator<Item = Range<*const u8>>) -> Option<Rewritten> {
        let v: Vec<_> = regions.collect();
        (!v.is_empty()).then_some(Rewritten(v))
    }

    /// The profile and hints of a DEFLATE stream of `d` where it is in a file rewritten (at the
    /// level of `opt`); None where it is not, or the level is of the adaptive profile anyway
    /// (where zlib's parameters are searched for: those kept are not of [`PARAMS`]).
    pub(crate) fn flate(&self, d: &[u8], opt: &Options) -> Option<(u16, aqr_flate::Hints)> {
        let at = d.as_ptr_range();
        let within = self.0.iter().any(|r| r.start <= at.start && at.end <= r.end);
        (within && opt.flate_profile() < aqr_flate::ADAPTIVE)
            .then(|| (aqr_flate::ADAPTIVE, aqr_flate::Hints { params: Some(PARAMS), ..Default::default() }))
    }
}

/// The DEFLATE stream `raw` made again by zlib's emulation at level 6 ([`aqr_flate::zlib::compress`],
/// [`PARAMS`]) where that pays; None where `raw` is kept: zlib's at level 6 already, of more data
/// than [`MAX_DATA`], or not parsed whole (it is checked as it is all the same). The record of
/// the stream made is a few bytes ([`Rewritten`]), whatever the level.
///
/// Below level 7, every other stream pays: the record of `raw` is that of a static profile,
/// some 10% of it. From level 7 on, `raw` is deconstructed at the adaptive profile as it would
/// be kept, and made again only where its record is more than [`RECORD_KEPT`] (zlib's at
/// another level: emulated) and more than the new one's (an encoder aqr-flate predicts well:
/// Info-ZIP's, on small files, whose records were often smaller than zlib's).
fn redeflate(raw: &[u8], opt: &Options) -> Option<Vec<u8>> {
    let fopt = aqr_flate::Options {
        threads: 1,
        max_data: MAX_DATA.get(),
        zlib: aqr_flate::ZLIB.get(),
        verify: false,
        ..aqr_flate::Options::for_profile(opt.flate_profile().max(aqr_flate::ADAPTIVE)).ok()?
    };
    let parsed = aqr_flate::Parsed::new(raw, &fopt).ok()?;
    if opt.flate_profile() < aqr_flate::ADAPTIVE {
        let new = aqr_flate::zlib::compress(parsed.data(), PARAMS)?;
        return (new[..] != raw[..]).then_some(new);
    }
    let (data, was, _) = parsed.deconstruct_bytes(&fopt, &aqr_flate::Hints::default()).ok()?;
    if was.len() <= RECORD_KEPT + raw.len() / RECORD_PER {
        return None;
    }
    let new = aqr_flate::zlib::compress(&data, PARAMS)?;
    drop(data);
    let hints = aqr_flate::Hints { params: Some(PARAMS), ..Default::default() };
    let (_, is, _) = aqr_flate::Parsed::new(&new, &fopt).ok()?.deconstruct_bytes(&fopt, &hints).ok()?;
    (is.len() < was.len() && new[..] != raw[..]).then_some(new)
}

/// What became of a content that a rewriter of [`Options::lossy`] takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rewrite {
    /// Archived as another file (R): what it lost ([`Mark::classes`], [`Mark::size`] the size
    /// of X), and the size of R.
    Made { format: Format, mark: Mark, len: u64 },
    /// Kept as it is, as the rewriter turned it down: why (a signature, a layout it does not
    /// write again).
    Kept { format: Format, class: Class, reason: String },
    /// Kept as it is, as R was not found the same as X ([`check`]): why. A bug to report.
    Failed { format: Format, class: Class, reason: String },
}

/// `d` rewritten as `lossy` lets, or None where no rewriter takes it nor changes anything: what
/// became of it, with R where it is [`Rewrite::Made`].
pub(crate) fn rewrite(d: &[u8], lossy: &Lossy, opt: &Options) -> Option<(Rewrite, Option<Vec<u8>>)> {
    let format = detect(d)?;
    let mut r: Option<Vec<u8>> = None;
    let mut classes = Vec::new();
    for w in REWRITERS.iter().filter(|w| w.format == format && lossy.allows(w.class, format)) {
        // (coding only where the format is recompressed at all: else its streams are kept)
        if w.class == Class::Coding && !crate::format_on(format, opt) {
            continue;
        }
        let x = r.as_deref().unwrap_or(d);
        let made = std::panic::catch_unwind(|| (w.rewrite)(x, opt))
            .unwrap_or_else(|e| Err(format!("rewriter panicked: {}", panic_text(&e))));
        match made {
            Ok(None) => {}
            Ok(Some(y)) => {
                let checked = std::panic::catch_unwind(|| (w.check)(x, &y))
                    .unwrap_or_else(|e| Err(format!("check panicked: {}", panic_text(&e))));
                if let Err(reason) = checked {
                    return Some((Rewrite::Failed { format, class: w.class, reason }, None));
                }
                classes.push(w.class.name().to_string());
                r = Some(y);
            }
            // (one turning it down: the file kept whole, what the others did too)
            Err(reason) => return Some((Rewrite::Kept { format, class: w.class, reason }, None)),
        }
    }
    let r = r.filter(|r| r[..] != d[..])?;
    let made = Rewrite::Made { format, mark: Mark { classes, size: d.len() as u64 }, len: r.len() as u64 };
    Some((made, Some(r)))
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    match (e.downcast_ref::<&str>(), e.downcast_ref::<String>()) {
        (Some(s), _) => s.to_string(),
        (_, Some(s)) => s.clone(),
        _ => "(no message)".into(),
    }
}

/// The contents of `src` as [`Options::lossy`] lets them be rewritten, for the batches of
/// [`crate::compress_tree`]: a content rewritten is R wherever it is read whole ([`Source::read`])
/// and, once it is, in its ranges too ([`Source::read_range`]), until its batch is written
/// ([`Source::done`]); its size ([`Source::size`]) stays X's, for the batches are laid out by
/// it before it is read. What is looked at before its batch (by `together`, `lay_alike`, the
/// siblings) is of `src` itself, X.
pub(crate) struct Rewriting<'a, S: ?Sized> {
    src: &'a S,
    lossy: Option<&'a Lossy>,
    opt: &'a Options,
    /// R of the contents rewritten and read, until their batches are written.
    held: Mutex<HashMap<usize, Bytes>>,
    /// What became of the contents that a rewriter takes.
    pub(crate) made: Mutex<HashMap<usize, Rewrite>>,
    /// The sizes of the contents by their names, for [`Rewriting::journaled`].
    sizes: std::sync::OnceLock<HashMap<&'a [u8], u64>>,
}

impl<'a, S: Source + ?Sized> Rewriting<'a, S> {
    pub(crate) fn new(src: &'a S, opt: &'a Options) -> Rewriting<'a, S> {
        let (held, made, sizes) = Default::default();
        Rewriting { src, lossy: opt.lossy.as_ref(), opt, held, made, sizes }
    }

    /// Why content `c`, a SQLite database, is kept as it is, if it is: where a journal of it is
    /// archived along (`NAME-journal` or `NAME-wal`, not empty), the database is not all there is
    /// of it. A hot rollback journal restores pages as they were before a transaction, and a page
    /// the transaction freed holds what the database has again once it is rolled back: a
    /// database written as it was archived. (The journal not among the inputs is not seen.)
    fn journaled(&self, c: usize) -> Option<String> {
        let sizes =
            self.sizes.get_or_init(|| (0..self.src.len()).map(|c| (self.src.name(c), self.src.size(c))).collect());
        let name = self.src.name(c);
        [&b"-journal"[..], b"-wal"].into_iter().find_map(|suffix| {
            let beside = [name, suffix].concat();
            sizes
                .get(&beside[..])
                .filter(|&&n| n > 0)
                .map(|_| format!("a {} archived along, not empty", String::from_utf8_lossy(&suffix[1..])))
        })
    }

    fn held(&self, c: usize) -> Option<Bytes> {
        self.lossy?;
        self.held.lock().unwrap().get(&c).cloned()
    }
}

impl<S: Source + ?Sized> Source for Rewriting<'_, S> {
    fn len(&self) -> usize {
        self.src.len()
    }

    fn name(&self, c: usize) -> &[u8] {
        self.src.name(c)
    }

    fn size(&self, c: usize) -> u64 {
        self.src.size(c)
    }

    fn read(&self, c: usize) -> io::Result<Bytes> {
        let Some(lossy) = self.lossy else { return self.src.read(c) };
        if let Some(r) = self.held(c) {
            return Ok(r);
        }
        let d = self.src.read(c)?;
        let _s = aqr_common::trace::span("lossy");
        if detect(&d) == Some(Format::Sqlite)
            && lossy.allows(Class::Slack, Format::Sqlite)
            && let Some(reason) = self.journaled(c)
        {
            let kept = Rewrite::Kept { format: Format::Sqlite, class: Class::Slack, reason };
            self.made.lock().unwrap().insert(c, kept);
            return Ok(d);
        }
        let Some((w, r)) = rewrite(&d, lossy, self.opt) else { return Ok(d) };
        let r = match r {
            Some(r) => {
                let r = Bytes::shared(Arc::new(r));
                self.held.lock().unwrap().insert(c, r.clone());
                r
            }
            None => d,
        };
        self.made.lock().unwrap().insert(c, w);
        Ok(r)
    }

    fn read_range(&self, c: usize, range: Range<u64>) -> io::Result<Vec<u8>> {
        match self.held(c) {
            Some(r) => {
                let end = (range.end as usize).min(r.len());
                Ok(r[(range.start as usize).min(end)..end].to_vec())
            }
            None => self.src.read_range(c, range),
        }
    }

    fn intact(&self, c: usize) -> io::Result<()> {
        self.src.intact(c)
    }

    fn done(&self, c: usize) {
        if self.lossy.is_some() {
            self.held.lock().unwrap().remove(&c);
        }
        self.src.done(c)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_tree;

#[cfg(test)]
mod tests_sqlite;
