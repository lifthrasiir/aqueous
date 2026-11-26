//! Whether recompressing pays: the [`probe`], formulas against their data ([`smaller`]), the
//! [`Verdict`] of a recompression.

use super::*;

aqr_common::zopt! {
    /// Whether [`probe`] is made: by default not at level 7 or more.
    static PROBE: Opt<&'static str> = Opt::value(
        "probe",
        "auto",
        "whether to probe if recompressing a stream may pay (auto: not from level 7)",
    )
    .choices(&["auto", "on", "off"])
    .bare("on")
    .off("off");
}

/// Unless [`Options::smaller`] is off, or the level is 7 or more (DEFLATE always rebuilt, a few
/// seconds of 10gb), what tells whether recompressing a stream may pay, for its data `depth`
/// formats deep: at the level of the archive, files looked for as [`Nest::of`] tells (scanned,
/// unless nested formats would go deeper than allowed). An ar, tar or cpio archive is scanned
/// for the files in it, as it gains nothing itself; text is told by its data, as aqr-wrt takes
/// no more than a tenth off it.
pub(crate) fn probe(opt: &Options, depth: u32) -> Option<Probe> {
    match PROBE.get() {
        "on" => {}
        "off" => return None,
        _ if opt.level.scale() >= 7 => return None,
        _ => {}
    }
    let look = |kind| match Nest::Scan.of(kind) {
        None | Some(Nest::As(Format::Text) | Nest::Words | Nest::Column) => Look::No,
        Some(Nest::As(f)) if !f.is_bundle() => Look::Whole,
        Some(Nest::Hint(i)) => match crate::hints::get(i).format {
            crate::hints::Told::As(f) if !f.is_bundle() => Look::Whole,
            _ => Look::Scan,
        },
        Some(_) => Look::Scan,
    };
    let starts = |d: &[u8]| detect(d).is_some();
    let probe = Probe { archiver: aqr_archive::archivers(opt.level), look, starts, scan: files_len };
    opt.smaller.then_some(if depth + 1 < opt.depth { probe } else { Probe::flat(probe.archiver) })
}

/// Bytes of the files of formats that recompressors take in `d`, as a scan would find them:
/// by their signatures, then their ends ([`extent`]), each from the end of the one before. An
/// ar, tar or cpio archive is not one, but the files in it are; nor is text (see [`probe`]).
pub(crate) fn files_len(d: &[u8]) -> usize {
    let len = |p: usize, f: Option<Format>| {
        f.filter(|f| !f.is_bundle() && *f != Format::Text).and_then(|f| extent(f, &d[p..])).unwrap_or(0)
    };
    let mut total = len(0, detect(d));
    let mut p = detect::next_start(d, total.max(1));
    while p < d.len() {
        let n = len(p, detect::starts(&d[p..]));
        total += n;
        p = detect::next_start(d, p + n.max(1));
    }
    total
}

/// Unless [`Options::smaller`] is off, whether formula `f` of `d` archives smaller than `d`
/// raw, its stream 0 the bytes of `gaps` of `d` if given ([`Made`]).
///
/// An ar, tar or cpio archive (alone, or found by a scan) that the files archived together hold
/// is taken whatever its size: its members' formulas are theirs, whose streams are stored once
/// ([`Formula::share_slices`]), though they cost in full here (the streams of a member that is a
/// PE file, say, are not its bytes, which the files are held by). The streams of the formula
/// taken of other data the files hold cost nothing for what holds that data ([`Cx::note_held`]).
/// An ISO 9660 or UDF image is taken whatever its size too: its formula is its own bytes, cut
/// into the files and the rest, which cost no more apart (and comparing would compress all of
/// an image twice, a DVD's 3 GB). So is a WIM file: its LZX chunks are compressed apart, 32 KiB
/// each, and their data compresses better together (by far more than the records cost, where
/// it is not compressed already; where it is, the records are small); comparing took 14 of
/// 24 s of a boot.wim of 169 MB. So is a scan's formula whose files are all of the data, each
/// of them compared already (or text, not compared: [`made`]). So is a PPMd file's, its header
/// cut off ([`takes_place`]), which costs no more apart: a file whose data starts another's
/// is stored once only where both are cut alike. So is a ProRes file's of [`PRORES_UNCOMPARED`]
/// bytes or more: its frames are coded to a fraction of their bytes, which zstd of the file never
/// nears, and the comparison costs 3.7 s on one thread at the end for 1.3 GB. A small one is
/// compared still: what zstd makes of nearly blank frames may beat the formula's modules. So is a
/// file of tensors' (aqr-tensors'): its encoder codes a run only where that comes to less than its
/// bytes (which zstd hardly compresses: weights), and comparing compresses gigabytes twice on one thread (3.6 of
/// 6.3 s for a model of 1.2 GB). So is a Wwise medium's (aqr-wwise's): its encoder gives a formula only where
/// its streams but the setup come to less than its bytes (coded packets, which zstd hardly compresses),
/// and comparing took a third of the time of a bank of 15,425 small media (17 of 47 thread-seconds).
pub(crate) fn smaller(f: &Formula, gaps: Option<&[Range<usize>]>, d: &[u8], opt: &Options, cx: &Cx) -> bool {
    smaller_as(f, gaps, d, bundle::Kinds::of(f), opt, cx)
}

aqr_common::zopt! {
    /// The size of a ProRes file from which on [`smaller`] takes its formula without comparing it.
    static PRORES_UNCOMPARED: Opt<usize> = Opt::size(
        "prores:uncompared",
        16 << 20,
        "bytes of a ProRes file from which on its formula is not compared with zstd",
    );
    /// The size of an XML file from which on [`smaller`] takes its formula without comparing it:
    /// it was chosen on a sample whose streams, by zstd -3, came to no more than the sample's
    /// (`xml:percent`), and its containers are recompressed in turn (aqr-wrt codes text at least
    /// 1/32 smaller than zstd or leaves it), so that it archives smaller than the file raw by a
    /// margin no module block or record can make up where the file is big; comparing compressed
    /// all of it twice (3 of 20 thread-seconds for enwik9). Small files are compared: the module's
    /// block alone costs 4 KB.
    pub(crate) static XML_UNCOMPARED: Opt<usize> = Opt::size(
        "xml:uncompared",
        4 << 20,
        "bytes of an XML file from which on its formula is not compared with zstd",
    );
    /// Whether [`smaller_as`] compares the formulas it takes without comparing by default (aqr-ppmd's,
    /// aqr-tensors', aqr-wwise's, large ProRes files' and large XML files').
    static SMALLER_ALWAYS: Opt<bool> = Opt::flag(
        "smaller-always",
        false,
        "compare the formulas of ppmd, tensors, wwise, large ProRes and large XML files with zstd too",
    );
}

/// [`smaller`] of formula `f` of what `of` tells, as its recompressor made it: once its
/// contents are in place, its own streams may be gone (a WIM file's data, all of it cut into
/// its contents' formulas).
fn smaller_as(f: &Formula, gaps: Option<&[Range<usize>]>, d: &[u8], of: bundle::Kinds, opt: &Options, cx: &Cx) -> bool {
    verdict_as(f, gaps, d, of, opt, cx, false) == Verdict::Take
}

/// Where data is and its length, as [`Cx::defer_for`] names it.
pub(crate) type Span = (usize, usize);

/// What [`smaller_as`] tells of a formula: taken, turned down, or turned down by little
/// ([`DEFER_SLACK`]) and kept beside its data for the batch to choose (where `defer` lets it,
/// of [`DEFER_MIN`] bytes or more, within [`Cx::defer_budget`]): with the sizes compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Take,
    Close((usize, usize)),
    Reject,
}

aqr_common::zopt_namespace!(
    "defer",
    "formulas that lose by little kept beside their data, for the batch to choose (aqr-compress)"
);

aqr_common::zopt! {
    /// A formula that archives in up to this share more than its data does as it is is kept
    /// beside it, where that may be ([`Verdict::Close`]); `-Z defer:no-slack`: none is.
    pub(crate) static DEFER_SLACK: Opt<Option<f64>> =
        Opt::value("defer:slack", Some(0.25), "keep a formula beside its data where it is up to this share larger").off(None);
    /// Whether, of a file archived alone, a formula that loses to its data only by the module and
    /// resource blocks it takes is kept beside it ([`Verdict::Close`]), the blocks paid for by
    /// all that take them ([`choose::by_blocks`]).
    pub(crate) static DEFER_MODULES: Opt<bool> = Opt::flag(
        "defer:modules",
        true,
        "of a file alone, keep a formula beside its data where it loses only by its module blocks",
    );
    /// Data shorter than this is not kept beside its formula ([`Verdict::Close`]).
    pub(crate) static DEFER_MIN: Opt<usize> = Opt::size("defer:min", 16 << 10, "data shorter is not kept beside its formula");
    /// The bytes of formulas kept beside their data, of a context, at most this many times its
    /// data ([`Cx::defer_budget`]): a formula's streams come to more than its data (WinUHA's
    /// 4.0 installers', 1.4 times what each is).
    pub(crate) static DEFER_BUDGET: Opt<f64> =
        Opt::value("defer:budget", 2.0, "formulas kept beside their data take at most this many times a context's data");
}

/// [`smaller_as`] as a [`Verdict`]. A formula holding alternatives ([`Node::Either`]) is sized with
/// their data kept, as it would be archived where they are all turned down.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verdict_as(
    given: &Formula,
    gaps: Option<&[Range<usize>]>,
    d: &[u8],
    of_given: bundle::Kinds,
    opt: &Options,
    cx: &Cx,
    defer: bool,
) -> Verdict {
    // (sized as it would be made with its alternatives turned down: their data kept)
    let (kept, of);
    let f = match given.eithers().is_empty() {
        true => {
            of = of_given;
            given
        }
        false => {
            kept = given.chosen(false);
            of = bundle::Kinds::of(&kept);
            &kept
        }
    };
    let share_min = SHARE_MIN.get();
    let bundled = of.bundles && cx.held_by_others(d, d);
    if bundled {
        cx.note_bundled(given);
    }
    let covered = gaps.is_some_and(|g| g.iter().all(Range::is_empty));
    // (an image coded against another's code holds a copy of it, which would only be compressed
    // to be taken off: the code filtered, it is smaller)
    let against = f.streams.iter().any(|s| s.class == exe::REFERENCE);
    // (a file keeping DEFLATE streams that files archived along with it are made of)
    let inflated = cx.bodies.as_ref().is_some_and(|b| b.holds_streams(d));
    let smaller = !opt.smaller
        || bundled
        || covered
        || against
        || inflated
        || of.files_apart
        || (!SMALLER_ALWAYS.get()
            && (aqr_ppmd::formula::is(f)
                || (d.len() >= PRORES_UNCOMPARED.get() && aqr_prores::formula::is(f))
                || aqr_tensors::formula::is(f)
                || aqr_wwise::formula::is(f)
                || (d.len() >= XML_UNCOMPARED.get() && aqr_xml::formula::is(f))))
        || {
            #[cfg(test)]
            tests::COMPARED.with(|c| c.borrow_mut().push(d.len()));
            let _s = aqr_common::trace::span("verdict");
            let pieces: Option<Vec<&[u8]>> = gaps.map(|gaps| gaps.iter().map(|g| &d[g.clone()]).collect());
            let stand_in = pieces.as_deref().map(|p| (0, p));
            // (the streams the files archived together hold cost nothing)
            let free: Vec<usize> = (0..f.streams.len())
                .filter(|&s| stand_in.is_none_or(|(t, _)| t != s))
                .filter(|&s| {
                    // (text coded alike as its copies, whose data was long enough to tell)
                    let data = &f.streams[s].data;
                    data.len() >= share_min && cx.held_by_others(data, d) || cx.coded_held(data)
                })
                .collect();
            let (n, raw) = cx.enc.sizes_but(f, stand_in, &free, d).expect("slices within their streams");
            // (not of streams sealed already, nor of data not made yet: kept apart as they are made)
            let apart = f.streams.iter().any(|s| s.data.sunk_in().is_some() || s.data.is_lazy());
            let slack = DEFER_SLACK.get().filter(|_| defer && !apart && d.len() >= DEFER_MIN.get());
            // (of a file archived alone, the formula sized without the blocks the archive holds
            // once, which the alternatives taking them pay for together: [`choose::by_blocks`])
            let alone = n >= raw && defer && !apart && !cx.enc.shares_modules() && DEFER_MODULES.get();
            let n_once = match alone {
                true => n.saturating_sub(cx.enc.once_len(f).expect("modules and resources of the formula")),
                false => n,
            };
            let close = slack.is_some_and(|slack| n as f64 <= raw as f64 * (1.0 + slack)) || alone && n_once < raw;
            if n >= raw && close {
                let bytes = kept_bytes(given);
                if cx.kept_beside.get() + bytes <= cx.defer_budget {
                    cx.kept_beside.set(cx.kept_beside.get() + bytes);
                    return Verdict::Close((n_once, raw));
                }
            }
            n < raw
        };
    if smaller {
        // `d` is not archived as it is (`f` is, or is part of something larger kept raw)
        cx.enc.forget(d);
        if !bundled && d.len() >= share_min && cx.held_by_others(d, d) {
            cx.note_held(given);
        }
    }
    if smaller { Verdict::Take } else { Verdict::Reject }
}

/// The bytes that keeping formula `f` beside its data holds ([`Cx::defer_budget`]): of its streams,
/// but those of its alternatives (either side), counted as they were kept (a content's streams are
/// moved into its parent's formula, not copied).
pub(crate) fn kept_bytes(f: &Formula) -> usize {
    let mut within = vec![false; f.streams.len()];
    for (_, e) in f.eithers() {
        for side in [&e.kept, &e.made] {
            side.visit(&mut |n| {
                if let &Node::Slice { stream, .. } = n
                    && let Some(w) = within.get_mut(stream)
                {
                    *w = true;
                }
            });
        }
    }
    f.streams.iter().zip(within).filter(|(_, w)| !w).map(|(s, _)| s.data.len()).sum()
}

#[cfg(not(aqr_dist))]
aqr_common::zopt! {
    /// Each file a scan finds, told on standard error (`SCAN`, the data's length, depth, start,
    /// length and format), for a measurement.
    pub(crate) static SCAN_DUMP: Opt<bool> = Opt::flag("dump:scan", false, "tell each file a scan finds on standard error (SCAN lines)");
}

aqr_common::zopt! {
    /// Bytes of the data that a scan may give to recompressors which turn it down, on top of
    /// twice the data: past them, no more files are looked for.
    pub(crate) static SCAN_SLACK: Opt<usize> = Opt::size(
        "scan:slack",
        16 << 20,
        "bytes of data a scan may give to recompressors that turn it down, besides twice the data",
    );
}
