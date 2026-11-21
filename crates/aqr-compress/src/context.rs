//! What recompressing a file carries along ([`Cx`]): the encoder sizing formulas, the bytes
//! that coded streams may still inflate to, what was undone and made once.

use super::*;

/// What recompressing a file carries along: the encoder sizing formulas, and the bytes that
/// DEFLATE, LZFSE and LZVN streams may still inflate to ([`Options::expansion`]).
pub(crate) struct Cx {
    pub(crate) enc: Encoder,
    pub(crate) inflate: Cell<u64>,
    /// Where the formulas recompressors make are evaluated beforehand, for [`compress`].
    pub(crate) pre: Option<Arc<verify::Prevalidator>>,
    /// Formulas made beforehand (of [`pure`] formats), by the data (where it is, its length)
    /// and the format.
    pub(crate) ready: RefCell<HashMap<(usize, usize, Format), Option<Formula>>>,
    /// The prevalidation of those taken, by where their data is: put off until it is known
    /// whether they are taken into a set ([`sets`]), whose files are not evaluated alone.
    pub(crate) deferred: RefCell<Vec<(usize, verify::Deferred)>>,
    /// Where the contents made apart of the formula of this data are sealed, if they are
    /// ([`seal`]): those of a formula whose formulas above all make their files apart, of a
    /// file archived alone. Taken by what may use it: [`recompress_with`], for [`nested`] only
    /// where its formula makes its files apart, and [`find`], only for a file that is all of
    /// the data (else the formula of the scan, or of what holds it, is compared with its data,
    /// [`smaller`], which reads the streams of the formulas sealed: they are no longer at hand).
    pub(crate) sealing: RefCell<Option<seal::Sealing>>,
    /// Whether [`make_later`] had the applications of contents of data not made yet evaluated
    /// as it made them ([`recompress_with`] evaluates the formula otherwise).
    pub(crate) parts_checked: std::cell::Cell<bool>,
    /// The contents planned to be coded in a set against a parent ([`sets::plan`]), which are
    /// not made alone.
    pub(crate) skip: RefCell<HashSet<sets::Key>>,
    /// For the files of an archive (the contents of a formula 0 formats deep, `archive`), the
    /// most bytes of files of a set ([`Options::set_bytes`]).
    pub(crate) top_sets: Option<usize>,
    /// The files of an archive to make (each its index among the contents, its slice and how),
    /// at once once all are known ([`nested`]).
    pub(crate) later: RefCell<Vec<(usize, Node, Nest)>>,
    /// What the files archived together hold ([`Held`]), for an archive of several.
    pub(crate) held: Option<Arc<Held>>,
    /// The DEFLATE streams of the files archived together, kept as they are, that files holding
    /// their data are made of ([`inflated`]), for an archive of several below level 4.
    pub(crate) bodies: Option<Arc<inflated::Bodies>>,
    /// The files archived together that `--lossy` rewrote ([`crate::lossy::Rewritten`]), whose
    /// DEFLATE streams are zlib's at level 6, coded so.
    pub(crate) rewritten: Option<Arc<crate::lossy::Rewritten>>,
    /// The remaps of the ARM64 and x86 images of the Mach-O and ELF files archived together
    /// ([`exe::plan`]).
    pub(crate) remaps: Option<Arc<exe::Remaps>>,
    /// The dictionary that the text archived together is coded with (aqr-wrt), for an archive
    /// of several: made of the files that are text, so that text holding other text codes to
    /// what holds the other's code; and for the contents of a formula of JSON lines, its
    /// segments' text ([`segment_words`]). Else each text has its own.
    pub(crate) words: RefCell<Option<Arc<aqr_wrt::formula::Shared>>>,
    /// The text coded with [`Cx::words`] of data the files archived together hold, by its
    /// length and hash: the text that holds it codes to what holds this, so that it costs
    /// nothing as that data does ([`smaller`]). So are the streams of the members of an ar, tar
    /// or cpio archive that they hold ([`Cx::note_bundled`]), and those of the formula of any
    /// data they hold ([`Cx::note_held`]). A context made apart (a file of an archive, a
    /// content made on a worker) starts with those noted before it was let go ([`Apart`]), and
    /// what it notes is taken back once it and those made beside it are done
    /// ([`Cx::adopt_notes`]): not shared as they are made, so that what one sees of another
    /// does not depend on which of them a worker took first (the archive on the threads).
    pub(crate) coded_held: RefCell<HashSet<(usize, u64)>>,
    /// The text of the files of an archive that others hold as contents, coded once ([`Once`]).
    /// Shared with the contexts of the files made apart.
    pub(crate) once: Arc<Once>,
    /// The files of an archive (the contents of a formula 0 formats deep), to tell as they are
    /// made. Not shared with the contexts of the files.
    pub(crate) files: Option<Arc<progress::Files>>,
    /// The files of no header to take apart by others archived along (a VPK's archives, Unity's
    /// resource files), by where their data is ([`siblings::plan`]). Shared with the contexts of
    /// the files.
    pub(crate) siblings: Option<Arc<siblings::Placed>>,
    /// The decodings that the recompressor of this data asks to be undone where nothing is
    /// made of their data ([`Undo`]): taken by [`recompress_with`] as the recompressor gives its
    /// formula, those of formulas made within it put aside meanwhile.
    pub(crate) undo: RefCell<Vec<Undo>>,
    /// Whether a formula that loses to its data by little may be kept beside it ([`Verdict::Close`]):
    /// where what takes it puts both in place ([`made_deferring`]).
    pub(crate) defers: Cell<bool>,
    /// The data (where it is, its length) whose formula [`recompress_with`] is to tell of as
    /// [`Verdict::Close`] where it is, telling so in [`Cx::closed`]: its caller's, which puts both
    /// in place.
    pub(crate) defer_for: Cell<Option<(usize, usize)>>,
    /// The data last told of so, and the sizes of [`Verdict::Close`].
    pub(crate) closed: Cell<Option<(Span, (usize, usize))>>,
    /// Bytes of the formulas kept beside their data so far, of at most [`Cx::defer_budget`].
    pub(crate) kept_beside: Cell<usize>,
    pub(crate) defer_budget: usize,
}

/// A decoding a recompressor asks to be undone ([`undo`]): the application at `coded` of the data,
/// of its formula's root, whose data is the content stream `stream` (its record `record`
/// bytes): put back as the bytes of `coded` (a stream of class `kept`) where nothing is made of
/// the content, and its data and record would not archive in less than those bytes (aqr-formula's
/// flat probe). An Inno Setup installer's LZMA stream split at level 7 or more, where nothing
/// probes it beforehand, would otherwise leave its data as it is in the formula, larger than
/// the stream, and the installer be kept whole as it is.
pub(crate) struct Undo {
    pub stream: usize,
    pub coded: Range<usize>,
    pub record: usize,
    pub kept: &'static str,
    /// Kept beside the coded bytes whatever is made of its data, for the batch to choose
    /// ([`Node::Either`], `choose`): a split that a probe turned down, taken still where that
    /// may be ([`Cx::defers`]).
    pub defer: bool,
}

/// Text coded once, for the files of an archive that other files of it hold as the contents of
/// their entries or members: the second to code a text of one takes the formula the first made,
/// kept for it, rather than coding the text again; nor is that formula evaluated beforehand
/// again ([`Once::evaluated`]). Nor is the first's where the text is large: what it would keep
/// is only that its output is the file, which decoding the archive takes for one of the two
/// alike applications only, reading it as much as running the module gives; it would only
/// hold the output while the files are recompressed (10gb: `enwik9`, 1 GB).
#[derive(Default)]
pub(crate) struct Once {
    /// By the length and XXH3-128 of the text.
    slots: HashMap<(usize, u128), std::sync::Mutex<Slot>>,
    /// The lengths of the texts of `slots`: a text of another length is not hashed.
    lens: HashSet<usize>,
    /// Where the texts whose formulas are not evaluated beforehand are (address and length).
    copies: std::sync::Mutex<HashSet<(usize, usize)>>,
}

enum Slot {
    Empty,
    Kept(Option<Formula>),
    Taken,
}

aqr_common::zopt! {
    /// Texts longer than this are not evaluated beforehand ([`Once::evaluated`]).
    pub(crate) static ONCE_LARGE: Opt<usize> =
        Opt::size("once-large", 64 << 20, "texts longer, coded once for several, are not evaluated beforehand");
}

impl Once {
    /// For the texts of these lengths and XXH3-128s.
    pub(crate) fn of(texts: impl IntoIterator<Item = (usize, u128)>) -> Once {
        let slots: HashMap<_, _> = texts.into_iter().map(|k| (k, std::sync::Mutex::new(Slot::Empty))).collect();
        let lens = slots.keys().map(|k| k.0).collect();
        Once { slots, lens, copies: Default::default() }
    }

    /// The formula `code` makes of text `d`, or the one made of the same text before, kept.
    pub(crate) fn code(&self, d: &[u8], code: impl FnOnce() -> Option<Formula>) -> Option<Formula> {
        let slot = match self.lens.contains(&d.len()) {
            true => self.slots.get(&(d.len(), xxhash_rust::xxh3::xxh3_128(d))),
            false => None,
        };
        let Some(slot) = slot else { return code() };
        // (held as it is coded: the second waits for it)
        let mut slot = slot.lock().unwrap();
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Empty => {
                let mut f = code();
                // (kept without a copy of its streams)
                f.iter_mut().flat_map(|f| &mut f.streams).for_each(|s| s.data.share());
                *slot = Slot::Kept(f.clone());
                if d.len() > ONCE_LARGE.get() {
                    self.copies.lock().unwrap().insert((d.as_ptr() as usize, d.len()));
                }
                f
            }
            Slot::Kept(f) => {
                self.copies.lock().unwrap().insert((d.as_ptr() as usize, d.len()));
                f
            }
            Slot::Taken => code(),
        }
    }

    /// Whether the formula of text `d` ([`Once::code`]) is to be evaluated beforehand.
    pub(crate) fn evaluated(&self, d: &[u8]) -> bool {
        !self.copies.lock().unwrap().contains(&(d.as_ptr() as usize, d.len()))
    }
}

/// `opt`, but [`Options::verify`] only where data is recompressed ([`Options::recompresses`]):
/// below level 2 no module is applied, and what Zstandard makes of the data in memory is
/// trusted.
pub(crate) fn verifying(opt: &Options) -> Options {
    Options { verify: opt.verify && opt.recompresses(), ..opt.clone() }
}

/// The encoder of formulas made with `opt`.
pub(crate) fn encoder(opt: &Options) -> Encoder {
    let mut enc = Encoder::with_threads(opt.level, opt.threads)
        .with_modules(opt.modules)
        .with_dictionaries(opt.dictionaries)
        .with_size_hint(opt.size_hint);
    if opt.shared_resources {
        enc.share_resources();
    }
    if opt.shared_modules {
        enc.share_modules();
    }
    enc
}

impl Cx {
    pub(crate) fn new(d: &[u8], opt: &Options) -> Cx {
        Cx::of_len(d.len(), opt)
    }

    /// A context of data of `n` bytes.
    pub(crate) fn of_len(n: usize, opt: &Options) -> Cx {
        let inflate = opt.expansion.saturating_mul(n as u64).saturating_add(EXPANSION_SLACK.get() as u64);
        Cx {
            enc: encoder(opt),
            inflate: Cell::new(inflate),
            pre: None,
            ready: Default::default(),
            deferred: Default::default(),
            sealing: Default::default(),
            parts_checked: Default::default(),
            skip: Default::default(),
            top_sets: None,
            later: Default::default(),
            held: None,
            bodies: None,
            rewritten: None,
            remaps: None,
            words: Default::default(),
            coded_held: Default::default(),
            once: Default::default(),
            files: None,
            siblings: None,
            undo: Default::default(),
            defers: Cell::new(false),
            defer_for: Cell::new(None),
            closed: Cell::new(None),
            kept_beside: Cell::new(0),
            defer_budget: (n as f64 * DEFER_BUDGET.get().max(0.0)) as usize,
        }
    }

    /// The aqr-flate profile and hints that a DEFLATE stream of `d` is deconstructed at where
    /// it is in a file that `--lossy` rewrote, whose streams are zlib's
    /// ([`crate::lossy::Rewritten::flate`]); None where it is as [`Options::flate_profile`] tells.
    pub(crate) fn rewritten(&self, d: &[u8], opt: &Options) -> Option<(u16, aqr_flate::Hints)> {
        self.rewritten.as_ref()?.flate(d, opt)
    }

    /// The most bytes of files of a set of the contents of a formula `depth` formats deep.
    pub(crate) fn set_limit(&self, depth: u32) -> usize {
        match self.top_sets {
            Some(n) if depth == 0 => n,
            _ => usize::MAX,
        }
    }

    /// Takes `n` bytes of inflated data from the budget, if there are as many left.
    pub(crate) fn inflate(&self, n: u64) -> bool {
        let left = self.inflate.get();
        let fits = n <= left;
        if fits {
            self.inflate.set(left - n);
        }
        fits
    }

    pub(crate) fn inflate_left(&self) -> u64 {
        self.inflate.get()
    }

    /// Whether the files archived together hold `data` ([`Held`]), but for the one `within`
    /// is part of.
    pub(crate) fn held_by_others(&self, data: &[u8], within: &[u8]) -> bool {
        self.held.as_ref().is_some_and(|h| h.holds(data, h.owner(within)))
    }

    /// Codes the text archived with `words` ([`Cx::words`]), their resources sized as nothing.
    pub(crate) fn share_words(&self, words: Option<Arc<aqr_wrt::formula::Shared>>) {
        for r in words.iter().flat_map(|w| w.resources()) {
            self.enc.hold_resource(r);
        }
        *self.words.borrow_mut() = words;
    }

    /// Notes the text coded by formula `f` of text `d` ([`Cx::coded_held`]), where it is coded
    /// with [`Cx::words`] and the files archived together hold `d`.
    pub(crate) fn note_coded(&self, f: &Formula, d: &[u8]) {
        let words = self.words.borrow();
        let Some(w) = words.as_deref() else { return };
        let shared = f.applies().iter().any(|a| a.resources.first().is_some_and(|r| w.resources().any(|w| w == r)));
        if shared && f.streams.len() == 1 && self.held_by_others(d, d) {
            self.coded_held.borrow_mut().insert(stream_key(&f.streams[0].data));
        }
    }

    /// Notes the streams of formula `f` of an ar, tar or cpio archive that the files archived
    /// together hold ([`Cx::coded_held`]), but its own (its headers): those of its members, which
    /// are the files', so that what holds the archive costs nothing for them either.
    pub(crate) fn note_bundled(&self, f: &Formula) {
        self.note_streams(f, |s| !bundle::own_class(&s.class));
    }

    /// Notes the streams of formula `f` of data that the files archived together hold
    /// ([`Cx::coded_held`]): the file that holds it has the same formula, whose streams are
    /// stored once (as text is coded alike, [`Cx::note_coded`]), so that what holds the data
    /// (a tar archive the files do not hold all of, say) costs nothing for them either.
    pub(crate) fn note_held(&self, f: &Formula) {
        self.note_streams(f, |_| true);
    }

    pub(crate) fn note_streams(&self, f: &Formula, take: impl Fn(&aqr_formula::Stream) -> bool) {
        let mut held = self.coded_held.borrow_mut();
        for s in f.streams.iter().filter(|s| take(s) && !s.data.is_empty()) {
            held.insert(stream_key(&s.data));
        }
    }

    /// Whether `data` is a stream noted by [`Cx::note_coded`], [`Cx::note_bundled`] or
    /// [`Cx::note_held`].
    pub(crate) fn coded_held(&self, data: &[u8]) -> bool {
        let held = self.coded_held.borrow();
        !held.is_empty() && held.contains(&stream_key(data))
    }

    /// Takes the notes of a context made apart ([`Cx::coded_held`]), once it and those made
    /// beside it are done.
    pub(crate) fn adopt_notes(&self, notes: HashSet<(usize, u64)>) {
        self.coded_held.borrow_mut().extend(notes);
    }
}
