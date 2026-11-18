//! The recompressors of the pure formats ([`pure`]), each given the data and a [`Pure`].

use super::*;

/// What the recompressor of a [`pure`] format is given besides the data: the options (of the
/// level and the threads), how many formats deep the data is, and what the context lends.
pub(crate) struct Pure<'a> {
    pub(crate) opt: &'a Options,
    pub(crate) depth: u32,
    /// The dictionary text is coded with, if any ([`Cx::words`]).
    pub(crate) words: Option<&'a aqr_wrt::formula::Shared>,
    /// Which text is coded once ([`Cx::once`]).
    pub(crate) once: &'a Once,
    /// What the files archived along hold ([`Cx::held`]).
    pub(crate) held: Option<&'a Held>,
    /// Where the formula is of the file itself archived directly ([`direct_room`]): the order
    /// whose sink the formula's parts are sealed into as they are made, as the recompressor may
    /// have them ([`tensors`], [`Pure::made`]).
    pub(crate) seal: Option<&'a seal::Order>,
    /// Where those parts are evaluated beforehand ([`Cx::pre`]).
    pub(crate) pre: Option<&'a verify::Prevalidator>,
}

impl Pure<'_> {
    /// What part `part` of the formula, of bytes `of`, makes ready to be sealed into the sink of
    /// [`Pure::seal`], on any thread: its memory declared, evaluated beforehand, and its blocks
    /// of its own encoded ([`aqr_archive::sink::Sink::ready`]).
    pub(crate) fn made(&self, part: &mut Formula, of: &[u8]) -> aqr_archive::sink::Ready {
        let order = self.seal.expect("sealing");
        let _a = aqr_common::allowance::enter(aqr_common::allowance::Context::NONE);
        choose::kept_in(part, |part| {
            memory::declare(part, &Native { threads: 1 }, &memory::Limits::default());
            if let Some(pre) = self.pre {
                part.streams.iter_mut().for_each(|s| s.data.share());
                pre.submit(Arc::new((**part).clone()), of);
            }
            order.sink().ready(part).expect("encoded")
        })
    }
}

/// aqr-tensors' formula of `d`, each segment coded sealed as it is made where `p` tells, else its
/// streams spilled.
pub(crate) fn tensors(d: &[u8], p: &Pure) -> Option<Formula> {
    let segment = aqr_tensors::formula::SEGMENT_BYTES.get();
    #[cfg(test)]
    let segment = tests::TENSORS_SEGMENT.get().unwrap_or(segment);
    match p.seal {
        Some(order) => {
            let made = |part: &mut Formula, of: &[u8]| p.made(part, of);
            let mut seal = |part: &mut Formula, _: &[u8], ready| {
                choose::kept_in(part, |part| order.sink().seal_ready(part, ready)).expect("sealed")
            };
            aqr_tensors::formula::encode_sealing(d, p.opt.threads, segment, &made, &mut seal)
        }
        None => aqr_tensors::formula::encode_in(d, p.opt.threads, segment),
    }
}

/// The formula that the recompressor of `format` makes of `d` (its `formula` module, see its
/// FORMAT.md), if it makes one, where it is a function of `d` alone (it looks for no files in
/// its data, and takes nothing of a budget) and of what `p` gives: `None` for the other formats
/// ([`Recompressor::Cx`]). Whether coding pays off is the recompressor's call: the streams are
/// not compressed yet, so their size says little. The formula is the same whatever the threads.
/// Text is coded as `p.once` tells.
pub(crate) fn pure(format: Format, d: &[u8], p: &Pure) -> Option<Option<Formula>> {
    let r = recompressor(format);
    if let Recompressor::Cx(_) = r {
        return None;
    }
    if !fits(format, d.len()) {
        return Some(None);
    }
    #[cfg(test)]
    tests::PURE.with(|n| n.set(n.get() + 1));
    let code = || {
        #[cfg(test)]
        if format == Format::Text {
            tests::TEXTS.lock().unwrap().push(d.len());
        }
        attempted(format, d, || match r {
            Recompressor::Pure(e) => e(d, p.opt.threads),
            Recompressor::PureWith(e) => e(d, p),
            Recompressor::Cx(_) => None,
        })
    };
    Some(if format == Format::Text { p.once.code(d, code) } else { code() })
}

/// What `make`, the recompressor of `format` on `d`, makes, within the heap one recompression
/// may take ([`aqr_common::allowance::attempt`] of [`aqr_common::budget::attempt_limit`], which
/// those it makes of the contents share): `None` where it gave up past it, or panicked
/// ([`caught`]), which is told ([`fallbacks`]), as if it did not take the data.
pub(crate) fn attempted(format: Format, d: &[u8], make: impl FnOnce() -> Option<Formula>) -> Option<Formula> {
    let limit = aqr_common::budget::attempt_limit();
    #[cfg(test)]
    let (limit, hog) = tests::hog(format, d.len()).map_or((limit, false), |l| (l, true));
    #[cfg(test)]
    let bug = tests::panics(format, d.len());
    let make = || {
        #[cfg(test)]
        if bug {
            panic!("a bug of the recompressor");
        }
        // (more than the limit taken at the start, a reservation of no pages touched, and the
        // check after it, as the next item on the pool would)
        #[cfg(test)]
        let _hog = hog.then(|| {
            let v = Vec::<u8>::with_capacity(limit as usize + 1);
            aqr_common::allowance::check();
            v
        });
        make()
    };
    match caught(format.name(), d.len(), || aqr_common::allowance::attempt(limit, make)) {
        Some(Ok(f)) => f,
        Some(Err(e)) => {
            memory::gave_up_heap(format.name(), e.used, d.len() as u64);
            None
        }
        None => None,
    }
}

/// What `f`, a recompression as `what` (a format's name) of data of `bytes` bytes, makes: `None`
/// where it panicked, which is told ([`memory::panicked`]) as a bug of the recompressor, the data
/// kept as it is.
pub(crate) fn caught<R>(what: &'static str, bytes: usize, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(p) => {
            let message = match p.downcast::<String>() {
                Ok(s) => *s,
                Err(p) => p.downcast_ref::<&str>().map_or("a panic of no message", |s| s).to_string(),
            };
            memory::panicked(what, message, bytes as u64);
            None
        }
    }
}

/// aqr-bz2's formula of `d` (`depth` formats deep), each stream taken only where its data may
/// pay ([`probe`]; `held` telling what the files archived with it hold): not compressed again
/// to check it where it would not pay, which costs most (some 12 MB/s of data a thread), nor
/// decoded past its start (50-80 MB/s) where that start tells. Data that starts as a file of a
/// format (a tar archive, say), whose files further on or what they share the start does not
/// tell, is decoded whole first; other data (text, say) is told by its start. The record is the
/// params, a few bytes.
pub(crate) fn bz2(d: &[u8], opt: &Options, depth: u32, held: Option<&Held>) -> Option<Formula> {
    aqr_bz2::formula::encode_if(d, opt.threads, &mut bz2_pays(d, opt, depth, held))
}

/// Whether a bzip2 stream in file `d` pays, as aqr-bz2's `encode_if` asks: see [`bz2`].
fn bz2_pays<'a>(
    d: &'a [u8],
    opt: &Options,
    depth: u32,
    held: Option<&'a Held>,
) -> impl FnMut(usize, &[u8], bool) -> bool + 'a {
    let probe = probe(opt, depth);
    // (an encoder of its own, as formulas of pure formats are made on several threads)
    let enc = aqr_archive::Encoder::new(opt.level);
    move |coded, data, whole| {
        if !whole && detect(data).is_some_and(|f| f != Format::Text) {
            return true;
        }
        probe.as_ref().is_none_or(|p| {
            let held = || held.is_some_and(|h| h.holds(data, h.owner(d))) as u8 as f64;
            p.pays(&enc, coded, || Some(16), data, held, Kind::Unknown)
        })
    }
}

/// aqr-alz's formula of an ALZ archive, the contents of its entries a content of the
/// applications (or of the formula, those stored), recompressed in turn, each by its kind by its
/// name ([`kind_by_name`]). Its DEFLATE and bzip2 bodies decode within what is left of the
/// budget ([`Options::expansion`]); a bzip2 body is taken where its data may pay, as a bzip2
/// file is ([`bz2`]).
pub(crate) fn alz(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let profile = opt.flate_profile();
    let o = aqr_alz::formula::Options {
        threads: opt.threads,
        profile,
        zlib: aqr_flate::ZLIB.get(),
        limit: cx.inflate_left(),
        kind: kind_by_name,
    };
    let e = aqr_alz::formula::encode_with(d, &o, &mut bz2_pays(d, opt, depth, cx.held.as_deref()))?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-isc's formula of an InstallShield cabinet, the contents of its files a content of the
/// application, recompressed in turn. Its chunks decode within what is left of the budget
/// ([`Options::expansion`]).
pub(crate) fn isc(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let profile = opt.flate_profile();
    let o = aqr_isc::formula::Options {
        threads: opt.threads,
        profile,
        zlib: aqr_flate::ZLIB.get(),
        limit: cx.inflate_left(),
    };
    let e = aqr_isc::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-egg's formula of an EGG archive, the contents of its files a content of the
/// applications (or of the formula, those stored), recompressed in turn, each by its kind by its
/// name ([`kind_by_name`]). Its DEFLATE, bzip2 and LZMA blocks decode within what is left of the
/// budget ([`Options::expansion`]); a bzip2 block is taken where its data may pay, as a bzip2
/// file is ([`bz2`]).
pub(crate) fn egg(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let profile = opt.flate_profile();
    let o = aqr_egg::formula::Options {
        threads: opt.threads,
        profile,
        zlib: aqr_flate::ZLIB.get(),
        limit: cx.inflate_left(),
        kind: kind_by_name,
    };
    let e = aqr_egg::formula::encode_with(d, &o, &mut bz2_pays(d, opt, depth, cx.held.as_deref()))?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-lha's formula of an LHA archive, the contents of its files a content of the application
/// (or of the formula, those stored), recompressed in turn, each by its kind by its name
/// ([`kind_by_name`]). Its streams decode within what is left of the budget
/// ([`Options::expansion`]).
pub(crate) fn lha(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let o = aqr_lha::formula::Options { threads: opt.threads, limit: cx.inflate_left(), kind: kind_by_name };
    let e = aqr_lha::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-rar's formula of a RAR archive, the contents of its files a content of the application
/// (or of the formula, those stored), recompressed in turn, each by its kind by its name
/// ([`kind_by_name`]). Its streams decode within what is left of the budget
/// ([`Options::expansion`]); those with PPMd blocks only from level 9 ([`ppmd_on`]).
pub(crate) fn rar(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let o = aqr_rar::formula::Options {
        threads: opt.threads,
        limit: cx.inflate_left(),
        kind: kind_by_name,
        ppmd: ppmd_on(opt),
    };
    let e = aqr_rar::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-7z's formula, the contents of the files a content of the applications (or of the formula,
/// those of stored folders), recompressed in turn, each by its kind by its name
/// ([`kind_by_name`]).
pub(crate) fn sevenz(d: &[u8], opt: &Options) -> Option<Formula> {
    let o = aqr_7z::formula::Options { threads: opt.threads, kind: kind_by_name, ..Default::default() };
    aqr_7z::formula::encode_with(d, &o)
}

/// aqr-mszip's formula of a CAB file, the contents of the files a content of the applications,
/// recompressed in turn, each by its kind by its name ([`kind_by_name`]). Its MSZIP and LZX
/// folders (the latter from level 9, [`lzx_on`]) inflate within what is left of the budget
/// ([`Options::expansion`]).
pub(crate) fn cab(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let profile = opt.flate_profile();
    let o = aqr_mszip::formula::Options {
        threads: opt.threads,
        profile,
        zlib: aqr_flate::ZLIB.get(),
        limit: cx.inflate_left(),
        kind: kind_by_name,
        lzx: lzx_on(opt),
    };
    let e = aqr_mszip::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-mszip's formula of a CHM file, the files of its LZX section a content of the application,
/// recompressed in turn, each by its kind by its name; within what is left of the budget, from
/// level 9 ([`lzx_on`], [`format_on`]).
pub(crate) fn chm(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let o = aqr_mszip::formula::Options {
        threads: opt.threads,
        limit: cx.inflate_left(),
        kind: kind_by_name,
        ..Default::default()
    };
    let e = aqr_mszip::formula::encode_chm_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-pdf's options at the level of `opt`: the records of its DEFLATE streams of the profile
/// that [`Options::flate`] tells, and from level 7 on with no time budget, as DEFLATE is always
/// rebuilt there ([`probe`]).
pub(crate) fn pdf_options(opt: &Options) -> aqr_pdf::encode::Options {
    let profile = opt.flate_profile();
    let default = aqr_pdf::encode::Options::default();
    let speed = if opt.level.scale() >= 7 { f64::INFINITY } else { default.speed };
    let verify = aqr_pdf::encode::VERIFY.get().then_some::<aqr_pdf::encode::Eval>(|f, m| f.eval(&m));
    aqr_pdf::encode::Options { speed, profile, zlib: aqr_flate::ZLIB.get(), threads: opt.threads, verify, ..default }
}

/// Whether `format` is one of [`pure`].
fn is_pure(format: Format) -> bool {
    !matches!(recompressor(format), Recompressor::Cx(_))
}

/// How the recompressor of a format is run.
#[derive(Clone, Copy)]
pub(crate) enum Recompressor {
    /// A [`pure`] one, its `encode`: the formula of the data, on so many threads.
    Pure(fn(&[u8], usize) -> Option<Formula>),
    /// A [`pure`] one taking more of what [`Pure`] gives than the threads.
    PureWith(fn(&[u8], &Pure) -> Option<Formula>),
    /// One that looks for files in its data or takes of a budget, given the context: of the data,
    /// the kind of its contents (of a zlib stream or gzip member without a name), the options and
    /// how many formats deep it is.
    Cx(fn(&[u8], Kind, &Options, u32, &Cx) -> Option<Formula>),
}

/// The formula of a disk image: of a compressed container by its chunks, its disk a content (as a
/// DMG); else of its file systems, the Splits of files in pieces recorded, their contents at
/// hand (see `chunked`).
pub(crate) fn disk(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    if let Some(f) = chunked::container(d, opt, cx) {
        return Some(f);
    }
    let f = bundle::disk(d, opt.threads)?;
    if let Some(pre) = &cx.pre {
        pre.learn_splits(&f, opt.threads);
    }
    Some(f)
}

/// The formula of a SQLite database ([`Format::Sqlite`], aqr-sqlite's), but of one longer than the
/// bound of an application: `sqlite-recons` is a whole module, its plan more than the database it
/// makes (some 5 times it), and that much heap is not taken to have it turned down then.
pub(crate) fn sqlite(d: &[u8], p: &Pure) -> Option<Formula> {
    if d.len() as u64 > memory::bound_of(p.opt.application_bound, aqr_decompress::SQLITE) {
        return None;
    }
    aqr_sqlite::formula::encode(d, p.opt.threads)
}

/// The formula of a SQLite database whose blobs are Zstandard frames ([`Format::SqliteZstd`]):
/// those that libzstd's emulation makes again at level 3, of aqr-zstd's `zstd-frames`.
pub(crate) fn sqlite_zstd(d: &[u8], threads: usize) -> Option<Formula> {
    let blobs = aqr_sqlite::blobs::blobs(d, 8, &|h| h.starts_with(&aqr_zstd::MAGIC))?;
    // (Distant Horizons' database: its coarser levels made of the finer, where it has them)
    if let Some(f) = aqr_minecraft::lods_formula::encode(d, &blobs, threads) {
        return Some(f);
    }
    let frames: Vec<_> = blobs.into_iter().map(|b| b.pieces).collect();
    aqr_zstd::formula::frames_in(d, &frames, 3, Kind::Table, threads)
}

aqr_common::zopt! {
    /// Whether the estimate of the heap turns inputs down ([`fits`]); off (`-Z test:no-estimate`),
    /// everything fits, to see the limit hold as it runs.
    static ESTIMATE: Opt<bool> =
        Opt::flag("test:estimate", true, "inputs turned down by their heap's estimate (off: to see the limit hold as it runs)");
}

/// The recompressor of `format` ([`formats::Spec::recompressor`]).
pub(crate) fn recompressor(format: Format) -> Recompressor {
    format.spec().recompressor
}

/// Whether recompressing `len` bytes as `format` takes no more heap by its estimate
/// ([`formats::Spec::heap`]) than one recompression may ([`aqr_common::budget::attempt_limit`]: what
/// `aqr -M` asks for, else the budget of the process or half the system's memory where more),
/// which it is then held to as it runs ([`attempted`]). The estimate turns down up front what
/// would give up after much of the work. (With [`ESTIMATE`] off (`-Z test:no-estimate`),
/// everything fits, so that what is held to the limit as it runs can be seen.)
pub(crate) fn fits(format: Format, len: usize) -> bool {
    !ESTIMATE.get() || fits_within(format, len, aqr_common::budget::attempt_limit())
}

pub(crate) fn fits_within(format: Format, len: usize, limit: u64) -> bool {
    format.heap() * len as f64 <= limit as f64
}

/// Makes the formulas of the files of [`pure`] formats among the contents of `f` beforehand,
/// at once (as [`contents`] would take them in turn), into [`Cx::ready`]; returns their keys,
/// and the plan of the sets of the PNG files among them ([`sets::plan`]), whose members with
/// a parent are not made but left out of the contents ([`Cx::skip`]).
pub(crate) fn prepare(f: &Formula, opt: &Options, depth: u32, cx: &Cx) -> (Vec<(usize, usize, Format)>, sets::Plan) {
    let mut jobs = Vec::new();
    for n in shared_nodes(f).iter().chain([&f.root]) {
        pure_contents(n, f, opt, &mut jobs);
    }
    if jobs.len() < 2 {
        return Default::default();
    }
    // (PNG files all of their contents, as sets take them)
    let pngs: Vec<_> = jobs.iter().filter(|j| j.0 == Format::Png).filter_map(|j| Some((j.1, j.2?))).collect();
    let plan = sets::plan(&pngs, opt.threads, cx.set_limit(depth));
    let skip: HashSet<sets::Key> = plan.children().collect();
    jobs.retain(|j| !(j.0 == Format::Png && skip.contains(&(j.1.as_ptr() as usize, j.1.len()))));
    cx.skip.borrow_mut().extend(skip);
    let workers = opt.threads.min(jobs.len()).max(1);
    // (each may take all of `opt.threads`: the pool gives it the workers free, and those given
    // back as the others finish)
    let words = cx.words.borrow().clone();
    let p =
        Pure { opt, depth, words: words.as_deref(), once: &cx.once, held: cx.held.as_deref(), seal: None, pre: None };
    let files = cx.files.as_deref();
    let made = aqr_common::pool::par_map(jobs.len(), workers, |i| {
        files.inspect(|f| f.tell(jobs[i].1.as_ptr(), false));
        let _item = aqr_common::trace::item(files.and_then(|f| f.content(jobs[i].1.as_ptr())));
        let mut made = pure(jobs[i].0, jobs[i].1, &p).flatten();
        // (of the store as it is made, as [`recompress_with`] has a formula: they are all made
        // before any is taken)
        made.iter_mut().for_each(aqr_formula::store::spill_streams);
        files.inspect(|f| f.tell(jobs[i].1.as_ptr(), true));
        made
    });
    let mut ready = cx.ready.borrow_mut();
    let keys: Vec<_> = jobs.iter().map(|&(format, d, _)| (d.as_ptr() as usize, d.len(), format)).collect();
    for (&k, f) in keys.iter().zip(made) {
        ready.insert(k, f);
    }
    (keys, plan)
}

/// The files of [`pure`] formats below node `n` of `f` that [`contents`] would take as such
/// (those of a kind of the format, or that a scan finds at the start), each with its data as it
/// is given its recompressor (to its [`extent`]), and its slice where that is all of the content.
pub(crate) fn pure_contents<'a>(
    n: &Node,
    f: &'a Formula,
    opt: &Options,
    out: &mut Vec<(Format, &'a [u8], Option<sets::At>)>,
) {
    let mut found = Vec::new();
    pure_candidates(n, f, &mut found);
    // (the scans at once: of an image's thousands of files, they took a second alone)
    let found = aqr_common::pool::par_map(found.len(), opt.threads, |i| {
        let (nest, d, at) = found[i];
        // (a scan takes the file it starts with as one of the format it starts like)
        let format = match nest {
            Nest::As(format) => Some(detect::named(format, d)),
            Nest::Scan => detect(d),
            _ => None,
        };
        let format = format.filter(|&f| is_pure(f) && format_on(f, opt))?;
        let n = extent(format, d).filter(|&n| n > 0)?;
        Some((format, &d[..n], (n == d.len()).then_some(at)))
    });
    out.extend(found.into_iter().flatten());
}

/// The slices [`pure_contents`] looks at below node `n` of `f`, each with how it is taken (its
/// stream's kind's nest) and its data.
fn pure_candidates<'a>(n: &Node, f: &'a Formula, out: &mut Vec<(Nest, &'a [u8], sets::At)>) {
    match n {
        &Node::Slice { stream, start, len } if is_content(f, stream) => {
            let Some(s) = f.streams.get(stream).filter(|s| !s.data.is_lazy()) else { return };
            let Some(d) = s.data.get(start as usize..).and_then(|d| d.get(..len as usize)) else { return };
            if let Some(nest) = Nest::Scan.of(s.kind) {
                out.push((nest, d, (stream, start, len)));
            }
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| pure_candidates(n, f, out)),
        Node::Apply(a) => a.inputs.iter().for_each(|n| pure_candidates(n, f, out)),
        _ => {}
    }
}

/// Bytes of the streams of `f` by class, largest first.
pub fn classes(f: &Formula) -> Vec<(&str, usize)> {
    let mut out: Vec<(&str, usize)> = Vec::new();
    for Stream { class, data, .. } in &f.streams {
        match out.iter_mut().find(|(c, _)| c == class) {
            Some((_, n)) => *n += data.len(),
            None => out.push((class, data.len())),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out
}
