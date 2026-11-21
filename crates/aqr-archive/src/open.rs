//! Archives opened and decoded ([`decode`], [`open`], [`Archive`]): the footer and the segments
//! read, the files' trees and contents.

use super::*;

/// What an archive holds: a single file (where it has no metadata block), or files in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    File(Vec<u8>),
    /// The entries, and the contents of their files (by [`EntryNode::File`]).
    Tree(Box<Tree>, Vec<Vec<u8>>),
}

/// The file in archive `c`, an archive without a metadata block, its modules run by `runner`.
pub fn decode(c: &[u8], runner: &dyn Runner) -> Decoded<Vec<u8>> {
    match decode_value(c, runner)? {
        Value::File(d) => Ok(d),
        Value::Tree(..) => resource("an archive of several files"),
    }
}

/// What archive `c` holds, its modules run by `runner`.
pub fn decode_value(c: &[u8], runner: &dyn Runner) -> Decoded<Value> {
    decode_value_with(c, runner, 1)
}

/// [`decode_value`], its leaf blocks evaluated at once on up to `threads` threads (the calling
/// one and workers of [`aqr_common::pool`]). The first failure of a leaf, in their order, is
/// the archive's.
pub fn decode_value_with(c: &[u8], runner: &dyn Runner, threads: usize) -> Decoded<Value> {
    open(c, runner)?.value(threads)
}

/// How far [`Archive::each_leaf`] is, as it tells [`Archive::on_progress`]: the bytes of the
/// data blocks decompressed (as they are in the archive, whether Zstandard data or not), and of
/// the leaves' outputs evaluated, each of all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reached {
    pub decompressed: (u64, u64),
    pub evaluated: (u64, u64),
}

/// What [`Archive::each_leaf`] tells as it goes ([`Archive::on_progress`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// How far it is, as each data block is decoded and each leaf evaluated.
    Reached(Reached),
    /// Leaf `i` (by its index among the leaf blocks) being evaluated, then done with (its outputs
    /// taken by the caller too).
    Started(usize),
    Ended(usize),
}

/// What is told of the [`Step`]s, from any of the threads evaluating the leaves (not always in
/// order: a later [`Step::Reached`] may tell less).
pub type Telling = Arc<dyn Fn(Step) + Send + Sync>;

/// A content of a tree: its leaf, its output of it and its size.
pub(crate) type ContentRef = (usize, usize, u64);

/// How far an archive conforms to the archival profile ([`Archive::archival`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Archival {
    /// Leaf data blocks without a checksum.
    pub unchecked_leaves: usize,
    /// Formula blocks with an output no Checksum instruction covers.
    pub unchecked_formulas: usize,
}

impl Archival {
    pub fn conforms(&self) -> bool {
        self.unchecked_leaves == 0 && self.unchecked_formulas == 0
    }
}

/// An archive opened ([`open`]): its blocks framed, and its tree read where it has one. The
/// data of its blocks is decoded as the leaves are evaluated, a non-leaf block when first sliced
/// (and dropped once no slice of it is left), so that evaluating them takes about the memory of
/// those evaluated at once.
pub struct Archive<'c, 'n> {
    pub(crate) kept: Archived<'c>,
    runner: &'n dyn Runner,
    /// The modules loaded, by module block, as they are first applied.
    loaded: Vec<OnceLock<Decoded<Box<dyn Loaded + 'n>>>>,
    /// The tree, and its contents (each a leaf, an output of it and its size).
    pub(crate) tree: Option<(Tree, Vec<ContentRef>)>,
    /// What the last segment tells of the writing it ends ([`Archive::status`]).
    status: Option<Status>,
    /// The journal points of its segments, in the order of the segments ([`Archive::journal`]).
    journal: Vec<JournalPoint>,
    /// The budget the leaves evaluated take of by their weights ([`Archive::set_budget`]).
    budget: Option<&'n Budget>,
    /// Told how far the evaluation is ([`Archive::on_progress`]).
    tell: Option<Telling>,
    /// By leaf, the item its evaluation is told of as ([`Archive::set_items`]).
    items: Vec<Option<usize>>,
    /// Whether the pages of the blocks are let go of as they are read ([`Archive::let_go_read`]).
    let_go: bool,
    /// The payload decrypted as its data blocks are read, where it is ([`open_decrypting`]).
    sealed: Option<&'c envelope::Decrypting<'c>>,
}

/// A journal point of an archive (spec, "`journal`"): the state after a segment, named by a time
/// and a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPoint {
    /// Where the segment's metadata block is, from the archive's header magic: what
    /// [`open_upto`] and [`open_indexed_upto`] take to open the archive as it is at the point.
    pub metadata: usize,
    /// Milliseconds since the epoch (UTC).
    pub time: u64,
    pub label: String,
}

/// An archive's blocks: all of them read ([`open`]), or as its metadata blocks tell where they
/// are, read as they are needed ([`open_indexed`]).
pub(crate) enum Archived<'c> {
    Parsed(Box<Blocks<'c>>),
    Indexed(Box<index::Index<'c>>),
}

/// Archive `c` opened, its modules run by `runner`. An encrypted archive is a decryption
/// failure: its payload decrypted ([`envelope`]) is opened by [`open_payload`].
pub fn open<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner) -> Decoded<Archive<'c, 'n>> {
    if envelope::is_encrypted(c) {
        return Err(Failure::Decryption("an encrypted archive, and no passphrase".into()));
    }
    open_as(c, runner, false)
}

/// [`open`] of the archive as it is after the segment whose metadata block is at `metadata`
/// ([`JournalPoint::metadata`]): the segments after it left out, as if the archive ended there
/// (spec, "`journal`").
pub fn open_upto<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner, metadata: usize) -> Decoded<Archive<'c, 'n>> {
    if envelope::is_encrypted(c) {
        return Err(Failure::Decryption("an encrypted archive, and no passphrase".into()));
    }
    open_with(c, runner, false, false, Some(metadata))
}

/// The decrypted payload `c` of an encrypted archive opened, as [`open`] does, but for the
/// constraint of a payload: a size hint block first.
pub fn open_payload<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner) -> Decoded<Archive<'c, 'n>> {
    open_as(c, runner, true)
}

/// Archive `c` (the payload of an encrypted archive where `sealed`) opened as [`open`] does, but
/// salvaged (spec, "Salvage"): a file whose block is not there (an archive repaired from a
/// truncation, [`mirror::repair`]) is lost rather than the archive invalid, and so is a file of a
/// formula block that does not read, or that refers to blocks not there; its evaluation fails,
/// the others' as they do. Files are checked by their checksums where they have them.
pub fn open_salvaging<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner, sealed: bool) -> Decoded<Archive<'c, 'n>> {
    if envelope::is_encrypted(c) {
        return Err(Failure::Decryption("an encrypted archive, and no passphrase".into()));
    }
    open_with(c, runner, sealed, true, None)
}

/// The bytes of an archive (from its header magic to the end of its footer) as they are read: a
/// file's (mapped), or the payload of an encrypted archive, decrypted a chunk at a time as it is
/// read ([`envelope::Decrypting`]).
#[derive(Clone, Copy)]
pub enum Payload<'c> {
    Plain(&'c [u8]),
    Sealed(&'c envelope::Decrypting<'c>),
}

impl<'c> Payload<'c> {
    pub fn len(&self) -> usize {
        match self {
            Payload::Plain(c) => c.len(),
            Payload::Sealed(d) => d.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn sealed(&self) -> bool {
        matches!(self, Payload::Sealed(_))
    }

    /// Whether it is an archive of a tree (of a metadata block), as its footer tells: one that
    /// [`open_indexed`] reads only in part.
    pub fn of_tree(&self) -> bool {
        footer_fields(*self).is_ok_and(|f| f.3 != 0)
    }

    /// Bytes `r`: an invalid archive where they are not there (or of chunks that do not
    /// authenticate).
    pub(crate) fn get(&self, r: std::ops::Range<usize>) -> Decoded<&'c [u8]> {
        match *self {
            Payload::Plain(c) => c.get(r).ok_or_else(|| Failure::Invalid("truncated block".into())),
            Payload::Sealed(d) => d.get(r),
        }
    }

    /// All of its bytes (decrypted on up to `threads` threads).
    pub(crate) fn whole(&self, threads: usize) -> Decoded<&'c [u8]> {
        match *self {
            Payload::Plain(c) => Ok(c),
            Payload::Sealed(d) => d.all(threads),
        }
    }
}

/// Archive `p` opened as [`open`] does (an encrypted archive's payload as [`open_payload`]
/// does), but reading only what tells where its blocks are: the footer and the metadata blocks.
/// The blocks of a leaf are read as it is evaluated, those of the leaves taken alone
/// ([`Archive::contents_of`]) only (spec, "Partial access"): a file read alone reads its own
/// blocks, those it refers to, and the blocks between a formula block and the non-leaf blocks
/// it refers to (by their ordinals). The whole archive is read where all of it is evaluated or
/// told of (decrypted on up to `threads` threads), and an archive without a metadata block at
/// once.
pub fn open_indexed<'c, 'n>(p: Payload<'c>, runner: &'n dyn Runner, threads: usize) -> Decoded<Archive<'c, 'n>> {
    open_indexed_with(p, runner, threads, None, None)
}

/// [`open_indexed`] of the archive as it is after the segment whose metadata block is at
/// `metadata` ([`JournalPoint::metadata`]), as [`open_upto`] opens it.
pub fn open_indexed_upto<'c, 'n>(
    p: Payload<'c>,
    runner: &'n dyn Runner,
    threads: usize,
    metadata: usize,
) -> Decoded<Archive<'c, 'n>> {
    open_indexed_with(p, runner, threads, None, Some(metadata))
}

/// [`open_indexed`], of the entries at `paths` (each its names in turn, as the tree's) alone:
/// the metadata blocks read back from the last as far as each path is settled, it or an entry
/// above it found as other than a directory (spec, "Partial access"). Its tree has those paths
/// (and what is under them) as the archive's has them, but maybe not its other entries; its
/// leaves are evaluated only as they are taken alone ([`Archive::contents_of`]).
pub fn open_indexed_at<'c, 'n>(
    p: Payload<'c>,
    runner: &'n dyn Runner,
    threads: usize,
    paths: &[Vec<Vec<u8>>],
) -> Decoded<Archive<'c, 'n>> {
    open_indexed_with(p, runner, threads, Some(paths), None)
}

fn open_indexed_with<'c, 'n>(
    p: Payload<'c>,
    runner: &'n dyn Runner,
    threads: usize,
    paths: Option<&[Vec<Vec<u8>>]>,
    upto: Option<usize>,
) -> Decoded<Archive<'c, 'n>> {
    let footer = footer_fields(p)?;
    if footer.3 == 0 {
        return open_as(p.whole(threads)?, runner, p.sealed());
    }
    let (index, tree, status, journal) = index::open(p, threads, footer, paths, upto)?;
    let loaded = (0..index.modules_len()).map(|_| OnceLock::new()).collect();
    let kept = Archived::Indexed(Box::new(index));
    Ok(Archive {
        kept,
        runner,
        loaded,
        tree,
        status,
        journal,
        budget: None,
        tell: None,
        items: Vec::new(),
        let_go: false,
        sealed: None,
    })
}

/// The payload `d` of an encrypted archive (decrypted as it is read) opened as [`open_payload`]
/// opens it whole, but decrypting at first only what tells where the blocks are and what they
/// are: every block but the data blocks, and of those their heads, the start and the end of
/// their data. A data block is decrypted as it is read in evaluating (the leaves, as
/// [`Archive::write_file`] and [`Archive::hashed`] evaluate them), and let go of once read,
/// decrypted again where it is read again: not all of the payload in memory at once.
pub fn open_decrypting<'c, 'n>(d: &'c envelope::Decrypting<'c>, runner: &'n dyn Runner) -> Decoded<Archive<'c, 'n>> {
    let p = Payload::Sealed(d);
    let (_, archive_end, _, _) = footer_fields(p)?;
    let mut at = HEADER_MAGIC.len();
    while at < archive_end {
        let (ty, data, next) = index::frame_at(p, at, archive_end)?;
        if matches!(ty, 0x20..=0x37 | 0x40..=0x4b) {
            // (a dictionary's index, a frame's header, a checksum)
            const ENDS: usize = 64;
            p.get(data.start..data.end.min(data.start + ENDS))?;
            p.get(data.end.saturating_sub(ENDS).max(data.start)..data.end)?;
        } else {
            p.get(data)?;
        }
        at = next;
    }
    // SAFETY: what is read of the payload but data blocks is got above; a data block is got as it
    // is read (`Contents::ensure`), and let go of after (`Contents::read`)
    let mut a = open_as(unsafe { d.unread() }, runner, true)?;
    a.sealed = Some(d);
    Ok(a)
}

/// [`open`] of an archive, the payload of an encrypted one where `sealed`.
fn open_as<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner, sealed: bool) -> Decoded<Archive<'c, 'n>> {
    open_with(c, runner, sealed, false, None)
}

/// [`open_as`], salvaging where `salvaging` ([`open_salvaging`]), the archive ended after the
/// segment whose metadata block is at `upto` where it is given ([`open_upto`]).
fn open_with<'c, 'n>(
    c: &'c [u8],
    runner: &'n dyn Runner,
    sealed: bool,
    salvaging: bool,
    upto: Option<usize>,
) -> Decoded<Archive<'c, 'n>> {
    let (blocks, tree, status, journal, _) = opened(c, sealed, salvaging, upto)?;
    let loaded = blocks.modules.iter().map(|_| OnceLock::new()).collect();
    let kept = Archived::Parsed(Box::new(blocks));
    Ok(Archive {
        kept,
        runner,
        loaded,
        tree,
        status,
        journal,
        budget: None,
        tell: None,
        items: Vec::new(),
        let_go: false,
        sealed: None,
    })
}

/// Of archive `p`, as its footer tells: where the footer's fields start, where the archive's
/// blocks end (its footer starts), its size hint and the backward offset of its last metadata
/// block.
pub(crate) fn footer_fields(p: Payload) -> Decoded<(usize, usize, u64, u64)> {
    if p.get(0..HEADER_MAGIC.len().min(p.len()))? != HEADER_MAGIC {
        return invalid("bad header magic");
    }
    let truncated = || Failure::Invalid("truncated footer".into());
    let copy = p.len().checked_sub(2).ok_or_else(truncated)?;
    let u16_of = |d: &[u8]| u16::from_le_bytes([d[0], d[1]]) as usize;
    let n = u16_of(p.get(copy..p.len())?);
    let (fields_start, archive_end) = copy
        .checked_sub(n)
        .and_then(|fields| Some((fields, fields.checked_sub(8).filter(|&end| end >= HEADER_MAGIC.len())?)))
        .ok_or_else(truncated)?;
    let c = p.get(archive_end..p.len())?;
    if c[..6] != FOOTER_MAGIC || u16_of(&c[6..8]) != n {
        return invalid("bad footer");
    }
    let fields = &c[8..c.len() - 2];
    let mut at = 0;
    let (size_hint, metadata) = (leb(fields, &mut at)?, leb(fields, &mut at)?);
    if at != fields.len() || size_hint != (archive_end - HEADER_MAGIC.len()) as u64 {
        return invalid("bad footer");
    }
    Ok((fields_start, archive_end, size_hint, metadata))
}

/// What [`opened`] gives: an archive's blocks, its tree, its status, its journal points and its
/// archive-wide members (the segments' merged).
pub(crate) type Opened<'c> =
    (Blocks<'c>, Option<(Tree, Vec<ContentRef>)>, Option<Status>, Vec<JournalPoint>, tree::ArchiveExtra);

/// The blocks of archive `c` (the payload of an encrypted one where `sealed`, salvaged where
/// `salvaging`), its tree, its status, its journal points and its archive-wide members: what
/// [`open_with`] opens, ended after the segment whose metadata block is at `upto` where it is
/// given.
pub(crate) fn opened(c: &[u8], sealed: bool, salvaging: bool, upto: Option<usize>) -> Decoded<Opened<'_>> {
    opened_with(c, sealed, salvaging, upto, None)
}

/// [`opened`], each segment read told too, in `segments` where it is given.
pub(crate) fn opened_with<'c>(
    c: &'c [u8],
    sealed: bool,
    salvaging: bool,
    upto: Option<usize>,
    mut segments: Option<&mut Vec<tree::SegmentInfo>>,
) -> Decoded<Opened<'c>> {
    let (fields_start, archive_end, size_hint, metadata) = footer_fields(Payload::Plain(c))?;
    // (salvaging an encrypted archive's payload, its blocks lost are spacer blocks, its size
    // hint block among them maybe: not required then)
    let mut blocks = parse(&c[HEADER_MAGIC.len()..archive_end], 0, size_hint, metadata != 0, sealed && !salvaging)?;
    if metadata != 0 {
        let at = usize::try_from(metadata).ok().and_then(|b| fields_start.checked_sub(b));
        if blocks.segments.last().and_then(|s| s.metadata.as_ref()).map(|m| m.0) != at {
            return invalid("no metadata block where the footer points");
        }
    }
    let outputs: Vec<u64> =
        blocks.leaves.iter().map(|l| if let LeafBlock::Formula { outputs, .. } = l { *outputs } else { 1 }).collect();
    let mut lost = 0;
    // each segment's tree, then merged (spec, "Interpreting an archive")
    // (the metadata blocks' data, no longer held once read)
    let metas: Vec<(usize, Vec<u8>)> = blocks
        .segments
        .iter_mut()
        .filter_map(|s| s.metadata.as_mut().map(|(at, d)| (*at, std::mem::take(d))))
        .collect();
    let mut trees = Vec::with_capacity(metas.len());
    // (where the metadata block before each is: the end of the header magic, less 1, before the first)
    let mut prev = HEADER_MAGIC.len() - 1;
    let mut status = None;
    let mut journal = Vec::new();
    let mut archive = tree::ArchiveExtra::default();
    let mut ended = upto.is_none();
    for (k, (at, d)) in metas.into_iter().enumerate() {
        let m = tree::read(&d)?;
        drop(d);
        if k == 0 && m.entries.is_empty() {
            return invalid("the first segment of no entries");
        }
        status = Some(m.status);
        journal.extend(m.journal.iter().map(|(time, label)| JournalPoint {
            metadata: at,
            time: *time,
            label: label.clone(),
        }));
        let seg = &blocks.segments[k];
        let (first, end) = (seg.first, seg.end);
        // (salvaging, those there those the first offsets tell: the others lost with what follows)
        let to = |offsets: &[u64], at_: &[usize]| {
            (offsets.len() == at_.len() || salvaging && offsets.len() > at_.len())
                && offsets.iter().zip(at_).all(|(&o, &b)| at.checked_sub(o as usize) == Some(b))
        };
        if !to(&m.modules, &blocks.module_at[first.modules..end.modules])
            || !to(&m.resources, &blocks.resource_at[first.resources..end.resources])
            || !to(&m.dictionaries, &blocks.dictionary_at[first.dictionaries..end.dictionaries])
        {
            return invalid("module, resource or dictionary offsets not those of the blocks");
        }
        // (a leaf of the segment's, by where it is: its outputs, and the dictionary of a file at it
        // checked)
        let mut leaf_at = |b: usize, dictionary: Option<u64>| -> Decoded<Option<(usize, Option<u64>)>> {
            let leaves = &blocks.leaf_at[first.leaves..end.leaves];
            let Some(leaf) = leaves.binary_search(&b).ok().map(|l| first.leaves + l) else { return Ok(None) };
            let own = blocks.leaf_dictionary[leaf].map_or(0, |d| (d - first.dictionaries) as u64 + 1);
            if dictionary.is_some_and(|d| d != own) {
                return invalid("a file's dictionary not that of its leaf block");
            }
            Ok(Some((leaf, Some(outputs[leaf]))))
        };
        let (told, points) = (m.status, segments.is_some().then(|| m.journal.clone()));
        let own = m.archive.clone();
        archive.merge(m.archive.clone());
        trees.push(tree_of(m, at, prev, &mut leaf_at, outputs.len(), salvaging.then_some(&mut lost), None)?);
        if let (Some(all), Some(journal)) = (segments.as_deref_mut(), points) {
            let tree = trees.last().expect("just pushed").clone();
            all.push(tree::SegmentInfo { tree, status: told, journal, archive: own });
        }
        prev = at;
        if upto == Some(at) {
            ended = true;
            break;
        }
    }
    if !ended {
        return invalid("no metadata block where the archive is to end");
    }
    let tree = match trees.len() {
        0 => None,
        1 => trees.pop().map(|t| (t.tree, t.refs)),
        _ => Some(tree::merge(trees)),
    };
    // (the leaves lost, after those there: never where they are, never with a dictionary)
    for _ in 0..lost {
        blocks.leaves.push(LeafBlock::Lost);
        blocks.leaf_at.push(usize::MAX);
        blocks.leaf_dictionary.push(None);
        blocks.sizes.leaves.push(0);
    }
    Ok((blocks, tree, status, journal, archive))
}

impl<'c, 'n> Archive<'c, 'n> {
    /// The journal points of the archive's segments (spec, "`journal`"), in the order of the
    /// segments and, in a segment, of the times. Of an archive opened for some paths alone, only
    /// those of the segments read.
    pub fn journal(&self) -> &[JournalPoint] {
        &self.journal
    }

    /// Where the metadata block of each segment is, from the archive's header magic, in the
    /// order of the segments (as [`JournalPoint::metadata`]: what [`open_upto`] takes); none
    /// where the archive has no metadata block.
    pub fn segment_metadata(&self) -> Decoded<Vec<usize>> {
        Ok(self.parsed()?.segments.iter().filter_map(|s| s.metadata.as_ref().map(|m| m.0)).collect())
    }

    /// The archive's blocks, all of them read (where they are not yet, now, as [`open`] reads
    /// them).
    pub(crate) fn parsed(&self) -> Decoded<&Blocks<'c>> {
        match &self.kept {
            Archived::Parsed(b) => Ok(b.as_ref()),
            Archived::Indexed(i) => i.parsed(self.tree.as_ref().map(|t| &t.1)),
        }
    }

    /// The tree, where the archive has a metadata block.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref().map(|t| &t.0)
    }

    /// What its last segment's metadata block tells of the writing it ends (`segment-status`;
    /// none where it has no metadata block): whether that writing completed, or what it left out.
    /// An archive whose footer was made again after a metadata block telling nothing may have been
    /// cut short after it, segments after it lost whole.
    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// Has leaves evaluated at once only while their weights fit in `budget` beside what the
    /// rest of the process holds of it (or one alone, of those of this evaluation), taken in
    /// their order but for those of long work on a thread, first, and those going before one
    /// waiting for room (`Gate`): the weight of a leaf is the most bytes its evaluation holds
    /// at once, as its formula tells (its literals and slices, the inputs and outputs of an
    /// application as it is made, and its memory limit). None by default.
    pub fn set_budget(&mut self, budget: &'n Budget) {
        self.budget = Some(budget);
    }

    /// Lets go of the pages of the archive's blocks once they are read (decoded) in evaluating
    /// ([`aqr_common::pages::let_go`]): an archive mapped from its file, read back to be verified,
    /// so that it is not all resident by the end.
    ///
    /// # Safety
    ///
    /// The archive's bytes are a mapping, read only, of a file (not memory of its own: pages
    /// let go of read as zeros there).
    pub unsafe fn let_go_read(&mut self) {
        self.let_go = true;
    }

    /// Tells `tell` how far the evaluation of the leaves is as it goes (none: tells nothing).
    pub fn on_progress(&mut self, tell: Option<Telling>) {
        self.tell = tell;
    }

    /// Has the evaluation of each leaf (by its index) told of as of item `items[i]` to
    /// [`aqr_common::trace`] (a content, say; none past them).
    pub fn set_items(&mut self, items: Vec<Option<usize>>) {
        self.items = items;
    }

    /// The number of contents of the tree's files.
    pub fn contents_len(&self) -> usize {
        self.tree.as_ref().map_or(0, |t| t.1.len())
    }

    /// The number of leaf blocks.
    pub fn leaves_len(&self) -> usize {
        match &self.kept {
            Archived::Parsed(b) => b.leaves.len(),
            Archived::Indexed(i) => i.leaves_len(),
        }
    }

    /// The bytes of content `c` of the tree, as its metadata block tells.
    pub fn content_size(&self, c: usize) -> Option<u64> {
        self.tree.as_ref()?.1.get(c).map(|r| r.2)
    }

    /// The leaf (by its index among the leaf blocks) of content `c` of the tree.
    pub fn leaf_of(&self, c: usize) -> Option<usize> {
        self.tree.as_ref()?.1.get(c).map(|r| r.0)
    }

    /// Evaluates every leaf, at once on up to `threads` threads, giving `f` each one's outputs
    /// (by its index among the leaf blocks) as it is evaluated. The first failure, in the order
    /// of the leaves, is returned (as is one of `f`); the others may have been given theirs.
    pub fn leaves(&self, threads: usize, f: impl Fn(usize, Vec<Vec<u8>>) -> Decoded<()> + Sync) -> Decoded<()> {
        self.each_leaf(threads, |i, r| f(i, r?))
    }

    /// [`Archive::leaves`], giving `f` each leaf's failure too (that of reading the formula
    /// blocks, before any is evaluated, is returned).
    pub fn each_leaf(
        &self,
        threads: usize,
        f: impl Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        self.each_leaf_into(threads, None, None, &f)
    }

    /// [`Archive::each_leaf`] of the leaves (by index) marked in `only`: the others not evaluated,
    /// nor their formula blocks read, nor the non-leaf blocks only they slice decoded.
    pub fn each_leaf_only(
        &self,
        threads: usize,
        only: &[bool],
        f: impl Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        self.each_leaf_into(threads, Some(only), None, &f)
    }

    /// The file of an archive of a single file, written to `sink` as it is evaluated: of a
    /// single leaf, a formula's output is not held whole, but written as its pieces are pulled
    /// (`eval::run`). Returns its length.
    pub fn write_file(&self, threads: usize, sink: &mut Sink) -> Decoded<u64> {
        if self.tree.is_some() {
            return invalid("an archive of a tree");
        }
        let mut n = 0u64;
        if self.parsed()?.leaves.len() != 1 {
            let Value::File(d) = self.value(threads)? else { unreachable!("an archive of a single file") };
            sink(&d)?;
            return Ok(d.len() as u64);
        }
        let mut counted = |d: &[u8]| {
            n += d.len() as u64;
            sink(d)
        };
        let streaming: Mutex<&mut Sink> = Mutex::new(&mut counted);
        self.each_leaf_into(threads, None, Some(&streaming), &|_, outputs| {
            let mut sink = streaming.lock().unwrap();
            outputs?.iter().try_for_each(|d| sink(d))
        })?;
        Ok(n)
    }

    /// The content hashes of the outputs of an archive without a metadata block, in order (of
    /// the leaves, and of each leaf's outputs), each with the checksums its formula block takes
    /// of it: [`Archive::hashed_leaves`], None where any leaf's is not told.
    pub fn hashed(
        &self,
        records: &dyn crate::chash::Records,
        identify: &dyn Fn(&[u8]) -> Option<String>,
        threads: usize,
    ) -> Decoded<Option<Vec<crate::chash::Hashed>>> {
        if self.tree.is_some() {
            return invalid("an archive of a tree");
        }
        let leaves = self.hashed_leaves(records, identify, threads)?;
        Ok(leaves.into_iter().collect::<Option<Vec<_>>>().map(|l| l.concat()))
    }

    /// The content hashes of each leaf's outputs, each with the checksums its formula block takes
    /// of it, worked out without running a module where `records` tell the applications' outputs
    /// by the hashes of their inputs (of their modules as `identify` tells them by their Wasm
    /// modules), the blocks decoded on up to `threads` threads: see `eval`'s `hashed`. None
    /// for a leaf whose are not told so.
    pub fn hashed_leaves(
        &self,
        records: &dyn crate::chash::Records,
        identify: &dyn Fn(&[u8]) -> Option<String>,
        threads: usize,
    ) -> Decoded<Vec<Option<Vec<crate::chash::Hashed>>>> {
        use crate::chash::Ch;
        let blocks = self.parsed()?;
        let mut used = vec![false; blocks.non_leaf.len()];
        let env = |leaf: &LeafBlock| blocks.env(leaf, &self.loaded, self.runner);
        let formulas: Vec<Option<Vec<u8>>> = (blocks.leaves.iter())
            .map(|leaf| match *leaf {
                LeafBlock::Formula { data, .. } => {
                    let (d, dict) = blocks.formulas[data];
                    decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d])).map(Some)
                }
                _ => Ok(None),
            })
            .collect::<Decoded<_>>()?;
        let programs: Vec<Option<Program>> = (blocks.leaves.iter().zip(&formulas))
            .map(|(leaf, d)| match (leaf, d) {
                (&LeafBlock::Formula { outputs, .. }, Some(d)) => {
                    let (env, end) = env(leaf);
                    read(d, outputs, &env, end, &mut used).map(Some)
                }
                _ => Ok(None),
            })
            .collect::<Decoded<_>>()?;
        let mut refs = vec![0; blocks.non_leaf.len()];
        for p in programs.iter().flatten() {
            for insn in &p.insns {
                if let &Insn::Slice { block, .. } = insn {
                    refs[p.blocks[block]] += 1;
                }
            }
        }
        let tally = Tally { tell: None, decompressed: AtomicU64::new(0), evaluated: AtomicU64::new(0), totals: (0, 0) };
        let contents = Contents {
            need: vec![u64::MAX; refs.len()],
            refs: refs.into_iter().map(AtomicUsize::new).collect(),
            data: blocks.non_leaf.iter().map(|&(d, zstd, dict)| Waited::new(Slot::Raw(d, zstd, dict))).collect(),
            dictionaries: &blocks.dictionaries,
            tally: &tally,
            let_go: self.let_go,
            sealed: self.sealed,
        };
        let mut out = Vec::new();
        for (leaf, p) in blocks.leaves.iter().zip(&programs) {
            out.push(match (leaf, p) {
                (&LeafBlock::Stored(raw, zstd, checked, dict), _) => {
                    contents.ensure(raw)?;
                    let d = decoded_with(raw, zstd, checked, dict.map(|d| &blocks.dictionaries[d]))?;
                    contents.read(raw);
                    Some(vec![(Ch::of(&d), Vec::new())])
                }
                (LeafBlock::Formula { .. }, Some(p)) => eval::hashed(
                    p,
                    &env(leaf).0,
                    &contents,
                    &blocks.non_leaf,
                    &blocks.dictionaries,
                    records,
                    identify,
                    threads,
                )?,
                _ => return invalid("a file whose block is lost"),
            });
        }
        Ok(out)
    }

    /// How far the archive conforms to the archival profile (spec, "Archival profile"), as its
    /// blocks tell: its leaf data blocks without a checksum, and its formula blocks with an output
    /// no Checksum instruction covers (every formula block read). An archive opened is standalone
    /// (a dictionary shorthand block, which is not supported yet, is not opened at all).
    pub fn archival(&self) -> Decoded<Archival> {
        let blocks = self.parsed()?;
        let mut a = Archival::default();
        let mut used = vec![false; blocks.non_leaf.len()];
        for leaf in &blocks.leaves {
            match *leaf {
                LeafBlock::Stored(_, _, checked, _) => a.unchecked_leaves += !checked as usize,
                LeafBlock::Lost | LeafBlock::Unread | LeafBlock::Bad(_) => {}
                LeafBlock::Formula { outputs, data, .. } => {
                    let (d, dict) = blocks.formulas[data];
                    let d = decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d]))?;
                    let (env, end) = blocks.env(leaf, &self.loaded, self.runner);
                    let p = read(&d, outputs, &env, end, &mut used)?;
                    a.unchecked_formulas += !p.outputs_checked() as usize;
                }
            }
        }
        Ok(a)
    }

    /// [`Archive::each_leaf`], a leaf of a formula written to `sink` as it is evaluated where
    /// it is given ([`eval::run`]): `f` given no output of it then. Where `only` is given, the
    /// leaves (by index) not marked there are not evaluated, nor their formula blocks read, nor
    /// the non-leaf blocks only they slice decoded.
    pub(crate) fn each_leaf_into(
        &self,
        threads: usize,
        only: Option<&[bool]>,
        sink: Option<&Mutex<&mut Sink>>,
        f: &(dyn Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync),
    ) -> Decoded<()> {
        // (of an archive indexed, those of the leaves taken alone read)
        let partial;
        let blocks = match (&self.kept, only) {
            (Archived::Indexed(i), Some(o)) => {
                let picked: Vec<usize> = (0..i.leaves_len()).filter(|&l| o.get(l) == Some(&true)).collect();
                partial = i.blocks_for(&picked);
                &partial
            }
            _ => self.parsed()?,
        };
        // the leaves evaluated, in order
        let taken = |i: usize| only.is_none_or(|o| o.get(i) == Some(&true));
        let picked: Vec<usize> = (0..blocks.leaves.len()).filter(|&i| taken(i)).collect();
        // the formula blocks of those read first (every one where all are, the archive checked
        // then for non-leaf blocks no formula refers to): a non-leaf block is dropped once nothing
        // is to slice it
        let mut used = vec![false; blocks.non_leaf.len()];
        let env = |leaf: &LeafBlock| blocks.env(leaf, &self.loaded, self.runner);
        let formulas = blocks
            .leaves
            .iter()
            .enumerate()
            .map(|(i, leaf)| match *leaf {
                LeafBlock::Formula { data, .. } if taken(i) => {
                    match blocks.decoded_formulas.get(data).and_then(Option::as_deref) {
                        Some(d) => Ok(Some(std::borrow::Cow::Borrowed(d))),
                        None => {
                            let (d, dict) = blocks.formulas[data];
                            decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d]))
                                .map(|d| Some(std::borrow::Cow::Owned(d)))
                        }
                    }
                }
                _ => Ok(None),
            })
            .collect::<Vec<Decoded<_>>>();
        let programs = blocks
            .leaves
            .iter()
            .zip(&formulas)
            .map(|(leaf, d)| match (leaf, d) {
                (&LeafBlock::Formula { outputs, .. }, Ok(Some(d))) => {
                    let (env, end) = env(leaf);
                    read(d, outputs, &env, end, &mut used).map(Some)
                }
                (_, Err(e)) => Err(e.clone()),
                _ => Ok(None),
            })
            .collect::<Vec<Decoded<_>>>();
        // (a formula block that does not read is its leaf's failure, the others evaluated as they
        // are: a file damaged loses itself alone)
        let mut failed: Vec<Option<Failure>> = Vec::with_capacity(programs.len());
        let programs: Vec<Option<Program>> = programs
            .into_iter()
            .map(|p| {
                failed.push(p.as_ref().err().cloned());
                p.ok().flatten()
            })
            .collect();
        // (a non-leaf block of a formula block failing or lost not referenced)
        if failed.iter().any(Option::is_some) || blocks.leaves.iter().any(|l| matches!(l, LeafBlock::Lost)) {
            used.fill(true);
        }
        if only.is_none() && used.contains(&false) {
            return invalid("a non-leaf block no formula block references");
        }
        let mut refs = vec![0; blocks.non_leaf.len()];
        // (of some contents alone, each block decoded only as far as their slices reach: all of
        // it where every content is taken, for its checksum to be checked)
        let mut need = vec![if only.is_some() { 0 } else { u64::MAX }; blocks.non_leaf.len()];
        for p in picked.iter().filter_map(|&i| programs[i].as_ref()) {
            for insn in &p.insns {
                if let &Insn::Slice { block, start, len } = insn {
                    refs[p.blocks[block]] += 1;
                    let n = &mut need[p.blocks[block]];
                    *n = (*n).max(start.saturating_add(len));
                }
            }
        }
        // by leaf, its weight, the bytes of its outputs and of its work on a thread
        let slots: Vec<u64> = blocks.modules.iter().map(|m| m.wasm(self.runner).map_or(0, task_slots)).collect();
        let (mut weights, mut lens, mut serial) = (Vec::new(), Vec::new(), Vec::new());
        for (leaf, p) in blocks.leaves.iter().zip(&programs) {
            let (w, l, t) = match (leaf, p) {
                (&LeafBlock::Stored(d, zstd, checked, _), _) => {
                    let n = stored_len(d, zstd, checked);
                    (n, n, 0)
                }
                (&LeafBlock::Formula { modules, segment, .. }, Some(p)) => {
                    p.weight(&slots[blocks.segments[segment].first.modules..modules])
                }
                _ => (0, 0, 0),
            };
            weights.push(w);
            lens.push(l);
            serial.push(t);
        }
        // the leaves of long work on a thread first, the longest first, for it to be done beside
        // the others; the others in their order
        // (by their places among those picked)
        let long_serial = LONG_SERIAL.get();
        let long = |k: &usize| serial[picked[*k]] >= long_serial;
        let mut order: Vec<usize> = (0..picked.len()).filter(long).collect();
        order.sort_by_key(|&k| std::cmp::Reverse(serial[picked[k]]));
        order.extend((0..picked.len()).filter(|k| !long(k)));
        let stored = picked.iter().map(|&i| match blocks.leaves[i] {
            LeafBlock::Stored(d, ..) => d.len() as u64,
            _ => 0,
        });
        let sliced = blocks.non_leaf.iter().zip(&refs).filter(|(_, n)| **n > 0).map(|((d, ..), _)| d.len() as u64);
        let data_len = stored.sum::<u64>() + sliced.sum::<u64>();
        let tally = Tally {
            tell: self.tell.as_deref(),
            decompressed: AtomicU64::new(0),
            evaluated: AtomicU64::new(0),
            totals: (data_len, picked.iter().fold(0u64, |a, &i| a.saturating_add(lens[i]))),
        };
        tally.add(0, 0);
        let contents = Contents {
            need,
            refs: refs.into_iter().map(AtomicUsize::new).collect(),
            data: blocks.non_leaf.iter().map(|&(d, zstd, dict)| Waited::new(Slot::Raw(d, zstd, dict))).collect(),
            dictionaries: &blocks.dictionaries,
            tally: &tally,
            let_go: self.let_go,
            sealed: self.sealed,
        };
        let weights: Vec<u64> = picked.iter().map(|&i| weights[i]).collect();
        let gate = Gate::new(self.budget, &weights, order);
        let programs: Vec<Mutex<Option<Program>>> = programs.into_iter().map(Mutex::new).collect();
        // each leaf on a worker of its own, which is given back as it is done, for those still
        // running to take for their own work (a stream that takes long alone)
        let done = aqr_common::pool::dispatch_any(
            picked.len(),
            threads,
            || {
                let _w = aqr_common::trace::wait("budget");
                gate.next()
            },
            |k, _admitted| {
                let i = picked[k];
                let (_s, _i) =
                    (aqr_common::trace::span("leaf"), aqr_common::trace::item(self.items.get(i).copied().flatten()));
                tally.step(Step::Started(i));
                let outputs = match blocks.leaves[i] {
                    _ if failed[i].is_some() => Err(failed[i].clone().expect("a failure")),
                    LeafBlock::Lost => invalid("a file whose block is lost"),
                    LeafBlock::Bad(ref e) => Err(e.clone()),
                    LeafBlock::Unread => unreachable!("a leaf taken"),
                    LeafBlock::Stored(raw, zstd, checked, dict) => {
                        let d = contents
                            .ensure(raw)
                            .and_then(|()| decoded_with(raw, zstd, checked, dict.map(|d| &blocks.dictionaries[d])));
                        contents.read(raw);
                        tally.add(raw.len() as u64, 0);
                        d.map(|d| vec![d])
                    }
                    ref leaf @ LeafBlock::Formula { .. } => {
                        let p = programs[i].lock().unwrap().take().expect("each leaf once");
                        let env = env(leaf).0;
                        match sink {
                            Some(sink) => eval::run(p, &env, &contents, Some(&mut **sink.lock().unwrap()), threads),
                            None => eval::run(p, &env, &contents, None, threads),
                        }
                    }
                };
                tally.add(0, lens[i]);
                let r = f(i, outputs);
                tally.step(Step::Ended(i));
                r
            },
        );
        done.into_iter().collect()
    }

    /// What the archive holds, its leaves evaluated as [`Archive::leaves`] does.
    pub fn value(&self, threads: usize) -> Decoded<Value> {
        match self.tree() {
            None => {
                let leaves: Mutex<Vec<Option<Vec<Vec<u8>>>>> =
                    Mutex::new((0..self.leaves_len()).map(|_| None).collect());
                self.leaves(threads, |i, outputs| {
                    leaves.lock().unwrap()[i] = Some(outputs);
                    Ok(())
                })?;
                // the first leaf as it is
                let mut out = Vec::new();
                for d in leaves.into_inner().unwrap().into_iter().flatten().flatten() {
                    if out.is_empty() {
                        out = d;
                    } else {
                        out.extend_from_slice(&d);
                    }
                }
                Ok(Value::File(out))
            }
            Some(t) => {
                let files: Mutex<Vec<Vec<u8>>> = Mutex::new(vec![Vec::new(); self.contents_len()]);
                self.contents(threads, |c, d| {
                    files.lock().unwrap()[c] = d;
                    Ok(())
                })?;
                Ok(Value::Tree(Box::new(t.clone()), files.into_inner().unwrap()))
            }
        }
    }

    /// Evaluates every leaf as [`Archive::leaves`] does, giving `f` each content of the tree
    /// (by [`EntryNode::File`]) once, as it is evaluated; checked against the size of its files.
    pub fn contents(&self, threads: usize, f: impl Fn(usize, Vec<u8>) -> Decoded<()> + Sync) -> Decoded<()> {
        self.contents_of(threads, None, f)
    }

    /// [`Archive::contents`] of the contents marked in `only` alone (by content; all where
    /// `None`): only the leaves of those evaluated, and only the non-leaf blocks they slice
    /// decoded. The other contents of those leaves are not given.
    pub fn contents_of(
        &self,
        threads: usize,
        only: Option<&[bool]>,
        f: impl Fn(usize, Vec<u8>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        let Some((_, refs)) = &self.tree else { return invalid("an archive of a single file") };
        let wanted = |c: usize| only.is_none_or(|o| o.get(c) == Some(&true));
        // by leaf, its outputs taken: output, content, size
        let mut of: Vec<Vec<(usize, usize, u64)>> = vec![Vec::new(); self.leaves_len()];
        refs.iter().enumerate().filter(|&(c, _)| wanted(c)).for_each(|(c, &(l, o, n))| of[l].push((o, c, n)));
        let leaves: Option<Vec<bool>> = only.map(|_| of.iter().map(|o| !o.is_empty()).collect());
        self.each_leaf_into(threads, leaves.as_deref(), None, &|i, outputs| {
            let mut outputs = outputs?;
            for &(o, c, n) in &of[i] {
                let Some(d) = outputs.get_mut(o).map(std::mem::take) else {
                    return invalid("a file or a fork of an output its leaf block does not have");
                };
                if d.len() as u64 != n {
                    return invalid("a file of another size than its entry's");
                }
                f(c, d)?;
            }
            Ok(())
        })
    }
}

/// What tells the leaf block at a place of an archive ([`tree_of`]): its index and its outputs
/// (where they are known), a file's dictionary (1-based in its segment) given to be checked; none
/// where none is there.
type LeafAt<'a> = dyn FnMut(usize, Option<u64>) -> Decoded<Option<(usize, Option<u64>)>> + 'a;

/// The tree of metadata block `m` at `at`, the metadata block before it at `prev` (the end of the
/// header magic, less 1, where it is the first), `leaf_at` telling the leaf block at a place (by
/// its index, and its outputs where they are known; a file's dictionary given to be checked),
/// none where none is there; and its contents, each a leaf and an output of it, and the size of
/// the files of it. Salvaging (`lost`), the leaves not there are numbered from `lost_from`. Of
/// the entries `kept` alone (by number from the root, each with its parent) where it is given: the
/// others left out, and not checked (spec, "Partial access").
pub(crate) fn tree_of(
    m: tree::Metadata,
    at: usize,
    prev: usize,
    leaf_at: &mut LeafAt,
    lost_from: usize,
    mut lost: Option<&mut usize>,
    kept: Option<&[bool]>,
) -> Decoded<tree::SegmentTree> {
    let kept = |k: usize| kept.is_none_or(|w| w.get(k) == Some(&true));
    if m.prev != (at - prev) as u64 {
        return invalid(match prev {
            p if p < HEADER_MAGIC.len() => "a first metadata block not pointing to the header",
            _ => "a metadata block not pointing to the one before it",
        });
    }
    let mut refs: Vec<(usize, usize, u64)> = Vec::new();
    let mut ref_of: HashMap<(usize, usize), usize> = HashMap::new();
    // the content of output `output` of the leaf block at relative offset `offset`, of `size`
    // bytes; and the leaf
    // (salvaging, the leaves lost by where they were, numbered after those there)
    let mut lost_at: HashMap<usize, usize> = HashMap::new();
    let mut content = |offset: u64, output: u64, size: u64, dictionary: Option<u64>| -> Decoded<(usize, usize)> {
        let found = match at.checked_sub(offset as usize) {
            Some(b) => leaf_at(b, dictionary)?,
            None => None,
        };
        let (leaf, outputs) = match (found, lost.as_deref_mut()) {
            (Some(found), _) => found,
            (None, Some(lost)) => {
                let k = *lost_at.entry(at.wrapping_sub(offset as usize)).or_insert_with(|| {
                    *lost += 1;
                    *lost - 1
                });
                (lost_from + k, None)
            }
            (None, None) => return invalid("a file or a fork not at a leaf block"),
        };
        if leaf >= lost_from {
            let c = match ref_of.entry((leaf, output as usize)) {
                MapEntry::Occupied(c) => *c.get(),
                MapEntry::Vacant(e) => {
                    refs.push((leaf, output as usize, size));
                    *e.insert(refs.len() - 1)
                }
            };
            return Ok((c, leaf));
        }
        if outputs.is_some_and(|n| output >= n) {
            return invalid("a file or a fork of an output its leaf block does not have");
        }
        let output = output as usize;
        let c = match ref_of.entry((leaf, output)) {
            MapEntry::Occupied(c) if refs[*c.get()].2 == size => *c.get(),
            MapEntry::Occupied(_) => return invalid("files or forks of an output of other sizes"),
            MapEntry::Vacant(e) => {
                refs.push((leaf, output, size));
                *e.insert(refs.len() - 1)
            }
        };
        Ok((c, leaf))
    };
    let mut extras = m.extras;
    let mut t = Tree { root: std::mem::take(&mut extras[0]), entries: Vec::new() };
    // (whether the root and each entry of the tree has an entry extra, not null)
    let mut given = vec![m.given[0]];
    let mut tombstones = Vec::new();
    // the tree's index of each entry by number, tombstones left out
    let mut index = vec![None; m.entries.len() + 1];
    let mut names = HashSet::with_capacity(m.entries.len());
    if !m
        .entries
        .iter()
        .enumerate()
        .filter(|(k, _)| kept(k + 1))
        .all(|(_, (parent, name, _))| names.insert((*parent, &name[..])))
    {
        return invalid("two entries of a name in a directory");
    }
    drop(names);
    for (k, (parent, name, read)) in m.entries.into_iter().enumerate() {
        let k = k + 1;
        if !kept(k) {
            continue;
        }
        let parent_index = match parent {
            0 => None,
            p => Some(index[p].expect("a directory")),
        };
        let node = match read {
            tree::Read::Dir => EntryNode::Dir,
            tree::Read::File { offset, output, size, dictionary } => {
                let (c, _) = content(offset, output, size, Some(dictionary))?;
                EntryNode::File(c)
            }
            tree::Read::Link(target) => EntryNode::Link(target),
            tree::Read::Fifo => EntryNode::Fifo,
            tree::Read::Device { block, major, minor } => EntryNode::Device { block, major, minor },
            tree::Read::Special(0) => {
                if m.given[k] {
                    return invalid("a tombstone of an extra");
                }
                tombstones.push((parent_index, name));
                continue;
            }
            tree::Read::Special(kind) => EntryNode::Special(kind),
        };
        index[k] = Some(t.entries.len());
        given.push(m.given[k]);
        t.entries.push(Entry { parent: parent_index, name, node, extra: std::mem::take(&mut extras[k]) });
    }
    // (after the files, so that their contents are numbered as they are)
    for (k, forks) in m.forks.into_iter().enumerate().filter(|(k, _)| kept(*k)) {
        for (name, (offset, output, size)) in forks {
            let c = content(offset, output, size, None)?.0;
            match k {
                0 => &mut t.root,
                k => &mut t.entries[index[k].expect("not a tombstone")].extra,
            }
            .forks
            .push((name, c));
        }
    }
    Ok(tree::SegmentTree { tree: t, refs, given, tombstones })
}
