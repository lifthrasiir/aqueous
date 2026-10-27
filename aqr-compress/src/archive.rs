//! Archives of several files ([`compress_tree`]; spec, "Segments and entries"): the files of a
//! [`Tree`] are recompressed as the entries of a ZIP archive are, each by its kind by its name
//! ([`kind_by_name`]) and up to [`Options::depth`] formats deep from the file, the PNG files
//! alike among them coded as sets; each file's formula is then a leaf of the archive's one
//! segment. The files are made at once, on up to [`Options::threads`] threads (the formulas of
//! those of pure formats beforehand, as contents are; then each file in a context of its own).
//!
//! The files are taken in batches of about [`Options::batch`] bytes, in their order (by
//! extension, then by name; PE files by likeness, from where the first is, [`lay_alike`]):
//! each batch read ([`Source`]), made, laid out, checked and written
//! before the next, so that what is held at once is about a batch's, whatever the files'
//! (but for the largest file). Files of the same content as one before are stored once. Files
//! that may hold each other (a file and a ZIP archive or gzip file of it, a file and the start
//! of it) are put in one batch ([`together`]), where what their streams share is stored once
//! ([`Formula::share_slices`]): the recompressors take what the batch holds as costing
//! nothing ([`Held`]).
//!
//! A file is to be read without the others, so what files share is bounded: a set of PNG files
//! takes at most [`Options::set_bytes`] bytes of them (as it is decoded whole for any of them),
//! and the streams of a class and kind of the files of a batch are put one after another,
//! solid, in non-leaf blocks of at most [`Options::block`] bytes, but those that do not compress
//! ([`aqr::Encoder::compresses`]).
//!
//! With [`Options::verify`], each batch is decoded alone ([`aqr::SegmentWriter::standalone`],
//! the applications' outputs taken from those evaluated beforehand, as [`crate::compress`]
//! does) and its files compared with the input: those not given back (all of the batch's where
//! it does not decode) are made again kept raw.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::ops::Range;
use std::sync::Mutex;

use aqr_decompress::{MODULES, Native};
use aqr_formula::aqr::{self, Batch, ContentAt, Leaf, Placed, SegmentWriter, Tree};
use aqr_formula::probe::{self, Held};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::{Cx, Once, Options, detect, kind_by_name, nested, place_all, sets, verify};

/// Streams shorter than this are put together with those of their class and kind without
/// telling whether they compress.
const MERGE_PROBE: usize = 4 << 10;

/// A file's content to archive: its data, and a name telling its kind ([`kind_by_name`]).
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub name: &'a [u8],
    pub data: &'a [u8],
}

/// Where the contents of the files to archive come from, read as their batch is made.
pub trait Source: Sync {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A name telling the kind of content `c` ([`kind_by_name`]), and its order.
    fn name(&self, c: usize) -> &[u8];

    /// Bytes of content `c`, as it is to be read (for the batches: it may be read otherwise).
    fn size(&self, c: usize) -> u64;

    fn read(&self, c: usize) -> io::Result<Vec<u8>>;

    /// Bytes `range` of content `c` (those within it), for a look at its start or its end.
    fn read_range(&self, c: usize, range: Range<u64>) -> io::Result<Vec<u8>> {
        let d = self.read(c)?;
        let end = (range.end as usize).min(d.len());
        Ok(d[(range.start as usize).min(end)..end].to_vec())
    }
}

impl Source for [Input<'_>] {
    fn len(&self) -> usize {
        <[Input]>::len(self)
    }

    fn name(&self, c: usize) -> &[u8] {
        self[c].name
    }

    fn size(&self, c: usize) -> u64 {
        self[c].data.len() as u64
    }

    fn read(&self, c: usize) -> io::Result<Vec<u8>> {
        Ok(self[c].data.to_vec())
    }

    fn read_range(&self, c: usize, range: Range<u64>) -> io::Result<Vec<u8>> {
        let d = self[c].data;
        let end = (range.end as usize).min(d.len());
        Ok(d[(range.start as usize).min(end)..end].to_vec())
    }
}

/// An archive of several files, and what became of them.
pub struct Archived {
    pub archive: Vec<u8>,
    pub report: Report,
}

/// What became of the files of an archive.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// By content.
    pub files: Vec<FileReport>,
    /// The non-leaf blocks that several files slice.
    pub solid: Vec<Solid>,
    /// The contents coded against each other, a set each.
    pub sets: Vec<Vec<usize>>,
    /// Bytes of the archive but for the files': its header and footer, the module, resource and
    /// metadata blocks.
    pub overhead: usize,
    /// The contents kept raw because the archive did not give them back.
    pub unverified: Vec<usize>,
    /// Bytes of the archive, and of the contents read.
    pub len: usize,
    pub read: u64,
}

#[derive(Debug, Clone, Default)]
pub struct FileReport {
    /// The format the data starts like ([`detect`]).
    pub format: Option<&'static str>,
    /// The modules its formula applies, in order (none: kept as it is).
    pub modules: Vec<Module>,
    /// Bytes of the archive that are the file's: its leaf block, and the part of the non-leaf
    /// blocks it slices by the bytes it slices of them (and of a set, by the sizes of its
    /// outputs). None for a file of the same content as another.
    pub archived: f64,
    /// The content it is the same as, stored once.
    pub same_as: Option<usize>,
    /// Bytes of the content.
    pub len: u64,
}

/// A non-leaf block that several files slice.
#[derive(Debug, Clone)]
pub struct Solid {
    pub class: String,
    pub kind: Kind,
    pub len: usize,
    pub archived: usize,
    /// The contents that slice it, in order.
    pub contents: Vec<usize>,
}

/// The archive of `tree`, whose files are of `contents` (by [`EntryNode::File`]), with what
/// became of them. Panics on a tree that is not one the spec allows.
///
/// [`EntryNode::File`]: aqr::EntryNode::File
pub fn compress_tree(tree: &Tree, contents: &[Input], opt: &Options) -> Archived {
    let mut archive = Vec::new();
    let report = compress_tree_to(tree, contents, opt, &mut archive).expect("no failure but of the tree");
    Archived { archive, report }
}

/// The archive of `tree`, whose files are of the contents of `src`, written to `out` as it is
/// made, with what became of the files. Fails where a content cannot be read or the archive
/// written, and on a tree the spec does not allow.
pub fn compress_tree_to<S: Source + ?Sized>(
    tree: &Tree,
    src: &S,
    opt: &Options,
    out: &mut dyn Write,
) -> io::Result<Report> {
    let n = src.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| sort_key(src.name(a)).cmp(&sort_key(src.name(b))).then(a.cmp(&b)));
    // the contents that may hold each other together, in the place of the first, a batch never
    // cutting them apart
    let (group, entries, pe) = together(src, opt)?;
    lay_alike(&mut order, &pe, &group, src, opt)?;
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    order.iter().for_each(|&c| members.entry(group[c]).or_default().push(c));
    let mut batches: Vec<Vec<usize>> = vec![Vec::new()];
    let mut bytes = 0u64;
    for &c in &order {
        let Some(unit) = members.remove(&group[c]) else { continue };
        let size = unit.iter().fold(0u64, |n, &c| n.saturating_add(src.size(c)));
        if !batches.last().unwrap().is_empty() && bytes.saturating_add(size) > opt.batch as u64 {
            batches.push(Vec::new());
            bytes = 0;
        }
        batches.last_mut().unwrap().extend(unit);
        bytes = bytes.saturating_add(size);
    }

    let mut w = SegmentWriter::new();
    out.write_all(SegmentWriter::header())?;
    let mut first: HashMap<(u128, usize), usize> = HashMap::new();
    let mut at: Vec<Option<ContentAt>> = vec![None; n];
    let mut r = Report { files: vec![FileReport::default(); n], ..Report::default() };
    for batch in &batches {
        let read = aqr_common::pool::par_map(batch.len(), opt.threads, |i| {
            let d = src.read(batch[i])?;
            let hash = xxhash_rust::xxh3::xxh3_128(&d);
            Ok::<_, io::Error>((d, hash))
        });
        let mut files: Vec<File> = Vec::with_capacity(batch.len());
        let mut data = Vec::with_capacity(batch.len());
        for (&c, d) in batch.iter().zip(read) {
            let (d, hash) = d?;
            r.read += d.len() as u64;
            r.files[c].len = d.len() as u64;
            match first.entry((hash, d.len())) {
                std::collections::hash_map::Entry::Occupied(e) => r.files[c].same_as = Some(*e.get()),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(c);
                    files.push(File::of(c, &d, hash, kind_by_name(src.name(c)), entries.contains(&c)));
                    data.push(d);
                }
            }
        }
        if files.is_empty() {
            continue;
        }
        let mut raw = HashSet::new();
        let made = loop {
            let made = make(&w, &files, data, &raw, src, opt);
            let bad: Vec<usize> = made.bad.iter().copied().filter(|k| !raw.contains(k)).collect();
            if bad.is_empty() {
                break made;
            }
            raw.extend(bad);
            data = reread(src, &files, opt.threads)?;
        };
        out.write_all(&made.batch.bytes)?;
        w.commit(&made.batch);
        for (k, file) in files.iter().enumerate() {
            let (l, o) = made.at[k];
            at[file.c] = Some(ContentAt { leaf: made.batch.leaf_at[l], output: o, size: file.len as u64 });
            let f = &made.files[k];
            let c = file.c;
            (r.files[c].format, r.files[c].modules, r.files[c].archived) = (f.format, f.modules.clone(), f.archived);
        }
        let global = |ks: &[usize]| ks.iter().map(|&k| files[k].c).collect::<Vec<_>>();
        for s in made.solid {
            r.solid.push(Solid { contents: global(&s.contents), ..s });
        }
        r.sets.extend(made.sets.iter().map(|s| global(s)));
        r.unverified.extend(raw.iter().map(|&k| files[k].c));
    }
    let contents: Vec<ContentAt> =
        (0..n).map(|c| at[c].or_else(|| at[r.files[c].same_as?]).expect("every content made")).collect();
    // (an encoder of the metadata block only)
    let enc = aqr::Encoder::with_threads(opt.level, opt.threads);
    let (end, _) = w.finish(&enc, tree, &contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.0))?;
    out.write_all(&end)?;
    r.len = w.len() + end.len();
    r.overhead = r.len - r.files.iter().map(|f| f.archived).sum::<f64>().round() as usize;
    r.unverified.sort_unstable();
    Ok(r)
}

/// Contents of at least this many bytes are looked into for others they may hold
/// ([`together`]), as [`Held`] is.
const TOGETHER_MIN: u64 = Held::MIN as u64;

/// Members of ar, tar and cpio archives of at least this many bytes are looked for among the
/// contents ([`together`]): the files of a tar archive are mostly small.
const MEMBER_MIN: u64 = 4 << 10;

/// Slices shorter than this are not shared with others ([`Formula::share_slices`]): what they
/// share is found by compressing them solid, or not at all where they are apart (a tar
/// archive's members of a kind, and the same files of other kinds, apart from it).
const SLICE_MIN: usize = 1 << 10;

/// The most bytes of contents put in one batch for holding each other ([`together`]).
const TOGETHER_MAX: u64 = 2 << 30;

/// For each content of `src`, the first of those that may hold each other (where the archive
/// stores what they share once, [`Formula::share_slices`], only within a batch): a ZIP archive
/// or gzip file and the contents of its entries (of the CRC-32 and size that its central
/// directory or its trailer tells; and other archives with such entries), an ar, tar or cpio
/// archive and the contents of its members (read whole, one at a time, for their CRC-32; those
/// of at least [`MEMBER_MIN`] bytes), and contents that start alike (their first 64 KiB; the
/// same file, longer or shorter). Of the contents of at least [`TOGETHER_MIN`] bytes, which are
/// read at their start (and the archives at their end, the contents of the sizes of entries
/// whole); together, no more than [`TOGETHER_MAX`] bytes. Returns them, and the contents found
/// as entries or members of others, and the PE files.
fn together<S: Source + ?Sized>(src: &S, opt: &Options) -> io::Result<(Vec<usize>, HashSet<usize>, Vec<usize>)> {
    const HEAD: u64 = 64 << 10;
    // (the end of a ZIP archive holding its central directory, mostly)
    const TAIL: u64 = 1 << 20;
    let n = src.len();
    let large: Vec<usize> = (0..n).filter(|&c| src.size(c) >= TOGETHER_MIN).collect();
    // of each: the hash of its start, the CRC-32 and size of the entries it holds, and whether
    // it is an archive of aqr-bundle's (whose members are known once it is read whole) or a PE
    // file
    type Looked = (u64, Vec<(u32, u64)>, bool, bool);
    let looked = aqr_common::pool::par_map(large.len(), opt.threads, |i| -> io::Result<Looked> {
        let (c, size) = (large[i], src.size(large[i]));
        let head = src.read_range(c, 0..HEAD)?;
        let format = detect(&head);
        let held = match format {
            Some(detect::Format::Zip) => aqr_zip::zip::entry_sums(&src.read_range(c, size.saturating_sub(TAIL)..size)?),
            Some(detect::Format::Gzip) => {
                let t = src.read_range(c, size - 8..size)?;
                let word = |p: usize| u32::from_le_bytes(t[p..p + 4].try_into().expect("4 bytes"));
                vec![(word(0), word(4) as u64)]
            }
            _ => Vec::new(),
        };
        let held = held.into_iter().filter(|e| e.1 >= TOGETHER_MIN).collect();
        let bundle = format.is_some_and(detect::Format::is_bundle);
        Ok((xxhash_rust::xxh3::xxh3_64(&head), held, bundle, format == Some(detect::Format::Pe)))
    });
    let mut parent: Vec<usize> = (0..n).collect();
    let mut bytes: Vec<u64> = (0..n).map(|c| src.size(c)).collect();
    fn root(p: &mut [usize], mut c: usize) -> usize {
        while p[c] != c {
            (p[c], c) = (p[p[c]], p[c]);
        }
        c
    }
    let mut join = |a: usize, b: usize| {
        let (a, b) = (root(&mut parent, a), root(&mut parent, b));
        if a != b && bytes[a].saturating_add(bytes[b]) <= TOGETHER_MAX {
            let (a, b) = (a.min(b), a.max(b));
            parent[b] = a;
            bytes[a] += bytes[b];
        }
    };
    // by what they start with (but for many alike, which are not the same file), and by the
    // entries of the archives (the contents of their sizes told by their CRC-32, of the sizes
    // of few)
    let mut heads: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut sizes: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut by_entry: HashMap<(u32, u64), Vec<usize>> = HashMap::new();
    let mut pe = Vec::new();
    for (&c, l) in large.iter().zip(looked) {
        let (head, mut entries, bundle, is_pe) = l?;
        if is_pe {
            pe.push(c);
        }
        // (one past what a batch may hold is not put with others)
        if bundle && src.size(c) <= TOGETHER_MAX {
            entries = member_sums(&src.read(c)?, opt.threads);
        }
        heads.entry(head).or_default().push(c);
        entries.into_iter().for_each(|e| by_entry.entry(e).or_default().push(c));
    }
    // (of those that may be a member)
    for c in (0..n).filter(|&c| src.size(c) >= MEMBER_MIN) {
        sizes.entry(src.size(c)).or_default().push(c);
    }
    for cs in heads.values().filter(|cs| (2..=8).contains(&cs.len())) {
        cs.iter().for_each(|&c| join(cs[0], c));
    }
    let wanted: Vec<usize> = {
        let mut w: Vec<usize> =
            by_entry.keys().filter_map(|e| sizes.get(&e.1).filter(|cs| cs.len() <= 8)).flatten().copied().collect();
        w.sort_unstable();
        w.dedup();
        w
    };
    let sums =
        aqr_common::pool::par_map(wanted.len(), opt.threads, |i| src.read(wanted[i]).map(|d| crc32fast::hash(&d)));
    let mut by_sum: HashMap<(u32, u64), Vec<usize>> = HashMap::new();
    for (&c, sum) in wanted.iter().zip(sums) {
        by_sum.entry((sum?, src.size(c))).or_default().push(c);
    }
    let mut entries = HashSet::new();
    for (e, archives) in &by_entry {
        let alike = by_sum.get(e).map_or(&[][..], |cs| &cs[..]);
        archives.iter().chain(alike).for_each(|&c| join(archives[0], c));
        entries.extend(alike);
    }
    Ok(((0..n).map(|c| root(&mut parent, c)).collect(), entries, pe))
}

/// Contents `cs` laid in `order` by likeness ([`probe::by_likeness`]), from where the first of
/// them is, but those put together with others (`group`, [`together`]): PE files, of which
/// those of a program in several versions, apart by their names, compress far better within a
/// block of each other (in 10gb, 436.7 → 395.6 MB in blocks of 16 MiB at zstd level 9, of 371.3
/// MB as one).
fn lay_alike<S: Source + ?Sized>(
    order: &mut Vec<usize>,
    cs: &[usize],
    group: &[usize],
    src: &S,
    opt: &Options,
) -> io::Result<()> {
    let mut members = vec![0usize; group.len()];
    group.iter().for_each(|&g| members[g] += 1);
    let alone: HashSet<usize> = cs.iter().copied().filter(|&c| members[group[c]] == 1).collect();
    let these: Vec<usize> = order.iter().copied().filter(|c| alone.contains(c)).collect();
    if these.len() < 2 {
        return Ok(());
    }
    let likes =
        aqr_common::pool::par_map(these.len(), opt.threads, |i| src.read(these[i]).map(|d| probe::likeness(&d)));
    let likes = likes.into_iter().collect::<io::Result<Vec<_>>>()?;
    let laid: Vec<usize> = probe::by_likeness(&likes).into_iter().map(|i| these[i]).collect();
    let at = order.iter().position(|c| alone.contains(c)).expect("one of them");
    order.retain(|c| !alone.contains(c));
    order.splice(at..at, laid);
    Ok(())
}

/// The CRC-32 and size of the files' contents in ar, tar or cpio archive `d`, those of at least
/// [`MEMBER_MIN`] bytes.
fn member_sums(d: &[u8], threads: usize) -> Vec<(u32, u64)> {
    use aqr_bundle::{formula, layout::Role};
    let recs = formula::detect(d).and_then(|f| formula::records(f, d)).unwrap_or_default();
    let bodies: Vec<&[u8]> = recs
        .iter()
        .filter_map(|r| r.body())
        .filter(|b| b.role == Role::File && b.span.len() as u64 >= MEMBER_MIN)
        .map(|b| &d[b.span.clone()])
        .collect();
    aqr_common::pool::par_map(bodies.len(), threads, |i| (crc32fast::hash(bodies[i]), bodies[i].len() as u64))
}

/// What files are ordered by, to put those alike next to each other: the extension, then the
/// name, of the last component.
fn sort_key(name: &[u8]) -> (Vec<u8>, &[u8]) {
    let base = name.rsplit(|&b| b == b'/').next().unwrap_or(name);
    let ext = base.iter().rposition(|&b| b == b'.').filter(|&p| p > 0).map_or(&[][..], |p| &base[p + 1..]);
    (ext.to_ascii_lowercase(), base)
}

/// A file of a batch: its content, the kind its name tells, and what is known of its data,
/// which is held only by the formula of the batch ([`make`]).
struct File {
    c: usize,
    kind: Kind,
    /// Whether it is found as an entry or member of another ([`together`]).
    entry: bool,
    len: usize,
    /// Its XXH3-128 ([`check`]) and its checksum ([`aqr::checksum`]).
    hash: u128,
    sum: u64,
    /// The format it starts like ([`detect`]).
    format: Option<&'static str>,
}

impl File {
    fn of(c: usize, d: &[u8], hash: u128, kind: Kind, entry: bool) -> File {
        File { c, kind, entry, len: d.len(), hash, sum: aqr::checksum(d), format: detect(d).map(|f| f.name()) }
    }

    /// Whether `d` is the file's data.
    fn is(&self, d: &[u8]) -> bool {
        d.len() == self.len && xxhash_rust::xxh3::xxh3_128(d) == self.hash
    }
}

/// The data of `files` read again from `src`, failing where it is no longer what it was.
fn reread<S: Source + ?Sized>(src: &S, files: &[File], threads: usize) -> io::Result<Vec<Vec<u8>>> {
    aqr_common::pool::par_map(files.len(), threads, |k| {
        let d = src.read(files[k].c)?;
        match files[k].is(&d) {
            true => Ok(d),
            false => Err(io::Error::other(format!("content {} changed while archived", files[k].c))),
        }
    })
    .into_iter()
    .collect()
}

/// A batch made: its blocks, each file's leaf and output (by its index in the batch), what
/// became of the files (by the same), and those not given back.
struct Made {
    batch: Batch,
    at: Vec<(usize, usize)>,
    files: Vec<FileReport>,
    solid: Vec<Solid>,
    sets: Vec<Vec<usize>>,
    bad: Vec<usize>,
}

/// Text of the files of a batch shorter than this in all is coded with dictionaries of each
/// text's own: one of them all would cost more than it saves.
const WORDS_MIN: usize = 256 << 10;

/// The dictionary of the text of a batch ([`Cx::words`]): of the files that are text (at least
/// [`aqr_wrt::formula::MIN_LEN`] bytes, but those kept raw), where they are [`WORDS_MIN`]
/// bytes in all.
fn words(regions: &[&[u8]], files: &[File], raw: &HashSet<usize>, opt: &Options) -> Option<aqr_wrt::formula::Shared> {
    let texts: Vec<&[u8]> = (0..files.len())
        .filter(|k| !raw.contains(k) && aqr_wrt::formula::starts(regions[*k]))
        .map(|k| regions[k])
        .collect();
    if texts.iter().map(|t| t.len()).sum::<usize>() < WORDS_MIN {
        return None;
    }
    aqr_wrt::formula::Shared::of(&texts, opt.threads)
}

/// The batch of `files` (by index, the data of each in `data`), to follow the blocks of `w`,
/// recompressed but those (by index) in `raw`. The data is the formula's: where it is needed
/// again, to verify the batch, it is read from `src`.
fn make<S: Source + ?Sized>(
    w: &SegmentWriter,
    files: &[File],
    data: Vec<Vec<u8>>,
    raw: &HashSet<usize>,
    src: &S,
    opt: &Options,
) -> Made {
    // a formula of the files as the contents of its streams, each a file of its own
    let mut f = Formula::default();
    let mut slices = Vec::with_capacity(files.len());
    for (k, (file, d)) in files.iter().zip(data).enumerate() {
        slices.push(if raw.contains(&k) {
            f.push_as("raw", file.kind, d)
        } else {
            f.push_content("raw", file.kind, d)
        });
    }
    f.root = Node::Concat(slices);

    // nested a format deeper, so that the files are as deep as a file alone is
    let nopt = Options { depth: opt.depth.saturating_add(1), ..opt.clone() };
    let total = files.iter().map(|f| f.len).sum();
    let mut cx = Cx::of_len(total, &nopt);
    cx.top_sets = Some(opt.set_bytes);
    // (the files known by where their copies are, and taken from the input; and what they
    // hold, a content told by where it is)
    let regions: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    cx.held = Some(std::sync::Arc::new(Held::of(&regions, opt.threads)));
    cx.share_words(words(&regions, files, raw, opt).map(std::sync::Arc::new));
    // (the text of a file found in another coded once, for both)
    cx.once = std::sync::Arc::new(Once::of(
        files.iter().enumerate().filter(|(k, f)| f.entry && !raw.contains(k)).map(|(_, f)| (f.len, f.hash)),
    ));
    cx.pre = opt.verify.then(|| std::sync::Arc::new(verify::Prevalidator::spawn(opt.threads, &regions)));
    let bases: Vec<usize> = f.streams.iter().map(|s| s.data.as_ptr() as usize).collect();
    let mut taken = HashSet::new();
    if let Some((mut children, plan)) = nested(&f, &nopt, 0, &cx) {
        let sets = sets::sets(&f, &mut children, &plan, &nopt, 0, &cx);
        for s in &sets {
            for &m in &s.members {
                if let &Node::Slice { stream, start, .. } = &children[m].0 {
                    taken.insert(bases[stream] + start as usize);
                }
            }
        }
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), sets, &nopt);
        // (what the files hold of each other, by what they are taken for: a file and the
        // same in an archive, both kept as they are)
        f.share_slices(SLICE_MIN);
    }
    // (a text formula kept that none took)
    cx.once = Default::default();
    let deferred: Vec<_> = cx.deferred.borrow_mut().drain(..).collect();
    if let Some(pre) = &cx.pre {
        deferred.into_iter().filter(|(at, _)| !taken.contains(at)).for_each(|(_, j)| pre.send(j));
    }
    let memo = cx.pre.take().map(verify::Prevalidator::finish_shared).unwrap_or_default();

    // a leaf for each file, or for each set of them
    let Node::Concat(nodes) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else { unreachable!() };
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut at = vec![(0, 0); files.len()];
    let mut set_leaf: HashMap<usize, usize> = HashMap::new();
    for (k, node) in nodes.into_iter().enumerate() {
        let sum = Some(files[k].sum);
        match node {
            Node::Output { shared, index, .. } => {
                let n = f.shared[shared].sizes.len();
                let l = *set_leaf.entry(shared).or_insert_with(|| {
                    leaves.push(Leaf { outputs: vec![Node::Concat(Vec::new()); n], sums: vec![None; n] });
                    leaves.len() - 1
                });
                (leaves[l].outputs[index], leaves[l].sums[index]) = (node, sum);
                at[k] = (l, index);
            }
            node => {
                leaves.push(Leaf { outputs: vec![node], sums: vec![sum] });
                at[k] = (leaves.len() - 1, 0);
            }
        }
    }

    // the streams of a class together, within a block each; but those that do not compress,
    // which would gain nothing and only make a file read alone read more
    for s in &mut f.streams {
        s.content = false;
        if s.kind != Kind::Compressed && s.data.len() >= MERGE_PROBE && !cx.enc.compresses(&s.data).expect("zstd") {
            s.kind = Kind::Compressed;
        }
    }
    let counts: Vec<usize> = leaves.iter().map(|l| l.outputs.len()).collect();
    f.root = Node::Concat(leaves.iter_mut().flat_map(|l| std::mem::take(&mut l.outputs)).collect());
    f.merge_classes_within(opt.block as u64);
    let Node::Concat(nodes) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else { unreachable!() };
    let mut nodes = nodes.into_iter();
    leaves.iter_mut().zip(counts).for_each(|(l, n)| l.outputs = nodes.by_ref().take(n).collect());

    let batch = w.batch(&cx.enc, &f, &leaves).expect("slices within their streams");
    drop(cx);
    let (reports, solid, sets) = report(&f, &leaves, &batch.layout, files, &at);
    drop(f);

    let bad = if opt.verify {
        let m = verify::Cached {
            native: Native { threads: opt.threads },
            memo: memo.into(),
            hits: Default::default(),
            files: &|k, range: Range<usize>| src.read_range(files[k].c, range.start as u64..range.end as u64).ok(),
        };
        check(w, &batch, files, &at, &m, opt.threads)
    } else {
        Vec::new()
    };
    Made { batch, at, files: reports, solid, sets, bad }
}

/// The files of batch `b` (by index; each at its leaf and output in `at`) that its blocks, decoded
/// alone with the modules of `m`, do not give back: all of them where it does not decode.
fn check(
    w: &SegmentWriter,
    b: &Batch,
    files: &[File],
    at: &[(usize, usize)],
    m: &verify::Cached,
    threads: usize,
) -> Vec<usize> {
    let c = w.standalone(b);
    let natives = aqr::natives(&MODULES, m);
    // the leaves in the order of their blocks, and of each the files of its outputs
    let mut by_block: Vec<usize> = (0..b.leaf_at.len()).collect();
    by_block.sort_by_key(|&l| b.leaf_at[l]);
    let mut of_leaf: Vec<Vec<(usize, usize)>> = vec![Vec::new(); b.leaf_at.len()];
    at.iter().enumerate().for_each(|(k, &(l, o))| of_leaf[l].push((o, k)));
    let bad = Mutex::new(Vec::new());
    let all = || (0..files.len()).collect();
    let Ok(a) = aqr::open(&c, &natives) else { return all() };
    let r = a.each_leaf(threads, |i, outputs| {
        let l = by_block[i];
        let mut bad = bad.lock().unwrap();
        match outputs {
            Ok(outputs) => {
                bad.extend(of_leaf[l].iter().filter(|&&(o, k)| !outputs.get(o).is_some_and(|d| files[k].is(d))).map(|&(_, k)| k))
            }
            Err(_) => bad.extend(of_leaf[l].iter().map(|&(_, k)| k)),
        }
        Ok(())
    });
    if r.is_err() {
        return all();
    }
    let mut bad = bad.into_inner().unwrap();
    bad.sort_unstable();
    bad
}

/// What became of `files` (by index) in batch `layout` of formula `f` and its `leaves`, each
/// file at its leaf and output in `at`: its report, the solid blocks and the sets.
fn report(
    f: &Formula,
    leaves: &[Leaf],
    layout: &[(Placed, usize)],
    files: &[File],
    at: &[(usize, usize)],
) -> (Vec<FileReport>, Vec<Solid>, Vec<Vec<usize>>) {
    // by leaf, the bytes it slices of each stream, and the modules it applies
    let mut sliced: Vec<HashMap<usize, u64>> = vec![HashMap::new(); leaves.len()];
    let mut modules: Vec<Vec<Module>> = vec![Vec::new(); leaves.len()];
    fn visit(n: &Node, sliced: &mut HashMap<usize, u64>, modules: &mut Vec<Module>, shared: &mut [bool]) {
        n.visit(&mut |n| match n {
            &Node::Slice { stream, len: len @ 1.., .. } => *sliced.entry(stream).or_default() += len,
            Node::Apply(a) if !modules.contains(&a.module) => modules.push(a.module),
            &Node::Output { shared: s, .. } => shared[s] = true,
            _ => {}
        })
    }
    for (l, leaf) in leaves.iter().enumerate() {
        let mut shared = vec![false; f.shared.len()];
        leaf.outputs.iter().for_each(|n| visit(n, &mut sliced[l], &mut modules[l], &mut shared));
        for s in (0..f.shared.len()).rev() {
            if shared[s] {
                f.shared[s].apply.inputs.iter().for_each(|n| visit(n, &mut sliced[l], &mut modules[l], &mut shared));
                if !modules[l].contains(&f.shared[s].apply.module) {
                    modules[l].push(f.shared[s].apply.module);
                }
            }
        }
    }
    let mut by_stream: HashMap<usize, Vec<usize>> = HashMap::new();
    for (l, s) in sliced.iter().enumerate() {
        s.keys().for_each(|&st| by_stream.entry(st).or_default().push(l));
    }
    let mut bytes = vec![0f64; leaves.len()];
    let mut solid = Vec::new();
    // by leaf, its files
    let mut of_leaf: Vec<Vec<usize>> = vec![Vec::new(); leaves.len()];
    (0..files.len()).for_each(|k| of_leaf[at[k].0].push(k));
    for &(p, n) in layout {
        match p {
            Placed::Formula(l) | Placed::Data(l) => bytes[l] += n as f64,
            Placed::NonLeaf(st) => {
                let ls = &by_stream[&st];
                let total: u64 = ls.iter().map(|&l| sliced[l][&st]).sum();
                ls.iter().for_each(|&l| bytes[l] += n as f64 * sliced[l][&st] as f64 / total.max(1) as f64);
                let mut ks: Vec<usize> = ls.iter().flat_map(|&l| of_leaf[l].iter().copied()).collect();
                ks.sort_unstable();
                if ks.len() > 1 {
                    let s = &f.streams[st];
                    solid.push(Solid {
                        class: s.class.to_string(),
                        kind: s.kind,
                        len: s.data.len(),
                        archived: n,
                        contents: ks,
                    });
                }
            }
            Placed::Module | Placed::Resource | Placed::Metadata => {}
        }
    }
    let mut sets = Vec::new();
    for (l, ks) in of_leaf.iter().enumerate() {
        if leaves[l].outputs.len() > 1 {
            let mut set = ks.clone();
            set.sort_by_key(|&k| at[k].1);
            sets.push(set);
        }
    }
    let out_len = |k: usize| leaves[at[k].0].outputs[at[k].1].len() as f64;
    let leaf_len: Vec<f64> = leaves.iter().map(|l| l.outputs.iter().map(|o| o.len() as f64).sum()).collect();
    let reports = (0..files.len())
        .map(|k| {
            let l = at[k].0;
            FileReport {
                format: files[k].format,
                modules: modules[l].clone(),
                archived: match leaf_len[l] {
                    n if n > 0.0 => bytes[l] * out_len(k) / n,
                    _ => bytes[l] / leaves[l].outputs.len() as f64,
                },
                same_as: None,
                len: files[k].len as u64,
            }
        })
        .collect();
    (reports, solid, sets)
}
