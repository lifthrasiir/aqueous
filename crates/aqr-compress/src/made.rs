//! Data recompressed by its format ([`recompress_at`], [`made`]): the recompressors of the
//! formats, what they make ([`Made`]) and what is undone where it does not pay.

use super::*;

/// [`recompress`] for data `depth` formats deep, within `cx`.
///
/// A file of `kind` is recompressed as [`Options::file`] and the kind tell ([`Nest::of`]), as a
/// file in an archive of many is, but for a format: that is looked for as in a file of no kind,
/// as a scan finds the file the data starts with, and what is in it where that is turned down
/// (which [`Nest::As`] does only by [`regions`]). Data kept raw is of the kind, or of
/// [`Kind::Text`] where it is text (as [`made`] tells), so archived as text.
pub(crate) fn recompress_at(d: &[u8], opt: &Options, depth: u32, kind: Kind, cx: &Cx) -> choose::Settled {
    let how = match kind {
        Kind::Format(_) => Some(opt.file),
        kind => opt.file.of(kind),
    };
    // (of a file alone, formulas within it that lose only by the blocks they take kept beside
    // their data, to be chosen together; not the file's own, which is kept as it is compared,
    // but a scan's whose files were all kept beside theirs: `(0, len)`, [`scanned`])
    let alone = !cx.enc.shares_modules() && DEFER_MODULES.get();
    let made = |how| made_as(d, opt, depth, how, cx, alone).filter(|m| m.close.is_none_or(|c| c == (0, d.len())));
    how.and_then(made).map_or_else(
        || {
            let text = how == Some(Nest::As(Format::Text))
                || matches!(kind, Kind::Unknown | Kind::Format(_)) && is_text(detect(d));
            let mut f = Formula::default();
            f.root = f.push_as("raw", if text { Kind::Text } else { kind }, d.to_vec());
            choose::kept(f)
        },
        |m| {
            let f = m.cut(d);
            // (of a file alone, those that pay for the blocks they take together made; else
            // nothing chooses here: the data kept, as it was compared)
            match alone {
                true => choose::by_blocks(f, &cx.enc).0,
                false => choose::kept(f),
            }
        },
    )
}

/// [`recompress_at`], unless `d` is kept raw; the data that a scan keeps between files is not
/// cut out of `d` yet.
pub(crate) fn made(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    made_as(d, opt, depth, how, cx, false)
}

/// [`made`] of a content whose formula, where it loses to the data by little, is kept beside it
/// ([`Made::close`]; its files found by a scan alike, [`find`]): for its parent to put both in
/// place ([`place_slices`]), and the batch to choose.
pub(crate) fn made_deferring(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    made_as(d, opt, depth, how, cx, true)
}

fn made_as(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx, defers: bool) -> Option<Made> {
    let outer = cx.defers.replace(defers);
    let m = made_alone(d, opt, depth, how, cx);
    cx.defers.set(outer);
    // (where `d` is part of a file, that part is done)
    aqr_common::progress::done(d);
    m
}

/// `d` through the recompressor of `format` ([`recompress_as`]), its formula kept beside it
/// where it loses by little and that may be ([`Cx::defers`], [`Made::close`]).
fn made_by(format: Format, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    let id = (d.as_ptr() as usize, d.len());
    let outer = cx.defer_for.replace(cx.defers.get().then_some(id));
    let f = recompress_as(format, d, opt, depth, cx);
    cx.defer_for.set(outer);
    let close = cx.closed.take().filter(|c| c.0 == id).map(|c| c.1);
    f.map(|f| Made { f, gaps: None, close })
}

pub(crate) fn made_alone(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    // (the data of a DEFLATE stream archived along with it, kept as it is: made of it)
    if depth < opt.depth
        && let Some(f) = cx.bodies.as_ref().and_then(|b| b.formula(d))
    {
        return Some(Made::from(f));
    }
    match how {
        _ if depth >= opt.depth || !opt.recompresses() => None,
        // a file of no header taken apart by another archived along (a VPK's archive by its
        // directory file, a resource file by its serialized file), whatever its first bytes are
        _ if let Some(format) = siblings::format(d, cx) => made_by(format, d, opt, depth, cx),
        Nest::Hint(i) => crate::hints::made(i, d, opt, depth, cx),
        Nest::Exact => detect(d).and_then(|format| made_by(format, d, opt, depth, cx)),
        // text with base64 or hexadecimal blocks or quoted-printable bodies where aqr-b2t takes
        // them (and it is [`smaller`]), of any length: the text without them and each block's
        // bytes are made as what they are (the text left by one is looked at by the other, a
        // level deeper: a mail of both)
        _ if matches!(how, Nest::As(Format::Text) | Nest::Scan)
            && let Some(f) = b2t(d, opt, depth, cx).or_else(|| quopri(d, opt, depth, cx)) =>
        {
            Some(Made::from(f))
        }
        // JSON lines or a table where aqr-jsonl or aqr-xsv takes it (and it is [`smaller`]);
        // else text as aqr-wrt tells of it alone (not [`smaller`]), so that the same text is
        // taken alike wherever it is; else scanned as other data is, or kept raw where named as
        // text
        _ if how == Nest::As(Format::Text) || how == Nest::Scan && is_text(detect(d)) => {
            match table(d, opt, depth, cx).or_else(|| recompress_as(Format::Text, d, opt, depth, cx)) {
                Some(f) => Some(Made::from(f)),
                None if how == Nest::Scan => {
                    let (m, close) = scanned(scan(d, None, opt, depth, cx), d, opt, cx);
                    m.or(close)
                }
                None => None,
            }
        }
        // else its samples after a header telling their size, or its tables of records, if any
        Nest::Scan => {
            let (m, close) = scanned(scan(d, None, opt, depth, cx), d, opt, cx);
            m.or_else(|| recompress_as(Format::Sized, d, opt, depth, cx).map(Made::from))
                .or_else(|| records(d, opt, depth, cx))
                .or(close)
        }
        Nest::As(format) => {
            let format = detect::named(format, d);
            let (m, close) = scanned(scan(d, Some(format), opt, depth, cx), d, opt, cx);
            m.or(close)
        }
    }
}

/// The formula of a scan of `d` where it is [`smaller`] (its alternatives sized with their data
/// kept); else, where files it found were kept beside their data ([`find`]), all of it kept
/// beside the data ([`Made::close`]), for the batch to choose where nothing else takes `d`. So is
/// one whose files were all kept beside their data, not compared: with them all kept it is the
/// data, which would otherwise be taken or not by a byte, and where taken be the scan's data
/// between files (of its class `raw`), not the data as its parent keeps it (10gb/progs: a PNG in
/// a ZIP file, in `zip/png` as it was turned down, its ZIP file taken then, 124 KB more).
pub(crate) fn scanned(m: Option<Made>, d: &[u8], opt: &Options, cx: &Cx) -> (Option<Made>, Option<Made>) {
    let Some(m) = m else { return (None, None) };
    let only_kept = !m.f.eithers().is_empty() && slices_only(&m.f.chosen(false), 0);
    if !only_kept && smaller(&m.f, m.gaps.as_deref(), d, opt, cx) {
        return (Some(m), None);
    }
    cx.lost.set(true);
    let close = (cx.defers.get() && !m.f.eithers().is_empty()).then(|| Made { close: Some((0, d.len())), ..m });
    (None, close)
}

/// Whether formula `f` is slices of stream `s` only.
fn slices_only(f: &Formula, s: usize) -> bool {
    let mut only = f.shared.is_empty();
    f.root.visit(&mut |n| match n {
        &Node::Slice { stream, .. } => only &= stream == s,
        Node::Concat(_) => {}
        _ => only = false,
    });
    only
}

/// `d`, data in which a scan finds no file, as its tables of records and chains of entries
/// where aqr-infer-recs takes them (a quick estimate telling they may save 5% at least) and the
/// formula is [`smaller`].
fn records(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    recompress_as(Format::Records, d, opt, depth, cx).map(Made::from)
}

/// Whether data detected as `format` is text: taken as a table, else as text ([`made`]).
fn is_text(format: Option<Format>) -> bool {
    format.is_some_and(|f| f.spec().text)
}

/// `d`, text of any length, as the text without its base64 and hexadecimal blocks and the bytes
/// they decode to where aqr-b2t finds blocks (a MIME message's attachments, PEM, data URIs,
/// base64 in JSON, RTF's pictures, SQL dumps' blobs) and the formula is [`smaller`]: each is
/// recompressed as what it is. Asked of every text, so that [`aqr_b2t::formula::starts`] is a
/// quick look; the text path (aqr-wrt) takes only that of [`aqr_wrt::formula::MIN_LEN`] bytes,
/// and a message with an attachment is often short.
fn b2t(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    // (mostly_text first: it gives up on a binary file within its first pieces, `starts` would
    // go over all of it)
    (aqr_wrt::formula::mostly_text(d) && aqr_b2t::formula::starts(d))
        .then(|| recompress_as(Format::B2t, d, opt, depth, cx))
        .flatten()
}

/// `d`, text, as the text without its quoted-printable bodies and the bytes they decode to
/// where aqr-b2t finds bodies (a header names the encoding) and the formula is [`smaller`].
fn quopri(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    (aqr_wrt::formula::mostly_text(d) && aqr_b2t::quopri::formula::starts(d))
        .then(|| recompress_as(Format::Quopri, d, opt, depth, cx))
        .flatten()
}

/// `d`, text, as JSON lines if aqr-jsonl takes it, else as a table if aqr-xsv does or as lines
/// of templates and fields if aqr-log does ([`log_or_table`]), where the formula is
/// [`smaller`].
fn table(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let jsonl = aqr_jsonl::formula::starts(d).then(|| recompress_as(Format::Jsonl, d, opt, depth, cx)).flatten();
    jsonl.or_else(|| {
        let xsv = aqr_xsv::formula::starts(d);
        let format = match aqr_log::formula::starts(d) {
            true => log_or_table(d, xsv)?,
            false if xsv => Format::Xsv,
            false => return None,
        };
        recompress_as(format, d, opt, depth, cx)
    })
}

aqr_common::zopt! {
    /// The bytes of a window of the sample that aqr-log, aqr-xsv and the text path are compared on
    /// ([`table_sample`]).
    pub(crate) static TABLE_WINDOW: Opt<usize> =
        Opt::size("table:window", 256 << 10, "bytes of a window of the sample that log, xsv and text are compared on");
    /// The number of windows of that sample ([`table_sample`]).
    pub(crate) static TABLE_WINDOWS: Opt<usize> =
        Opt::value("table:windows", 8, "windows of the sample that log, xsv and text are compared on");
    /// The level of zstd that the sample's streams are compared at ([`log_or_table`]).
    static TABLE_ZSTD_LEVEL: Opt<i32> =
        Opt::value("table:zstd-level", 3, "zstd level that log, xsv and text are compared at");
    /// aqr-log takes text over the text path (aqr-wrt) where its streams on the sample, compressed
    /// by zstd -3, are at most this many percent of the sample compressed so. Prose and source code,
    /// whose words vary and fill the columns of templates of every length, come out at 120% and
    /// more; lines that repeat their shape, which the text path takes as words only, come out at
    /// 100% and less. On the 10gb corpus (files' archived bytes, as `--stats` tells them): at 85%
    /// 156 files took aqr-log, -10.8 MB in all; at 100% 272, -11.9 MB, the 116 more -1.1 MB.
    static LOG_PERCENT: Opt<usize> =
        Opt::value("log:percent", 100, "percent of the text path's size under which aqr-log takes text");
}

/// The sample of text `d` that aqr-log and aqr-xsv are compared on: all of it up to
/// [`TABLE_WINDOWS`] windows of [`TABLE_WINDOW`] bytes, else that many windows spread from the
/// start of `d` to its end, each from a line's start to a line's end, one after the other.
/// The start alone told nothing of what a file does later: of the first MiB of the SMARD price
/// table, 261 bytes are streams (zeros), and aqr-xsv was 0.7% ahead on that of a load table that
/// archives 7.7% larger with it than with aqr-log.
pub(crate) fn table_sample(d: &[u8]) -> Cow<'_, [u8]> {
    let (window, windows) = (TABLE_WINDOW.get(), TABLE_WINDOWS.get());
    if d.len() <= window * windows {
        return Cow::Borrowed(d);
    }
    let mut s = Vec::with_capacity(window * windows);
    for i in 0..windows {
        let at = (d.len() - window) * i / (windows - 1);
        // (from the next line's start, unless at the start of `d`; to the last whole line)
        let from = match at {
            0 => 0,
            _ => d[at - 1..].iter().position(|&b| b == b'\n').map_or(d.len(), |j| at + j),
        };
        let to = (at + window).min(d.len());
        if from < to
            && let Some(j) = d[from..to].iter().rposition(|&b| b == b'\n')
        {
            s.extend_from_slice(&d[from..from + j + 1]);
        }
    }
    Cow::Owned(s)
}

/// The format for text `d` that aqr-log takes (its `starts`), `xsv` whether aqr-xsv does too:
/// of a sample of `d` ([`table_sample`]) each candidate's streams as zstd -3 compresses them, and
/// the sample itself for the text path. aqr-xsv where it is no larger than aqr-log; aqr-log
/// where it is at most [`LOG_PERCENT`] of the text path; else aqr-xsv if it takes the text, else
/// none (the text path).
fn log_or_table(d: &[u8], xsv: bool) -> Option<Format> {
    let s = &*table_sample(d);
    let level = TABLE_ZSTD_LEVEL.get();
    let z = |b: &[u8]| zstd::bulk::compress(b, level).map_or(b.len(), |c| c.len());
    let coded = |f: Formula| f.streams.iter().map(|s| z(&s.data)).sum::<usize>();
    let log = aqr_log::formula::encode(s, 1).map(coded);
    let x = xsv.then(|| aqr_xsv::formula::encode(s, 1).map(coded)).flatten();
    pick_table(log, x, || z(s), xsv, LOG_PERCENT.get())
}

/// [`log_or_table`]'s choice, of the sizes of the sample's streams by aqr-log (`log`) and by
/// aqr-xsv (`x`), and of the sample itself (`text`, worked out where it is asked for), at `percent`.
pub(crate) fn pick_table(
    log: Option<usize>,
    x: Option<usize>,
    text: impl FnOnce() -> usize,
    xsv: bool,
    percent: usize,
) -> Option<Format> {
    match log {
        Some(log) if x.is_some_and(|x| x <= log) => Some(Format::Xsv),
        Some(log) if log * 100 <= text() * percent => Some(Format::Log),
        _ => xsv.then_some(Format::Xsv),
    }
}

/// A formula of data, its stream 0 perhaps yet to be cut out of the data: the bytes of `gaps`,
/// ranges of the data, one after the other. A scan's formula is so until put in place, where
/// the data between the files found is cut out of the data's own stream if it can be.
pub(crate) struct Made {
    pub f: Formula,
    pub(crate) gaps: Option<Vec<Range<usize>>>,
    /// Where the formula lost to the data as it is by little ([`Verdict::Close`]): the sizes they
    /// archived in, the formula's and the data's. Whoever puts it in place keeps the data beside
    /// it ([`Node::Either`]).
    pub(crate) close: Option<(usize, usize)>,
}

impl From<Formula> for Made {
    fn from(f: Formula) -> Made {
        Made { f, gaps: None, close: None }
    }
}

impl Made {
    /// The formula, cut out of `d`, the data it is of.
    pub(crate) fn cut(self, d: &[u8]) -> Formula {
        let Made { mut f, gaps, .. } = self;
        if let Some(gaps) = gaps {
            // (of the store where it is large, see [`Options::spill`])
            let mut data = aqr_formula::store::Writer::new(Some(gaps.iter().map(Range::len).sum()));
            for g in gaps {
                data.write(&d[g]);
            }
            f.streams[0].data = data.into_bytes();
        }
        f
    }

    /// [`Made::cut`] out of `d` itself, in place where it is its own (a copy of what is kept,
    /// where it is shared: a file mapped, not to be copied whole).
    pub(crate) fn cut_from(self, d: Bytes) -> Formula {
        if d.is_shared() {
            return self.cut(&d);
        }
        let Made { mut f, gaps, .. } = self;
        let mut d = d.into_vec();
        if let Some(gaps) = gaps {
            let mut at = 0;
            for g in gaps {
                let n = g.len();
                if g.start != at {
                    d.copy_within(g, at);
                }
                at += n;
            }
            d.truncate(at);
            d.shrink_to_fit();
            f.streams[0].data = d.into();
        }
        f
    }
}

/// A stream of LZFSE and LZVN blocks, at level 9 only, if its data (as its blocks' headers
/// tell) fits in what is left of the budget ([`Options::expansion`]). The data is many times
/// the streams (14 times in the asset catalogs of /Applications: 3.7 GB of 272.5 MB), and
/// recompressing them takes 13% off at some 8 MB/s (34 s at level 8 rather than 1.3 s, peak
/// 8.2 GB; [`ahead`]). The LZVN streams there (small data) gain nothing at the default level
/// and are left out too ([`format_on`]).
pub(crate) fn bvx(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let (_, n) = aqr_bvx::bvx::extent(d)?;
    if !cx.inflate(n) {
        return None;
    }
    aqr_bvx::formula::encode(d, opt.threads)
}

/// aqr-wim's formula of `d`: of its data made again where needed, rather than held, where its
/// bytes are shared ([`aqr_formula::store::find`]: of the input mapped, or of the spill), so
/// that [`make_later`] makes the data of each content as it comes to it.
pub(crate) fn wim(d: &[u8], threads: usize) -> Option<Formula> {
    match aqr_formula::store::find(d) {
        Some(b) => aqr_wim::formula::encode_shared(&b, threads),
        None => aqr_wim::formula::encode(d, threads),
    }
}

/// Whether LZX streams (CAB's folders, CHM's sections) are taken at the level of `opt`: from
/// level 9, as they split at about 22 MB/s of data on one thread a stream, and a CAB or CHM file
/// alone gains little for it (441 CAB files: -1.1% for 73.7 -> 290 s; 139 CHM files: -18.3% for
/// 3.2 -> 14.8 s). What pays is the files archived along with them (40 LZX CAB files with the
/// files they hold: -33%).
/// (CHM files are not known by their names: one known so and turned down would not be scanned.)
pub(crate) fn lzx_on(opt: &Options) -> bool {
    opt.level.scale() >= LZX_LEVEL.get()
}

aqr_common::zopt! {
    /// The level LZX streams are taken from ([`lzx_on`]).
    pub(crate) static LZX_LEVEL: Opt<u8> =
        Opt::value("lzx-level", 9, "least level at which LZX streams (CAB, CHM) are rebuilt");
    /// The level RAR's PPMd blocks are taken from ([`ppmd_on`]).
    static PPMD_LEVEL: Opt<u8> = Opt::value("ppmd-level", 9, "least level at which RAR's PPMd blocks are rebuilt");
}

/// Whether RAR's streams with PPMd blocks are taken apart (and PPMd rebuilt): from level 9 only,
/// as rebuilding PPMd is slow and its data seldom archives smaller apart (high-order PPMd is
/// strong); below, they are turned down at their first PPMd block.
pub(crate) fn ppmd_on(opt: &Options) -> bool {
    opt.level.scale() >= PPMD_LEVEL.get()
}

/// Whether files of `format` are taken at the level of `opt` (else kept as they are): none at
/// levels 0 and 1, which recompress nothing ([`Options::recompresses`]); LZMA and
/// xz streams (aqr-lzma), 7z archives (aqr-7z, and aqr-lzma on their folders) and GIF files
/// (aqr-gif) from level 4, as they are slow for their bytes; streams of LZFSE and LZVN blocks
/// ([`bvx`]) and CHM files ([`lzx_on`]) from level 9. Parts of a format taken (CAB's LZX
/// folders, RAR's PPMd blocks) are its recompressor's to turn down: [`lzx_on`], [`ppmd_on`].
pub(crate) fn format_on(format: Format, opt: &Options) -> bool {
    opt.recompresses() && opt.level.scale() >= format.level()
}

/// `d` as a file of `format`, with its contents nested, if its recompressor takes it (and the
/// formula is [`smaller`]).
pub(crate) fn recompress_as(format: Format, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    recompress_with(format, d, Kind::Unknown, opt, depth, cx)
}

/// [`recompress_as`], the data of a zlib stream or gzip member (without a name) of kind
/// `contents`.
pub(crate) fn recompress_with(
    format: Format,
    d: &[u8],
    contents: Kind,
    opt: &Options,
    depth: u32,
    cx: &Cx,
) -> Option<Formula> {
    // (the sealing kept from what this recompressor makes, given back where it is not used)
    let mut sealing = cx.sealing.take();
    let f = recompress_sealed(format, d, contents, opt, depth, cx, &mut sealing);
    if sealing.is_some() {
        *cx.sealing.borrow_mut() = sealing;
    }
    f
}

/// [`recompress_with`], the contents of the formula sealed by `sealing` (taken) where it makes
/// its files apart, and is not compared with `d` ([`smaller`]).
#[allow(clippy::too_many_arguments)]
fn recompress_sealed(
    format: Format,
    d: &[u8],
    contents: Kind,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    sealing: &mut Option<seal::Sealing>,
) -> Option<Formula> {
    if !format_on(format, opt) || !fits(format, d.len()) {
        return None;
    }
    let _s = aqr_common::trace::span(format.name());
    #[cfg(test)]
    tests::TRIED.with(|t| t.borrow_mut().push(format));
    let ready = cx.ready.borrow_mut().remove(&(d.as_ptr() as usize, d.len(), format));
    let prepared = ready.is_some();
    // (the hint of the data, of what its entries are, for its recompressor)
    let entered = crate::hints::enter(d, cx);
    let words = cx.words.borrow().clone();
    let (words, held) = (words.as_deref(), cx.held.as_deref());
    // (the undos of this recompressor's formula, those of formulas within it its own)
    let outer = cx.undo.take();
    let f = match ready {
        Some(f) => f,
        None => match recompressor(format) {
            Recompressor::Cx(r) => attempted(format, d, || r(d, contents, opt, depth, cx)),
            _ => {
                // (of the file itself, archived directly: its parts sealed as they are made, each
                // evaluated beforehand on its own)
                let order = sealing_parts(sealing);
                let (seal, pre) = (order.as_deref(), cx.pre.as_deref());
                pure(format, d, &Pure { opt, depth, words, once: &cx.once, held, seal, pre }).flatten()
            }
        },
    };
    let undos = cx.undo.replace(outer);
    let f = f?;
    // (parts sealed already, evaluated beforehand as they were)
    let sealed = f.streams.iter().any(|s| s.data.sunk_in().is_some());
    let mut f = f;
    f.mark_contents();
    crate::hints::nameless(&mut f, format, &entered);
    drop(entered);
    // (an application over the bound: this recompression is not used, as if the recompressor did
    // not take the data; a formula of data not made yet is not looked at)
    if !f.streams.iter().any(|s| s.data.is_lazy())
        && let Some((module, needed)) = memory::exceeding(&f, &Native { threads: 1 }, opt.application_bound)
    {
        memory::gave_up(module, needed, d.len() as u64);
        return None;
    }
    aqr_formula::store::spill_streams(&mut f);
    if format == Format::Text {
        cx.note_coded(&f, d);
    }
    // evaluated beforehand while its contents are recompressed (once it is known not to be
    // taken into a set, where made beforehand as a PNG file among contents: it is put off, as
    // only PNG files are taken into sets), a copy of it sharing its bytes: the contents may take
    // the bytes of their streams out of it as they are made ([`make_later`]), let go of once
    // the prevalidator is done with them too
    f.streams.iter_mut().for_each(|s| s.data.share());
    let shared = Arc::new(f.clone());
    // (a formula of data not made yet, aqr-wim's: its applications evaluated with their contents
    // as they are made, [`make_later`], else once its data is made, below)
    let lazy = f.streams.iter().any(|s| s.data.is_lazy());
    if let Some(pre) = cx.pre.as_ref().filter(|_| !lazy && !sealed) {
        match prepared && format == Format::Png {
            true => pre
                .job(shared.clone(), d)
                .into_iter()
                .for_each(|j| cx.deferred.borrow_mut().push((d.as_ptr() as usize, j))),
            false if format == Format::Text && !cx.once.evaluated(d) => {}
            false => pre.submit(shared.clone(), d),
        }
    }
    let mark = cx.deferred.borrow().len();
    // (an ar, tar or cpio archive only bundles files: they are as deep as it is; and so are
    // those an NSIS installer stores)
    let deeper;
    let opt = match format.is_bundle()
        || format == Format::Pe && (nsis::in_pe(d).is_some_and(|b| !b.is_empty()) || aqr_inno::end(d).is_some())
    {
        true => {
            deeper = Options { depth: opt.depth.saturating_add(1), ..opt.clone() };
            &deeper
        }
        false => opt,
    };
    drop(shared);
    let watches = told_as_second_half(&f, d);
    let words = (format == Format::Jsonl || format.is_bundle()).then(|| segment_words(&f, opt, cx)).flatten();
    // (where the data put off is, and what the formula is, as the contents may take its streams
    // out of it)
    let bases: Vec<usize> = f.streams.iter().map(|s| addr(&s.data)).collect();
    let of = bundle::Kinds::of(&f);
    cx.parts_checked.set(false);
    if of.files_apart {
        *cx.sealing.borrow_mut() = sealing.take();
    }
    let mut children = nested(&mut f, d, opt, depth, cx);
    if !undos.is_empty() && undo(&mut f, d, children.as_mut(), &undos, opt, cx) && children.is_none() {
        f.compact();
    }
    if words.is_some() {
        cx.share_words(None);
    }
    drop(watches);
    // (data not made by the contents made: made now, and the formula evaluated as others are
    // where its contents' applications were not)
    if lazy {
        f.streams.iter_mut().filter(|s| s.data.is_lazy()).for_each(|s| s.data = s.data.made());
        if let Some(pre) = cx.pre.as_ref().filter(|_| !cx.parts_checked.get()) {
            pre.submit(Arc::new(f.clone()), d);
        }
    }
    let mut taken = Vec::new();
    if let Some((mut children, plan)) = children {
        let sets = sets::sets(&f, &mut children, &plan, opt, depth, cx);
        for s in &sets {
            for &m in &s.members {
                if let &Node::Slice { stream, start, .. } = &children[m].0 {
                    taken.push(bases[stream] + start as usize);
                }
            }
        }
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), sets);
        aqr_formula::store::spill_streams(&mut f);
    }
    // the contents' put off, but those taken into a set
    let deferred: Vec<_> = cx.deferred.borrow_mut().drain(mark..).collect();
    if let Some(pre) = &cx.pre {
        deferred.into_iter().filter(|(at, _)| !taken.contains(at)).for_each(|(_, j)| pre.send(j));
    }
    // (text as aqr-wrt tells, see [`made`]; else kept beside the data where it loses by little
    // and the caller puts both in place, [`made_by`])
    if format == Format::Text {
        return Some(f);
    }
    let id = (d.as_ptr() as usize, d.len());
    match verdict_as(&f, None, d, of, opt, cx, cx.defer_for.get() == Some(id)) {
        Verdict::Take => Some(f),
        Verdict::Close(sizes) => {
            cx.closed.set(Some((id, sizes)));
            Some(f)
        }
        Verdict::Reject => None,
    }
}

/// Undoes the decodings of `undos` of formula `f` of data `d` ([`Undo`]) where nothing is made of
/// their contents (of `children`, those [`nested`] made: their slices of the content dropped)
/// and their data would not pay as it is; not where contents are planned into sets, whose
/// members are told by their places among the children. Whether any was undone.
fn undo(
    f: &mut Formula,
    d: &[u8],
    children: Option<&mut (Contents, sets::Plan)>,
    undos: &[Undo],
    opt: &Options,
    cx: &Cx,
) -> bool {
    if children.as_ref().is_some_and(|c| !c.1.is_empty()) {
        return false;
    }
    let mut children = children.map(|c| &mut c.0);
    let of = |n: &Node, s: usize| matches!(n, &Node::Slice { stream, .. } if stream == s);
    let mut undone = false;
    for u in undos {
        // (a formula kept beside the data is not taken: [`Made::close`])
        let made = children
            .as_ref()
            .is_some_and(|c| c.iter().any(|(n, m)| of(n, u.stream) && m.as_ref().is_some_and(|m| m.close.is_none())));
        let Some(data) = f.streams.get(u.stream).filter(|s| s.content && !s.data.is_lazy()) else { continue };
        let Some(coded) = d.get(u.coded.clone()) else { continue };
        // (beside the coded bytes, whatever is made of the data: its children placed in it)
        if u.defer {
            let kept = f.push(u.kept, coded.to_vec());
            if !wrap_apply(&mut f.root, u.stream, &kept, (0, coded.len())) {
                f.streams.pop();
            }
            continue;
        }
        if made || !undoes(coded.len(), u.record, &data.data, d, opt, cx) {
            continue;
        }
        let kept = f.push(u.kept, coded.to_vec());
        if !replace_apply(&mut f.root, u.stream, &kept) {
            f.streams.pop();
            continue;
        }
        if let Some(c) = children.as_mut() {
            c.retain(|(n, _)| !of(n, u.stream));
        }
        undone = true;
    }
    undone
}

/// Whether a decoding of `coded` bytes into `data` and a record of `record` bytes, nothing made
/// of the data, is undone ([`undo`]): unless they may archive in less than the coded bytes, as
/// aqr-formula's flat probe tells (what other data of `within`'s batch holds counting as nothing).
fn undoes(coded: usize, record: usize, data: &[u8], within: &[u8], opt: &Options, cx: &Cx) -> bool {
    let held = || cx.held_by_others(data, within) as u8 as f64;
    !Probe::flat(aqr_archive::archivers(opt.level)).pays(&cx.enc, coded, || Some(record), data, held, Kind::Unknown)
}

/// Puts the application below `n` that takes a slice of `stream` among its inputs beside `kept`
/// ([`Node::Either`]), of the `sizes` told. Whether there was one.
fn wrap_apply(n: &mut Node, stream: usize, kept: &Node, sizes: (usize, usize)) -> bool {
    match n {
        Node::Apply(a) if a.inputs.iter().any(|i| matches!(i, &Node::Slice { stream: s, .. } if s == stream)) => {
            let len = a.len;
            let made = std::mem::replace(n, Node::Concat(Vec::new()));
            *n = Node::Either(Box::new(aqr_formula::Either { kept: kept.clone(), made, len, sizes }));
            true
        }
        Node::Apply(a) => a.inputs.iter_mut().any(|i| wrap_apply(i, stream, kept, sizes)),
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().any(|n| wrap_apply(n, stream, kept, sizes)),
        _ => false,
    }
}

/// Puts `with` in place of the application below `n` that takes a slice of `stream` among its
/// inputs. Whether there was one.
fn replace_apply(n: &mut Node, stream: usize, with: &Node) -> bool {
    match n {
        Node::Apply(a) if a.inputs.iter().any(|i| matches!(i, &Node::Slice { stream: s, .. } if s == stream)) => {
            *n = with.clone();
            true
        }
        Node::Apply(a) => a.inputs.iter_mut().any(|i| replace_apply(i, stream, with)),
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().any(|n| replace_apply(n, stream, with)),
        _ => false,
    }
}

/// Where bytes `b` are, as an address (0 for bytes not made yet, or sunk).
fn addr(b: &Bytes) -> usize {
    match b.is_lazy() || b.sunk_in().is_some() {
        true => 0,
        false => b.as_ptr() as usize,
    }
}

aqr_common::zopt! {
    /// Text of the contents of a formula shorter than this in all is coded with dictionaries of each
    /// text's own (as [`archive`]'s batches have it).
    static SEGMENT_WORDS_MIN: Opt<usize> = Opt::size(
        "words:segment-min",
        256 << 10,
        "least bytes of the text in a formula's contents that share a dictionary",
    );
}

/// Sets [`Cx::words`] for the contents of `f`, where none is set: a dictionary of the text of its
/// contents (for JSON lines cut into segments, those of [`Kind::Text`]; for an archive that only
/// bundles files, the members that are text as [`made`] tells), at least [`SEGMENT_WORDS_MIN`]
/// bytes in all, so that it is coded with one dictionary, as it is where the file is one segment,
/// and the same text of members is coded alike (the files of a tar archive, as they are on their
/// own in an archive of files). Returns it, to be unset once they are made.
fn segment_words(f: &Formula, opt: &Options, cx: &Cx) -> Option<Arc<aqr_wrt::formula::Shared>> {
    // (of one segment, the file's text has a dictionary of its own anyway)
    let apart = bundle::bundles(f);
    if cx.words.borrow().is_some() || !(apart || matches!(f.root, Node::Concat(_))) {
        return None;
    }
    let texts: Vec<&[u8]> = match apart {
        true => {
            let mut texts = Vec::new();
            member_texts(&f.root, f, &mut texts);
            texts
        }
        false => f
            .streams
            .iter()
            .filter(|s| s.content && s.kind == Kind::Text && aqr_wrt::formula::starts(&s.data))
            .map(|s| &s.data[..])
            .collect(),
    };
    if texts.len() < 2 || texts.iter().map(|t| t.len()).sum::<usize>() < SEGMENT_WORDS_MIN.get() {
        return None;
    }
    let words = Arc::new(aqr_wrt::formula::Shared::of(&texts, opt.threads)?);
    cx.share_words(Some(words.clone()));
    Some(words)
}

/// The contents below node `n` of `f` that are text ([`segment_words`]), made already.
fn member_texts<'a>(n: &Node, f: &'a Formula, out: &mut Vec<&'a [u8]>) {
    match n {
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| member_texts(n, f, out)),
        &Node::Slice { stream, .. } if is_content(f, stream) => slices_of(n, f, &mut |d| {
            if is_text(detect(d)) && aqr_wrt::formula::starts(d) && !aqr_obj::formula::starts(d) {
                out.push(d);
            }
        }),
        Node::Apply(a) => a.inputs.iter().for_each(|n| member_texts(n, f, out)),
        _ => {}
    }
}

/// The bytes of the slices below node `n` of `f` that are made already.
fn slices_of<'a>(n: &Node, f: &'a Formula, each: &mut dyn FnMut(&'a [u8])) {
    match n {
        &Node::Slice { stream, start, len } => {
            let d = f.streams.get(stream).filter(|s| !s.data.is_lazy()).map(|s| &s.data[..]);
            if let Some(d) = d.and_then(|d| d.get(start as usize..)?.get(..len as usize)) {
                each(d);
            }
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| slices_of(n, f, each)),
        _ => {}
    }
}

/// Tells of the work on the streams of `f` that are data of their own (not of `d`, the data
/// `f` is of), as their contents are made in turn ([`nested`]), as of the second half of `d`,
/// each stream its share by size: the first is the formula's own, as far as its recompressor
/// tells ([`aqr_common::progress`]).
fn told_as_second_half(f: &Formula, d: &[u8]) -> Vec<aqr_common::progress::Watch> {
    let within = |s: &[u8]| d.as_ptr_range().contains(&s.as_ptr());
    let own: Vec<&[u8]> = f
        .streams
        .iter()
        .filter(|s| !s.data.is_lazy() && s.data.sunk_in().is_none())
        .map(|s| &s.data[..])
        .filter(|s| !s.is_empty() && !within(s))
        .collect();
    let total: usize = own.iter().map(|s| s.len()).sum();
    let (half, rest) = (d.len() / 2, d.len() - d.len() / 2);
    let at = |n: usize| half + (n as u128 * rest as u128 / total.max(1) as u128) as usize;
    let mut n = 0;
    own.iter()
        .map(|s| {
            let part = &d[at(n)..at(n + s.len())];
            n += s.len();
            aqr_common::progress::alias(s, part)
        })
        .collect()
}
