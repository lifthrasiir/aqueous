//! The archive encoder ([`Encoder`]): formulas laid out as blocks, each stream and resource
//! compressed once however many formulas hold it.

use super::*;

/// Encodes formulas as archives at a [`Level`], keeping the compressed streams and resources
/// by their contents: [`Encoder::len`] sizes a formula, and the same streams and resources in a
/// later formula are not compressed again.
pub struct Encoder {
    pub(crate) level: Level,
    /// Threads compressing a stream (the archive does not depend on them).
    pub(crate) threads: usize,
    /// Compressed streams (as [`compressed`] gives) by their [`StreamKey`], of the store where one
    /// is set ([`kept`]).
    pub(crate) streams: RefCell<HashMap<StreamKey, Option<aqr_formula::Bytes>>>,
    /// Whether the streams compressed are kept in [`Encoder::streams`] (not while
    /// [`Encoder::encode_pieces`] writes).
    pub(crate) keep: std::cell::Cell<bool>,
    /// Whether archives are being sized ([`Encoder::len_of`]): their slices are of blocks all of
    /// them, no literals ([`literals`]). A formula sized is sized alone, where its small
    /// streams would be literals; archived with others (embedded, or in a batch), they are
    /// merged with those of their class, which they compress better with.
    pub(crate) sizing: std::cell::Cell<bool>,
    /// Compressed resources by their [`StreamKey`].
    pub(crate) resources: RefCell<HashMap<StreamKey, Vec<u8>>>,
    /// The archived lengths ([`Encoder::raw_len`]) of streams forgotten compressed
    /// ([`Encoder::forget`]), or compared with a formula at the base level of [`Level::Of`]
    /// ([`Encoder::smaller`]), by their [`StreamKey`]: a stream is compared with formulas of it
    /// again without being compressed again.
    pub(crate) raw_lens: RefCell<HashMap<StreamKey, usize>>,
    /// Samples of streams compressed by [`Encoder::least`], by their stream's [`StreamKey`]
    /// and level, for picking the level of the stream without compressing them again.
    pub(crate) trials: RefCell<HashMap<(StreamKey, i32), Compressed>>,
    /// The Zstandard context every stream is compressed with, so that its workers and buffers
    /// are made once rather than for each stream (while they take at most [`CCTX_KEPT`]).
    pub(crate) cctx: RefCell<CCtx<'static>>,
    /// Whether formulas are sized without their resource blocks ([`Encoder::share_resources`]).
    pub(crate) shared: bool,
    /// Whether archives begin with [`SIZE_HINT_PLACEHOLDER`] ([`Encoder::with_size_hint`]).
    pub(crate) size_hint: bool,
    /// Whether formulas are sized without their module blocks ([`Encoder::share_modules`]).
    pub(crate) shared_modules: bool,
    /// The resources that formulas are sized without ([`Encoder::hold_resource`]).
    pub(crate) held: RefCell<HashSet<StreamKey>>,
    /// The Wasm module of each module ([`Encoder::with_modules`]).
    pub(crate) wasm: WasmOf,
    /// Module blocks compressed, by their module and level.
    pub(crate) module_blocks: RefCell<HashMap<(Module, i32), ModuleBlock>>,
    /// Told of the streams compressed at once ([`Encoder::on_precompress`]).
    pub(crate) progress: Option<Progress>,
    /// The zstd level each stream compressed by [`Encoder::precompress`] was compressed at, by
    /// its [`StreamKey`] (for the dictionaries of a batch, [`dict`]).
    pub(crate) levels: RefCell<HashMap<StreamKey, i32>>,
    /// Whether a batch's blocks alike are compressed with dictionaries trained for them
    /// ([`Encoder::with_dictionaries`]).
    pub(crate) trains: bool,
}

/// The dictionaries of a batch where its blocks take them, and the index of their first in
/// the segment ([`Encoder::content_block`]).
pub(crate) type Dicts<'a> = Option<(&'a dict::Planned, usize)>;

/// What [`Encoder::precompress`] tells as it compresses: the bytes of the streams compressed so
/// far, of all of them, from any of its threads.
pub type Progress = std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>;

/// A module block compressed, shared by the formulas that have it.
pub(crate) type ModuleBlock = std::sync::Arc<Vec<u8>>;

/// The Wasm module of a module, if there is one ([`Encoder::with_modules`]).
pub type WasmOf = fn(Module) -> Option<Wasm>;

/// A Wasm module as an encoder writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wasm {
    /// Its bytes, compressed into its module block.
    Bytes(Cow<'static, [u8]>),
    /// The data of its module block made beforehand ([`module_block_data`]), written as it is.
    Block(Cow<'static, [u8]>),
    /// Its SHA-512/256, written as a shorthand module block (32 bytes, no module bytes): the
    /// decompressor reconstructs the module from its own registry (spec, "Registered dictionaries
    /// and modules"). The module must be registered, or the archive cannot be decoded.
    /// `classes` is how many classes of tasks the module has ([`task_classes`] of its bytes, which
    /// the encoder does not have): an Apply declares a task limit for each.
    Registered { hash: [u8; 32], classes: usize },
}

/// The zstd level of the module blocks made beforehand: modules are made once and archived
/// often.
pub(crate) const MODULE_LEVEL: i32 = 22;

/// The data of the module block of Wasm module `wasm`, compressed at the highest level, to be
/// made once ([`Wasm::Block`]): in the split form where the module splits and that is smaller,
/// else in the stored form.
pub fn module_block_data(wasm: &[u8]) -> Result<Vec<u8>> {
    let stored = always_compressed(&mut CCtx::create(), &[wasm], MODULE_LEVEL, 1)?;
    match aqr_common::wasm::to_split_form(wasm) {
        Some(split) => {
            let split = always_compressed(&mut CCtx::create(), &[&split], MODULE_LEVEL, 1)?;
            Ok(if split.len() < stored.len() { split } else { stored })
        }
        None => Ok(stored),
    }
}

/// The Wasm module of the data of its module block ([`module_block_data`]).
pub fn module_of_block_data(d: &[u8]) -> Option<Vec<u8>> {
    module_of_decoded(stored_decoded(d)?).ok()
}

/// The Wasm module of the decoded data `d` of a module block: `d` itself in the stored form
/// (first byte 00), joined in the split form (0D), else invalid.
pub(crate) fn module_of_decoded(d: Vec<u8>) -> Decoded<Vec<u8>> {
    match d.first() {
        Some(0) => Ok(d),
        Some(&aqr_common::wasm::SPLIT_FORM) => {
            aqr_common::wasm::from_split_form(&d).ok_or_else(|| Failure::Invalid("bad split form of a module".into()))
        }
        _ => invalid("a module block of neither the stored nor the split form"),
    }
}

/// The decoded data of checksummed Zstandard data `d` (the data of a module or resource block).
pub(crate) fn stored_decoded(d: &[u8]) -> Option<Vec<u8>> {
    decoded(d, true, true).ok()
}

/// The resource whose block has data `stored` as it is (checksummed Zstandard data, as of a
/// resource or module block of another archive): [`Resource::with_stored`] of the data it
/// decodes to. Fails where `stored` is no such data.
pub fn stored_resource(stored: impl Into<Arc<[u8]>>) -> Result<Resource> {
    let stored = stored.into();
    let data = stored_decoded(&stored).ok_or_else(|| Error::new("no checksummed Zstandard data"))?;
    Ok(Resource::with_stored(data, stored))
}

/// The encoder, as a probe asks of it.
impl aqr_formula::probe::Archiver for Encoder {
    fn least(&self, d: &[u8], enough: usize) -> Result<usize> {
        Encoder::least(self, d, enough)
    }

    fn forget(&self, d: &[u8]) {
        Encoder::forget(self, d)
    }

    fn sample(&self) -> (usize, usize) {
        (SAMPLE.get(), SAMPLE_CHUNKS.get())
    }

    fn long_window(&self, len: usize) -> Option<usize> {
        LONG.get().is_some_and(|long| len > long).then_some(1 << WINDOW_LOG_MAX)
    }
}

/// Encoders at `level`, of the threads asked for, as a probe makes them
/// ([`aqr_formula::probe::Probe::archiver`]).
pub fn archivers(level: Level) -> aqr_formula::probe::Archivers {
    Arc::new(move |threads| Box::new(Encoder::with_threads(level, threads)))
}

/// A stream's (or resource's) hash and length.
pub(crate) type StreamKey = (u128, usize);

/// What [`compressed`] gives: the compressed stream, or `None` where it does not compress.
pub(crate) type Compressed = Option<Vec<u8>>;

/// A compressed stream as [`Encoder::streams`] keeps it: moved to the store where one is set
/// ([`aqr_formula::store`]), so that what is kept to be written later (all the streams of a file, as
/// its formula is sized) is not held in memory.
pub(crate) fn kept(c: Compressed) -> Option<aqr_formula::Bytes> {
    c.map(|c| {
        let mut b = aqr_formula::Bytes::from(c);
        aqr_formula::store::spill(&mut b);
        b
    })
}

/// What an [`Encoder`] kept of the streams, resources and module blocks it compressed
/// ([`Encoder::into_kept`]), without its Zstandard context.
pub struct Kept {
    pub(crate) streams: HashMap<StreamKey, Option<aqr_formula::Bytes>>,
    pub(crate) resources: HashMap<StreamKey, Vec<u8>>,
    pub(crate) raw_lens: HashMap<StreamKey, usize>,
    pub(crate) trials: HashMap<(StreamKey, i32), Compressed>,
    pub(crate) levels: HashMap<StreamKey, i32>,
    pub(crate) module_blocks: HashMap<(Module, i32), ModuleBlock>,
}

/// The Zstandard contexts of encoders alive (each [`Encoder`] has one), and the most at once
/// since [`reset_contexts_peak`]. Zstandard allocates outside the Rust heap, so that what they
/// hold (up to [`CCTX_KEPT`] each) is out of sight of the heap's accounting.
pub(crate) static CONTEXTS: (AtomicUsize, AtomicUsize) = (AtomicUsize::new(0), AtomicUsize::new(0));

/// The Zstandard contexts of encoders alive, and the most at once since
/// [`reset_contexts_peak`].
pub fn contexts() -> (usize, usize) {
    (CONTEXTS.0.load(Relaxed), CONTEXTS.1.load(Relaxed))
}

/// Starts the peak of [`contexts`] again from those alive.
pub fn reset_contexts_peak() {
    CONTEXTS.1.store(CONTEXTS.0.load(Relaxed), Relaxed);
}

impl Drop for Encoder {
    fn drop(&mut self) {
        CONTEXTS.0.fetch_sub(1, Relaxed);
    }
}

impl Encoder {
    pub fn new(level: Level) -> Encoder {
        Encoder::with_threads(level, 1)
    }

    /// Drops what is kept of stream `d` compressed (by [`Encoder::smaller`], say), where it will
    /// not be archived; only its archived length is kept, for [`Encoder::smaller`].
    pub fn forget(&self, d: &[u8]) {
        // (none of its length kept: not hashed, which reads all of it, an image of GBs mapped)
        let n = d.len();
        if !self.trials.borrow().keys().any(|t| t.0.1 == n) && !self.streams.borrow().keys().any(|k| k.1 == n) {
            return;
        }
        let k = key(&[d]);
        self.trials.borrow_mut().retain(|t, _| t.0 != k);
        if self.streams.borrow().contains_key(&k) {
            // from what is kept, compressing nothing
            let n = self.raw_len(d).expect("a stream compressed already");
            self.raw_lens.borrow_mut().insert(k, n);
            self.streams.borrow_mut().remove(&k);
        }
    }

    /// What it kept of the streams, resources and module blocks it compressed, for an encoder
    /// alike to [`adopt`](Encoder::adopt); its Zstandard context dropped with it, so that what
    /// is kept for later holds none (a context takes up to `CCTX_KEPT` outside the heap: one
    /// kept for each of the 16,421 files of a batch made apart once took 10 GB).
    pub fn into_kept(self) -> Kept {
        Kept {
            streams: self.streams.take(),
            resources: self.resources.take(),
            raw_lens: self.raw_lens.take(),
            trials: self.trials.take(),
            levels: self.levels.take(),
            module_blocks: self.module_blocks.take(),
        }
    }

    /// Keeps what an encoder alike (for data compressed into the same archive) kept of the
    /// streams, resources and module blocks it compressed ([`Encoder::into_kept`]), as if
    /// compressed here.
    pub fn adopt(&self, kept: Kept) {
        self.streams.borrow_mut().extend(kept.streams);
        self.resources.borrow_mut().extend(kept.resources);
        self.raw_lens.borrow_mut().extend(kept.raw_lens);
        self.trials.borrow_mut().extend(kept.trials);
        self.levels.borrow_mut().extend(kept.levels);
        self.module_blocks.borrow_mut().extend(kept.module_blocks);
    }

    /// Drops everything kept of the streams and resources compressed.
    pub fn clear(&self) {
        self.streams.borrow_mut().clear();
        self.levels.borrow_mut().clear();
        self.resources.borrow_mut().clear();
        self.raw_lens.borrow_mut().clear();
        self.trials.borrow_mut().clear();
    }

    /// Archives written ([`Encoder::encode_pieces_to`] and the like) beginning with a size hint
    /// block to be filled in, [`SIZE_HINT_PLACEHOLDER`] (where `on`), as an encrypted archive's
    /// payload has to ([`envelope::Sealer`]).
    pub fn with_size_hint(mut self, on: bool) -> Encoder {
        self.size_hint = on;
        self
    }

    /// An encoder compressing each stream on `threads` threads, into the same archive as on one.
    pub fn with_threads(level: Level, threads: usize) -> Encoder {
        let alive = CONTEXTS.0.fetch_add(1, Relaxed) + 1;
        CONTEXTS.1.fetch_max(alive, Relaxed);
        Encoder {
            level,
            threads: threads.max(1),
            streams: RefCell::default(),
            keep: std::cell::Cell::new(true),
            sizing: std::cell::Cell::new(false),
            resources: RefCell::default(),
            raw_lens: RefCell::default(),
            trials: RefCell::default(),
            cctx: RefCell::new(CCtx::create()),
            shared: false,
            shared_modules: false,
            held: RefCell::default(),
            wasm: |m| Some(Wasm::Bytes(Cow::Owned(stand_in(m)))),
            module_blocks: RefCell::default(),
            progress: None,
            levels: RefCell::default(),
            trains: true,
            size_hint: false,
        }
    }

    /// Tells `progress` of the streams that [`Encoder::precompress`] compresses (none: tells
    /// nothing).
    pub fn on_precompress(&mut self, progress: Option<Progress>) {
        self.progress = progress;
    }

    /// The encoder writing the Wasm module that `wasm` gives of each module into its module
    /// block (the data made beforehand where there is, else compressed at the level of the
    /// formula blocks), rather than its [`stand_in`]. Encoding fails on a module that has none.
    pub fn with_modules(mut self, wasm: WasmOf) -> Encoder {
        self.wasm = wasm;
        self.module_blocks.borrow_mut().clear();
        self
    }

    /// The classes of tasks of each of `modules` ([`task_classes`] of its Wasm module).
    pub(crate) fn classes_of(&self, modules: &[Module]) -> Vec<usize> {
        let class = |m: &Module| match (self.wasm)(*m) {
            Some(Wasm::Bytes(b)) => task_classes(&b),
            Some(Wasm::Block(d)) => module_of_block_data(&d).map_or(0, |w| task_classes(&w)),
            Some(Wasm::Registered { classes, .. }) => classes,
            None => 0,
        };
        modules.iter().map(class).collect()
    }

    /// The encoder compressing a batch's blocks alike with dictionaries trained for them where
    /// they pay ([`SegmentWriter::batch`]; `dict`), as by default, or not.
    pub fn with_dictionaries(mut self, on: bool) -> Encoder {
        self.trains = on;
        self
    }

    /// Checks bytes `d` that a [`Node::ModuleBytes`] takes of the block of module `m` against
    /// the block it writes (at `level`): the same.
    pub(crate) fn check_module_bytes(&self, m: Module, d: &[u8], level: i32) -> Result<()> {
        match self.module_block(m, level)?[..] == *d {
            true => Ok(()),
            false => Err(Error::new(format!("the bytes of a module block of {m} not its own"))),
        }
    }

    /// Appends module block `m` to `out`: a shorthand block where its Wasm is
    /// [`Wasm::Registered`], else a full module block of its bytes.
    pub(crate) fn write_module(&self, out: &mut impl Out, m: Module, level: i32) -> Result<()> {
        match (self.wasm)(m) {
            Some(Wasm::Registered { hash, .. }) => shorthand_block(out, MODULE, &hash),
            _ => block(out, MODULE, &self.module_block(m, level)?),
        }
        Ok(())
    }

    /// The data of the module block of `m`, compressed at `level`, once.
    pub(crate) fn module_block(&self, m: Module, level: i32) -> Result<ModuleBlock> {
        if let Some(b) = self.module_blocks.borrow().get(&(m, level)) {
            return Ok(b.clone());
        }
        let wasm = (self.wasm)(m).ok_or_else(|| Error::new(format!("no Wasm module of {m}")))?;
        let b = ModuleBlock::new(match wasm {
            Wasm::Block(b) => b.into_owned(),
            Wasm::Bytes(w) => always_compressed(&mut self.cctx(), &[&w], level, self.threads)?,
            // a shorthand block is written by `write_module`, not through here
            Wasm::Registered { .. } => return Err(Error::new(format!("{m} is registered, with no module block"))),
        });
        self.module_blocks.borrow_mut().insert((m, level), b.clone());
        Ok(b)
    }

    /// Sizes formulas ([`Encoder::len`], [`Encoder::smaller`]) without their resource blocks,
    /// as if the archive held them already: as in an archive of many files that share them,
    /// where they come to nothing per file. For measuring; the archives are the same.
    pub fn share_resources(&mut self) {
        self.shared = true;
    }

    /// Sizes formulas ([`Encoder::len`], [`Encoder::smaller`]) without their module blocks, as
    /// in an archive of many files, which has a module block of each module once (whose cost is
    /// then no one file's). The archives are the same.
    pub fn share_modules(&mut self) {
        self.shared_modules = true;
    }

    /// Whether formulas are sized without their module blocks ([`Encoder::share_modules`]).
    pub fn shares_modules(&self) -> bool {
        self.shared_modules
    }

    /// The bytes of the module blocks in the archive of `f`.
    pub fn modules_len(&self, f: &Formula) -> Result<usize> {
        let mut n = Count(0);
        let (modules, _) = f.blocks();
        for m in modules {
            self.write_module(&mut n, m, self.base_level())?;
        }
        Ok(n.0)
    }

    /// The bytes of the blocks that [`Encoder::len`] counts of `f` that an archive holds once
    /// however many formulas take them: its module blocks (unless [`Encoder::share_modules`])
    /// and its resource blocks (unless [`Encoder::share_resources`], nor those stored or held).
    /// Of a formula of a file archived alone, what its parts would share were they apart.
    pub fn once_len(&self, f: &Formula) -> Result<usize> {
        let modules = match self.shared_modules {
            true => 0,
            false => self.modules_len(f)? - self.module_bytes_len(f)?,
        };
        let held = self.held.borrow();
        let resources = match self.shared {
            true => 0,
            false => self.resources_len_of(f, |r| r.stored_data().is_none() && !held.contains(&key(&[&r[..]])))?,
        };
        Ok(modules + resources)
    }

    /// The bytes that [`Encoder::once_len`] counts of module `m`'s block alone.
    pub fn module_once_len(&self, m: Module) -> Result<usize> {
        if self.shared_modules {
            return Ok(0);
        }
        let mut n = Count(0);
        self.write_module(&mut n, m, self.base_level())?;
        Ok(n.0)
    }

    /// The bytes that [`Encoder::once_len`] counts of resource `r`'s block alone.
    pub fn resource_once_len(&self, r: &Resource) -> Result<usize> {
        if self.shared || r.stored_data().is_some() || self.held.borrow().contains(&key(&[&r[..]])) {
            return Ok(0);
        }
        let mut n = Count(0);
        self.resource_block(&mut n, r)?;
        Ok(n.0)
    }

    /// Sizes formulas without resource `r`, as if the archive held it already: where the
    /// formulas of the files archived together share it (a dictionary made of them all), whose
    /// cost is then no one file's. The archives are the same.
    pub fn hold_resource(&self, r: &Resource) {
        self.held.borrow_mut().insert(key(&[&r[..]]));
    }

    /// The bytes of the resource blocks in the archive of `f`.
    pub fn resources_len(&self, f: &Formula) -> Result<usize> {
        self.resources_len_of(f, |_| true)
    }

    /// [`Encoder::resources_len`] of the resources `which` takes.
    pub(crate) fn resources_len_of(&self, f: &Formula, which: impl Fn(&Resource) -> bool) -> Result<usize> {
        let (_, mut resources) = f.blocks();
        resources.retain(|r| which(r));
        let mut n = Count(0);
        resources.into_iter().try_for_each(|r| self.resource_block(&mut n, r))?;
        Ok(n.0)
    }

    /// The Zstandard context of the encoder.
    pub(crate) fn cctx(&self) -> RefMut<'_, CCtx<'static>> {
        self.cctx.borrow_mut()
    }

    /// The length of the archive of `f` without a checksum, nor the blocks whose data it takes as
    /// stored (`Encoder::module_bytes_len`), as the archive of it is made: its streams of a
    /// class and kind merged ([`Formula::merged`], as aqr-compress writes a file alone and an
    /// archive's batches group them). Apart, the many small streams of an archive's entries
    /// (a ZIP of 1,399 files) would count far more than they take.
    pub fn len(&self, f: &Formula) -> Result<usize> {
        self.len_with(f, None)
    }

    /// [`Encoder::len`], with the stream of `stand_in` (by index) its bytes rather than its data.
    /// Where that stream is merged with others, the streams are counted apart.
    pub(crate) fn len_with(&self, f: &Formula, stand_in: Option<(usize, &[&[u8]])>) -> Result<usize> {
        let (merged, groups) = f.merged();
        let mut streams: Vec<Vec<&[u8]>> = Vec::with_capacity(groups.len());
        for g in &groups {
            match stand_in {
                // (alone, as a stream that a slice goes past the end of is)
                Some((t, bytes)) if g[..] == [(t, 0, f.streams[t].data.len() as u64)] => streams.push(bytes.to_vec()),
                Some((t, _)) if g.iter().any(|p| p.0 == t) => {
                    return self.len_of(f, |n| self.write(f, stand_in, None, n));
                }
                _ => streams.push(g.iter().map(|&(s, a, b)| &f.streams[s].data[a as usize..b as usize]).collect()),
            }
        }
        let refs: Vec<&[&[u8]]> = streams.iter().map(|p| &p[..]).collect();
        self.len_of(f, |n| self.write_streams(&merged, &refs, &[], None, n))
    }

    /// The length of the archive of `f` that `write` writes, as [`Encoder::len`] counts it.
    pub(crate) fn len_of(&self, f: &Formula, write: impl FnOnce(&mut Count) -> Result<()>) -> Result<usize> {
        let mut n = Count(0);
        let sizing = self.sizing.replace(true);
        let written = write(&mut n);
        self.sizing.set(sizing);
        written?;
        let held = self.held.borrow();
        let modules = if self.shared_modules { self.modules_len(f)? } else { self.module_bytes_len(f)? };
        let resources = match self.shared {
            true => self.resources_len(f)?,
            false => self.resources_len_of(f, |r| r.stored_data().is_some() || held.contains(&key(&[&r[..]])))?,
        };
        Ok(n.0 - resources - modules)
    }

    /// The bytes of the module blocks of `f` whose bytes it takes ([`Node::ModuleBytes`]), which
    /// [`Encoder::len`] does not count, as those of resources stored ([`stored_resource`]): the
    /// archive holds them as they are either way, typically shared with the archive around them.
    pub(crate) fn module_bytes_len(&self, f: &Formula) -> Result<usize> {
        let mut modules: Vec<Module> = Vec::new();
        for n in f.shared.iter().flat_map(|s| s.inputs()).chain([&f.root]) {
            n.visit(&mut |n| {
                if let &Node::ModuleBytes(m, _) = n
                    && !modules.contains(&m)
                {
                    modules.push(m);
                }
            });
        }
        let mut n = Count(0);
        for m in modules {
            self.write_module(&mut n, m, self.base_level())?;
        }
        Ok(n.0)
    }

    /// The length of the archive of `d` as it is, without a checksum (`d` itself not copied).
    pub(crate) fn raw_len(&self, d: &[u8]) -> Result<usize> {
        let mut n = Count(0);
        n.put(&HEADER_MAGIC);
        self.content_block(&mut n, true, &[d], How::Usual, None)?;
        let blocks = n.0 - HEADER_MAGIC.len();
        footer(&mut n, blocks);
        Ok(n.0)
    }

    /// The zstd level of the module and formula blocks.
    pub(crate) fn base_level(&self) -> i32 {
        match self.level {
            Level::STORED => STORED,
            Level::Of(n) => Pick::of(n).base.0,
            Level::Zstd(l) => l,
        }
    }

    /// How stream `s` is compressed: light if of [`Kind::Compressed`]; dense where the level
    /// has a dense level ([`Pick`]), if PE code (an x86 or ARM64 filter's streams, or the rest
    /// of a PE file) or text, or of [`TOGETHER`].
    pub(crate) fn how(&self, s: &aqr_formula::Stream) -> How {
        how_of(self.level, &s.class, s.kind)
    }

    /// [`encode`] at the level of the encoder. Fails on slices out of their stream.
    pub fn encode(&self, f: &Formula, checksum: Option<u64>) -> Result<Vec<u8>> {
        // sized first (the streams compressed then are kept), so as to be written at its size
        let mut out = Vec::with_capacity(self.len_of(f, |n| self.write(f, None, None, n))? + 16);
        self.write(f, None, checksum, &mut out)?;
        Ok(out)
    }

    /// [`Encoder::encode`] of `f` whose streams' bytes are those of `pieces` (each the
    /// concatenation of its pieces; the bytes of `f`'s own streams not read), as
    /// [`aqr_formula::Formula::merged`] gives them. The blocks are compressed as they are written and
    /// not kept, but those compressed already (which are let go).
    pub fn encode_pieces(&self, f: &Formula, pieces: &[Vec<&[u8]>], checksum: Option<u64>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.encode_pieces_to(f, pieces, checksum, &mut out).expect("written to memory")?;
        Ok(out)
    }

    /// [`Encoder::encode_pieces`] written to `out` as it is made, of the bytes written. Fails
    /// where `out` does (the archive written in part, nothing more written after the failure),
    /// else gives what [`Encoder::encode_pieces`] would.
    pub fn encode_pieces_to(
        &self,
        f: &Formula,
        pieces: &[Vec<&[u8]>],
        checksum: Option<u64>,
        out: &mut dyn io::Write,
    ) -> io::Result<Result<usize>> {
        self.encode_sunk_to(f, pieces, &[], checksum, out)
    }

    /// [`Encoder::encode_pieces_to`] of `f` whose streams are those of `pieces`, then those of
    /// `sunk` (of a [`sink::Sink`]), whose blocks are written as they are.
    pub fn encode_sunk_to(
        &self,
        f: &Formula,
        pieces: &[Vec<&[u8]>],
        sunk: &[sink::Sunk],
        checksum: Option<u64>,
        out: &mut dyn io::Write,
    ) -> io::Result<Result<usize>> {
        let streams: Vec<&[&[u8]]> = pieces.iter().map(|p| &p[..]).collect();
        let mut w = Writing { out, len: 0, failed: None };
        self.keep.set(false);
        let r = self.write_streams(f, &streams, sunk, checksum, &mut w);
        self.keep.set(true);
        match w.failed {
            Some(e) => Err(e),
            None => Ok(r.map(|_| w.len)),
        }
    }

    /// [`Encoder::encode`] into `out`, with the stream of `stand_in` (by index) its bytes rather
    /// than its data.
    pub(crate) fn write(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        checksum: Option<u64>,
        out: &mut impl Out,
    ) -> Result<()> {
        let data: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let streams: Vec<&[&[u8]]> = (0..data.len())
            .map(|s| match stand_in {
                Some((t, bytes)) if t == s => bytes,
                _ => std::slice::from_ref(&data[s]),
            })
            .collect();
        self.write_streams(f, &streams, &[], checksum, out)
    }

    /// [`Encoder::write`] of `f` whose streams' bytes are `streams` (each in pieces), then the
    /// streams of a sink, `sunk`, their blocks encoded already.
    pub(crate) fn write_streams(
        &self,
        f: &Formula,
        streams: &[&[&[u8]]],
        sunk: &[sink::Sunk],
        checksum: Option<u64>,
        out: &mut impl Out,
    ) -> Result<()> {
        // of the module and formula blocks
        let level = self.base_level();
        f.check_outputs()?;
        let own = streams.len();
        let len_of = |s: usize| match s < own {
            true => Some(total(streams[s])),
            false => sunk.get(s - own).map(|s| s.len as usize),
        };
        let mut slices = Ok(());
        f.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && len_of(stream).is_none_or(|l| start.checked_add(len).is_none_or(|e| e > l as u64))
            {
                slices = Err(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
            }
        });
        slices?;

        out.put(&HEADER_MAGIC);
        let blocks = out.len();
        if self.size_hint {
            out.put(&SIZE_HINT_PLACEHOLDER);
        }
        if let (&Node::Slice { stream, start, len }, []) = (&f.root, &f.shared[..])
            && stream < own
        {
            let leaf = cut(streams[stream], start as usize, len as usize);
            if checksum.is_some_and(|c| c != checksum_of(&leaf)) {
                return Err(Error::new("output checksum not that of the leaf"));
            }
            self.content_block(out, true, &leaf, self.how(&f.streams[stream]), None)?;
        } else {
            // the non-leaf blocks sliced, of each stream in order, by their ordinals
            let mut nodes: Vec<&Node> = f.shared.iter().flat_map(|s| s.inputs()).collect();
            nodes.push(&f.root);
            let lit = literals(f.streams.len(), |s| s < own && !self.sizing.get(), nodes.iter().copied());
            let mut used = sliced(f.streams.len(), |s| len_of(s).unwrap_or(0), nodes);
            used.iter_mut().zip(&lit).filter(|(_, l)| **l).for_each(|(u, _)| u.clear());
            let (ordinals, next) = listed(&used, 0..f.streams.len());
            let lit: Vec<Option<&[&[u8]]>> = lit.iter().enumerate().map(|(s, &l)| l.then(|| streams[s])).collect();
            let (data, modules, resources) = self.formula_data(f, &ordinals, &lit, next, checksum)?;

            // (the blocks compressed at once beforehand, kept or not: where kept, as a formula is
            // compared with its data, those compressed already are skipped)
            {
                let mut blocks = Vec::new();
                for (s, used) in used.iter().enumerate().take(own) {
                    let (data, how) = (streams[s], self.how(&f.streams[s]));
                    for (r, _) in block_ranges(total(data)).zip(used).filter(|(_, u)| **u) {
                        blocks.push((cut(data, r.start, r.len()), how));
                    }
                }
                self.precompress_blocks(blocks, true)?;
            }
            for &m in &modules {
                self.write_module(out, m, level)?;
            }
            for r in resources {
                self.resource_block(out, r)?;
            }
            formula_block(out, 1, &always_compressed(&mut self.cctx(), &[&data], level, self.threads)?);
            for (s, used) in used.iter().enumerate() {
                if s >= own {
                    for (b, _) in sunk[s - own].blocks.iter().zip(used).filter(|(_, u)| **u) {
                        out.put(b);
                    }
                    continue;
                }
                let (data, how) = (streams[s], self.how(&f.streams[s]));
                for (r, _) in block_ranges(total(data)).zip(used).filter(|(_, u)| **u) {
                    self.content_block(out, false, &cut(data, r.start, r.len()), how, None)?;
                }
            }
        }

        footer(out, out.len() - blocks);
        Ok(())
    }

    /// The formula data of `f` (not compressed), its slices of the blocks listed at `ordinals`
    /// ([`listed`]: the first `next` blocks after the formula block, all of them taken), with
    /// the modules and resources it lists, in order.
    pub(crate) fn formula_data<'f>(
        &self,
        f: &'f Formula,
        ordinals: &[Vec<Option<u64>>],
        literals: &[Option<&[&[u8]]>],
        next: u64,
        checksum: Option<u64>,
    ) -> Result<(Vec<u8>, Vec<Module>, Vec<&'f Resource>)> {
        let (modules, resources) = f.blocks();
        let mut checked = Ok(());
        for n in f.shared.iter().flat_map(|s| s.inputs()).chain([&f.root]) {
            n.visit(&mut |n| match n {
                Node::ModuleBytes(m, d) if checked.is_ok() => {
                    checked = self.check_module_bytes(*m, d, self.base_level())
                }
                // (chosen before the formula is archived: [`Formula::resolve`])
                Node::Either(_) => checked = Err(Error::new("an alternative not resolved")),
                _ => {}
            });
        }
        checked?;

        let mut data = Vec::new();
        varint::put(&mut data, next);
        (0..next).for_each(|_| varint::put(&mut data, 0));
        for n in [resources.len(), modules.len()] {
            varint::put(&mut data, n as u64);
            (0..n).for_each(|i| varint::put(&mut data, i as u64));
        }
        let classes = self.classes_of(&modules);
        let mut e = Emit {
            ordinals,
            literals,
            modules: &modules,
            classes: &classes,
            resources: &resources,
            out: &mut data,
            stack: vec![],
        };
        for (i, s) in f.shared.iter().enumerate() {
            e.shared(i, s);
        }
        e.node(&f.root);
        debug_assert_eq!(e.stack, [None]);
        if let Some(c) = checksum {
            data.push(CHECKSUM);
            data.extend_from_slice(&c.to_le_bytes());
        }
        Ok((data, modules, resources))
    }

    /// The blocks that go before those a direct sink wrote ([`sink::Sink::direct`]) in the
    /// archive of `f`, all of whose streams are of the sink, stream `k` of `f` the sink's `k`
    /// (`sunk`, of [`sink::Sink::close_direct`]): its size hint block to be filled in where there is
    /// one ([`Encoder::with_size_hint`]), its module and resource blocks, then its formula block.
    /// The blocks of the sink follow it, then the footer ([`footer_bytes`]). Fails where a
    /// stream of `f` is not the sink's, or a block written is taken by no slice (the spec has
    /// every non-leaf block taken).
    pub fn direct_head(&self, f: &Formula, sunk: &[sink::Sunk], checksum: Option<u64>) -> Result<Vec<u8>> {
        f.check_outputs()?;
        if f.streams.len() != sunk.len()
            || f.streams
                .iter()
                .enumerate()
                .any(|(k, s)| s.data.sunk_in() != Some(k) || s.data.len() as u64 != sunk[k].len)
        {
            return Err(Error::new("a stream not of the sink"));
        }
        let mut nodes: Vec<&Node> = f.shared.iter().flat_map(|s| s.inputs()).collect();
        nodes.push(&f.root);
        let mut slices = Ok(());
        f.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && sunk.get(stream).is_none_or(|s| start.checked_add(len).is_none_or(|e| e > s.len))
            {
                slices = Err(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
            }
        });
        slices?;
        let used = sliced(f.streams.len(), |s| sunk[s].len as usize, nodes);
        let mut ordinals: Vec<Vec<Option<u64>>> = Vec::with_capacity(sunk.len());
        let mut next = 0;
        for (s, u) in sunk.iter().zip(&used) {
            if s.ordinals.len() != u.len() && !(u.is_empty() && s.ordinals.is_empty()) || !u.iter().all(|&u| u) {
                return Err(Error::new("a block of the sink taken by no slice"));
            }
            next += s.ordinals.len() as u64;
            ordinals.push(s.ordinals.iter().map(|&o| Some(o)).collect());
        }
        let mut all: Vec<u64> = ordinals.iter().flatten().flatten().copied().collect();
        all.sort_unstable();
        if all.iter().enumerate().any(|(i, &o)| o != i as u64) {
            return Err(Error::new("the sink's blocks not all of the formula's"));
        }
        let (data, modules, resources) = self.formula_data(f, &ordinals, &[], next, checksum)?;
        let mut out = Vec::new();
        if self.size_hint {
            out.extend_from_slice(&SIZE_HINT_PLACEHOLDER);
        }
        let level = self.base_level();
        for &m in &modules {
            self.write_module(&mut out, m, level)?;
        }
        for r in resources {
            self.resource_block(&mut out, r)?;
        }
        formula_block(&mut out, 1, &always_compressed(&mut self.cctx(), &[&data], level, self.threads)?);
        Ok(out)
    }

    /// Bytes of the module block of `m` in an archive, as [`Encoder::direct_head`] writes it.
    pub fn module_len(&self, m: Module) -> Result<usize> {
        let mut n = Count(0);
        self.write_module(&mut n, m, self.base_level())?;
        Ok(n.0)
    }
}

impl Encoder {
    /// Appends a compressed or uncompressed (leaf or non-leaf) block of `data`, whichever the
    /// spec allows, compressed as `how` tells (uncompressed where stored, and not kept
    /// compressed already), or with a dictionary where `dicts` has it compressed so (the
    /// dictionaries of a batch, and the index of their first in the segment).
    pub(crate) fn content_block(
        &self,
        out: &mut impl Out,
        leaf: bool,
        data: &[&[u8]],
        how: How,
        dicts: Dicts,
    ) -> Result<()> {
        if let Some((p, first)) = dicts
            && let Some((i, c)) = p.used.get(&key_how(data, how))
        {
            let mut index = Vec::new();
            varint::put(&mut index, (first + i) as u64);
            block_of(out, if leaf { LEAF_ZSTD_DICT } else { NON_LEAF_ZSTD_DICT }, &[&index, c]);
            return Ok(());
        }
        let mut streams = self.streams.borrow_mut();
        // (written once and not kept: compressed now, or let go where it was kept)
        if !self.keep.get() {
            let c = match streams.remove(&key_how(data, how)) {
                Some(c) => c,
                None => {
                    drop(streams);
                    let c = self.plan(key(data), data, how)?.carry_out(&mut self.cctx(), data, self.threads)?.0;
                    c.map(aqr_formula::Bytes::from)
                }
            };
            content_block(out, leaf, data, c.as_deref());
            return Ok(());
        }
        let c = match streams.entry(key_how(data, how)) {
            MapEntry::Occupied(e) => e.into_mut(),
            MapEntry::Vacant(e) => {
                let plan = self.plan(key(data), data, how)?;
                e.insert(kept(plan.carry_out(&mut self.cctx(), data, self.threads)?.0))
            }
        };
        content_block(out, leaf, data, c.as_deref());
        Ok(())
    }

    /// Appends the non-leaf blocks of a stream of `data`: [`BLOCK`] bytes each but the last
    /// (with a dictionary where `dicts` tells, [`Encoder::content_block`]).
    pub(crate) fn stream_blocks(&self, out: &mut impl Out, data: &[&[u8]], how: How, dicts: Dicts) -> Result<()> {
        for r in block_ranges(total(data)) {
            self.content_block(out, false, &cut(data, r.start, r.len()), how, dicts)?;
        }
        Ok(())
    }

    /// Appends a resource block of `r`, compressed at the level the encoder picks for it, or at
    /// the base level where it does not compress: the block is always compressed.
    pub(crate) fn resource_block(&self, out: &mut impl Out, r: &Resource) -> Result<()> {
        if let Some(s) = r.stored_data() {
            block(out, RESOURCE, s);
            return Ok(());
        }
        let mut resources = self.resources.borrow_mut();
        let c = match resources.entry(key(&[r])) {
            MapEntry::Occupied(e) => e.into_mut(),
            MapEntry::Vacant(e) => e.insert(self.always(r)?),
        };
        block(out, RESOURCE, c);
        Ok(())
    }

    /// The compressed external structure of `r`, compressed at the level the encoder picks for
    /// it, or at the base level where it does not compress: for a block always compressed.
    pub(crate) fn always(&self, r: &[u8]) -> Result<Vec<u8>> {
        let c = self.plan(key(&[r]), &[r], How::Usual)?.carry_out(&mut self.cctx(), &[r], self.threads)?.0;
        match c {
            Some(c) => Ok(c),
            None => always_compressed(&mut self.cctx(), &[r], self.base_level(), self.threads),
        }
    }

    /// About the fewest bytes stream `d` archives to at the level of the encoder, by a sample,
    /// or as soon as it is known to be at most `enough`: for [`Level::Of`], at the lowest and
    /// the highest zstd levels it picks from (its lower ones, the last higher one: some
    /// data, and data of a few hundred KB, which zstd compresses with parameters of its own,
    /// compress better at lower levels), or as it is where they do not compress it. It
    /// compresses the sample at those levels only, the faster first, where picking the level
    /// compresses it at every level: a bound, for telling whether a formula of `d` may archive
    /// smaller.
    pub fn least(&self, d: &[u8], enough: usize) -> Result<usize> {
        let levels = match self.level {
            Level::STORED => return Ok(d.len()),
            Level::Of(n) => {
                let p = Pick::of(n);
                [p.lower, &[p.top()]].concat()
            }
            Level::Zstd(l) => vec![l],
        };
        let (k, sample) = (key(&[d]), sampled(&[d]));
        let mut least = d.len();
        for level in levels {
            let c = self.trial(k, &sample, level)?;
            if let Some(c) = &c {
                least = least.min(scaled(c.len(), sample.len(), d.len()));
            }
            self.trials.borrow_mut().insert((k, level), c);
            if least <= enough {
                break;
            }
        }
        Ok(least)
    }

    /// Whether the encoder would compress stream `d` rather than store it as it is, by a sample
    /// at the probe level (zstd level 1): it stores what that saves less than 1/32 of, as
    /// [`Level::Of`] does (the sample is kept, for when it is compressed).
    pub fn compresses(&self, d: &[u8]) -> Result<bool> {
        if self.level == Level::STORED {
            return Ok(false);
        }
        let (k, sample) = (key(&[d]), sampled(&[d]));
        let c = self.trial(k, &sample, PROBE_LEVEL.get())?;
        let saves = c.as_ref().is_some_and(|c| saves(c.len(), sample.len()));
        self.trials.borrow_mut().insert((k, PROBE_LEVEL.get()), c);
        Ok(saves)
    }

    /// [`Encoder::compresses`] of each of streams `ds`, on up to the encoder's threads.
    pub fn compresses_each(&self, ds: &[&[u8]]) -> Result<Vec<bool>> {
        if self.level == Level::STORED {
            return Ok(vec![false; ds.len()]);
        }
        let trials = aqr_common::pool::par_map(ds.len(), self.threads, |i| {
            let (k, sample) = (key(&[ds[i]]), sampled(&[ds[i]]));
            THREAD_CCTX
                .with(|c| Ok((k, sample.len(), compressed(&mut c.borrow_mut(), &[&sample], PROBE_LEVEL.get(), 1)?)))
        });
        let mut kept = self.trials.borrow_mut();
        trials
            .into_iter()
            .map(|t| {
                let (k, n, c): (StreamKey, usize, Compressed) = t?;
                let saves = c.as_ref().is_some_and(|c| saves(c.len(), n));
                kept.insert((k, PROBE_LEVEL.get()), c);
                Ok(saves)
            })
            .collect()
    }

    /// Of streams `ds`, whether each compresses by 1/32 of its sample (as [`Encoder::compresses`]
    /// tells, but at [`KIN_LEVEL`]) after the samples of its kin `kin[i]` (others of
    /// `ds`, nearest first, as a raw prefix of at most [`KIN_PREFIX`] bytes): streams of a class
    /// alike, versions of a program packed say, which do not compress alone but do archived
    /// together (where the archive puts a class's streams one after another). On up to the
    /// encoder's threads; false for a stream of no kin.
    pub fn compresses_after_each(&self, ds: &[&[u8]], kin: &[Vec<usize>]) -> Result<Vec<bool>> {
        if self.level == Level::STORED {
            return Ok(vec![false; ds.len()]);
        }
        let trials = aqr_common::pool::par_map(ds.len(), self.threads, |i| -> Result<bool> {
            // (the nearest last, next to the sample: a level keeps fewer positions far back)
            let mut parts = Vec::new();
            let mut room = KIN_PREFIX.get();
            for &j in kin.get(i).map_or(&[][..], |k| &k[..]) {
                if room == 0 {
                    break;
                }
                let s = sampled(&[ds[j]]);
                let n = s.len().min(room);
                room -= n;
                parts.push(s[s.len() - n..].to_vec());
            }
            if parts.is_empty() {
                return Ok(false);
            }
            let prefix: Vec<u8> = parts.into_iter().rev().flatten().collect();
            let own = sampled(&[ds[i]]);
            Ok(after_prefix_len(&own, &prefix, KIN_LEVEL.get())?.is_some_and(|n| saves(n, own.len())))
        });
        trials.into_iter().collect()
    }

    /// Of `n` trials, each of data (one or more) after a prefix, made by `of` as they are tried
    /// (so that only those being tried are held), what each data archives in at the base level
    /// after the prefix (a raw prefix, as where they are one after another in a stream): its bytes where it
    /// does not compress by 1/32 (stored). On up to the encoder's threads. To choose between
    /// alternatives of a batch's formula as they would be archived along with the rest.
    pub fn len_after_each(
        &self,
        n: usize,
        of: impl Fn(usize) -> (Vec<Vec<u8>>, Vec<u8>) + Sync,
    ) -> Result<Vec<Vec<usize>>> {
        let (level, stored) = (BASE_LEVEL.get(), self.level == Level::STORED);
        let lens = aqr_common::pool::par_map(n, self.threads, |i| -> Result<Vec<usize>> {
            let (pieces, prefix) = of(i);
            pieces
                .iter()
                .map(|d| match stored || d.is_empty() {
                    true => Ok(d.len()),
                    false => Ok(after_prefix_len(d, &prefix, level)?.filter(|&n| saves(n, d.len())).unwrap_or(d.len())),
                })
                .collect()
        });
        lens.into_iter().collect()
    }

    /// `sample` of the stream whose key is `k` compressed at `level`, as [`Encoder::least`]
    /// compressed it if it did.
    pub(crate) fn trial(&self, k: StreamKey, sample: &[u8], level: i32) -> Result<Option<Vec<u8>>> {
        match self.trials.borrow_mut().remove(&(k, level)) {
            Some(c) => Ok(c),
            None => compressed(&mut self.cctx(), &[sample], level, self.threads),
        }
    }

    /// What to make of `data`, whose key is `k`, at the level of the encoder, compressed as
    /// `how` tells (not stored).
    pub(crate) fn plan(&self, k: StreamKey, data: &[&[u8]], how: How) -> Result<Plan> {
        plan_by(self.level, how, data, &mut |sample, level| match sample {
            Some(sample) => match self.trials.borrow_mut().remove(&(k, level)) {
                Some(c) => Ok(c),
                None => compressed(&mut self.cctx(), &[sample], level, self.threads),
            },
            None => compressed(&mut self.cctx(), data, level, self.threads),
        })
    }

    /// Compresses `streams` as [`Encoder::encode`] would, each on a thread of its own (on up to the encoder's
    /// threads, the calling one and workers of [`aqr_common::pool`]), but those of more than
    /// two of Zstandard's jobs, each on up to `LARGE_THREADS` beforehand: for the blocks of
    /// them written next, the same bytes as one after another.
    pub fn precompress(&self, streams: &[&aqr_formula::Stream]) -> Result<()> {
        let blocks =
            streams.iter().flat_map(|s| block_ranges(s.data.len()).map(move |r| (vec![&s.data[r]], self.how(s))));
        self.precompress_blocks(blocks.collect(), false)
    }

    /// [`Encoder::precompress`], but each block of more than one of Zstandard's jobs on as many
    /// workers as are free for its jobs, the largest first (as each block written alone is):
    /// for streams that are all there is to compress, nothing else made meanwhile.
    pub fn precompress_wide(&self, streams: &[&aqr_formula::Stream]) -> Result<()> {
        let blocks =
            streams.iter().flat_map(|s| block_ranges(s.data.len()).map(move |r| (vec![&s.data[r]], self.how(s))));
        self.precompress_blocks(blocks.collect(), true)
    }

    /// [`Encoder::precompress`] of `blocks` (each its bytes in pieces, and how it is compressed),
    /// those kept already, or listed before, skipped. Where `wide`, those of more than one of
    /// Zstandard's jobs are compressed first and at once, the largest first, each on as many
    /// workers as are free for its jobs (as each block written alone is): for an archive of one
    /// file, whose blocks are all there is to compress.
    pub(crate) fn precompress_blocks(&self, blocks: Vec<(Vec<&[u8]>, How)>, wide: bool) -> Result<()> {
        let todo: Vec<(Vec<&[u8]>, How)> = {
            let kept = self.streams.borrow();
            let mut seen = std::collections::HashSet::new();
            blocks
                .into_iter()
                .filter(|(d, how)| {
                    let k = key_how(d, *how);
                    !kept.contains_key(&k) && seen.insert(k)
                })
                .collect()
        };
        let level = self.level;
        let total = todo.iter().map(|t| self::total(&t.0) as u64).sum();
        let (done, progress) = (std::sync::atomic::AtomicU64::new(0), self.progress.as_deref());
        // (each kept as it is made: the store taking it then, not once all of a batch's are)
        let one =
            |(d, how): &(Vec<&[u8]>, How), threads: usize| -> Result<(StreamKey, Option<aqr_formula::Bytes>, i32)> {
                let (d, how) = (&d[..], *how);
                let made = THREAD_CCTX.with(|c| {
                    let c = &mut c.borrow_mut();
                    let plan = plan_by(level, how, d, &mut |sample, l| match sample {
                        Some(sample) => compressed(c, &[sample], l, 1),
                        None => compressed(c, d, l, 1),
                    })?;
                    let (c, l) = plan.carry_out(c, d, threads)?;
                    Ok((key_how(d, how), self::kept(c), l))
                });
                let n = self::total(d) as u64;
                if let Some(p) = progress {
                    p(done.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n, total);
                }
                made
            };
        let jobs = if wide { 1 } else { 2 };
        let (mut large, small): (Vec<_>, Vec<_>) = todo.iter().partition(|t| self::total(&t.0) > jobs * ZSTD_JOB.get());
        let mut made: Vec<_> = match wide {
            true => {
                large.sort_by_key(|t| std::cmp::Reverse(self::total(&t.0)));
                let threads = self.threads;
                aqr_common::pool::par_map(large.len(), threads, |i| one(large[i], threads))
            }
            false => {
                let each = self.threads.min(LARGE_THREADS.get());
                aqr_common::pool::par_map(large.len(), self.threads / each, |i| one(large[i], each))
            }
        };
        made.extend(aqr_common::pool::par_map(small.len(), self.threads, |i| one(small[i], 1)));
        let (mut kept, mut levels) = (self.streams.borrow_mut(), self.levels.borrow_mut());
        for m in made {
            let (k, c, l) = m?;
            kept.insert(k, c);
            levels.insert(k, l);
        }
        Ok(())
    }

    /// Whether `f` archives smaller than `d` raw. A formula of at most half of the bytes
    /// that `d` is estimated to compress to (by a sample) is taken as smaller without
    /// compressing `d`; for [`Level::Of`], `d` is compressed at most at its base level
    /// to be compared.
    pub fn smaller(&self, f: &Formula, d: &[u8]) -> Result<bool> {
        self.smaller_of(f, None, &[], d)
    }

    /// [`Encoder::smaller`], with stream `s` of `f` the bytes of `pieces` one after the other
    /// rather than its data (which is not read): a stream yet to be put together, sized as it
    /// will be archived.
    pub fn smaller_with(&self, f: &Formula, s: usize, pieces: &[&[u8]], d: &[u8]) -> Result<bool> {
        self.smaller_of(f, Some((s, pieces)), &[], d)
    }

    /// [`Encoder::smaller`] (or [`Encoder::smaller_with`] the stream of `stand_in`), the
    /// non-leaf blocks of streams `free` (by index) not counted: data the archive holds anyway,
    /// as the same data elsewhere ([`Formula::share_slices`]).
    pub fn smaller_but(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        free: &[usize],
        d: &[u8],
    ) -> Result<bool> {
        self.smaller_of(f, stand_in, free, d)
    }

    /// [`Encoder::smaller_but`] as sizes: what the formula archives in (`free` taken off), and
    /// what `d` does as it is (where the formula is at most half of an estimate of it, that
    /// estimate).
    pub fn sizes_but(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        free: &[usize],
        d: &[u8],
    ) -> Result<(usize, usize)> {
        self.sizes_of(f, stand_in, free, d)
    }

    pub(crate) fn smaller_of(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        free: &[usize],
        d: &[u8],
    ) -> Result<bool> {
        self.sizes_of(f, stand_in, free, d).map(|(n, raw)| n < raw)
    }

    pub(crate) fn sizes_of(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        free: &[usize],
        d: &[u8],
    ) -> Result<(usize, usize)> {
        // (the last of the work on `d`, told as its streams and `d` are compressed)
        let streams = f.streams.iter().enumerate().map(|(i, s)| match stand_in {
            Some((t, pieces)) if t == i => total(pieces),
            _ => s.data.len(),
        });
        let _sweep = Swept::over(aqr_common::progress::Sweep::new(d, (streams.sum::<usize>() + d.len()) as u64));
        let mut n = self.len_with(f, stand_in)?;
        if !matches!((&f.root, &f.shared[..]), (Node::Slice { .. }, [])) {
            for &s in free {
                let mut c = Count(0);
                self.stream_blocks(&mut c, &[&f.streams[s].data], self.how(&f.streams[s]), None)?;
                n = n.saturating_sub(c.0);
            }
        }
        // (`d` was compressed, so a formula of it of at most half of its estimate was not one)
        if let Some(&raw) = self.raw_lens.borrow().get(&key(&[d])) {
            return Ok((n, raw));
        }
        if !self.streams.borrow().contains_key(&key(&[d])) {
            let c = match self.plan(key(&[d]), &[d], How::Usual)? {
                Plan::Done(c, _) => c,
                Plan::Level(level, estimate) => {
                    if n <= estimate / 2 {
                        return Ok((n, estimate));
                    }
                    // Of compares at the base level, as a formula mostly is smaller, and what
                    // was compared is then not archived: a formula is taken over data that only
                    // a higher level would have compressed to less (by up to what they differ)
                    // (counted, not held, where it is not kept)
                    let counted = match matches!(self.level, Level::Of(_)) && level > self.base_level() {
                        true => Some(self.base_level()),
                        false => (d.len() > RAW_KEPT_MAX.get()).then_some(level),
                    };
                    if let Some(counted) = counted {
                        let c = compressed_len(&mut self.cctx(), &[d], counted, self.threads)?;
                        let raw = archived_raw_len(d, c);
                        self.raw_lens.borrow_mut().insert(key(&[d]), raw);
                        return Ok((n, raw));
                    }
                    compressed_to(&mut self.cctx(), &[d], level, self.threads, estimate)?
                }
                // (counted, not held, where it is not kept, as above)
                Plan::Probe if d.len() > RAW_KEPT_MAX.get() => {
                    let c = compressed_len(&mut self.cctx(), &[d], PROBE_LEVEL.get(), self.threads)?;
                    let raw = archived_raw_len(d, c.filter(|&c| saves(c, d.len())));
                    self.raw_lens.borrow_mut().insert(key(&[d]), raw);
                    return Ok((n, raw));
                }
                Plan::Probe => Plan::Probe.carry_out(&mut self.cctx(), &[d], self.threads)?.0,
            };
            self.streams.borrow_mut().insert(key(&[d]), kept(c));
        }
        Ok((n, self.raw_len(d)?))
    }
}

thread_local! {
    /// The work that the data compressed on this thread is told of as ([`Swept`]).
    pub(crate) static SWEEP: RefCell<Option<aqr_common::progress::Sweep>> = const { RefCell::new(None) };
}
