//! The entry points ([`recompress`], [`compress`], [`compress_formula`]) and where an archive
//! is written ([`Sink`]): the formula finished, its streams sealed and written, directly where
//! they may be.

use super::*;

/// `d` by its length and hash, for [`Cx::coded_held`].
pub(crate) fn stream_key(d: &[u8]) -> (usize, u64) {
    (d.len(), xxhash_rust::xxh3::xxh3_64(d))
}

/// `d` as a formula, with the recompressor for its format if any takes it.
pub fn recompress(d: &[u8], opt: &Options) -> Formula {
    recompress_at(d, opt, 0, Kind::Unknown, &Cx::new(d, opt)).into_inner()
}

/// Declares the memory limits of `f`'s applications as the compressor does without having run
/// them (from their modules' plans): for tests of formulas made apart.
#[doc(hidden)]
pub fn declare_memory(f: &mut Formula) {
    choose::kept_in(f, |f| memory::declare(f, &Native { threads: 1 }, &memory::Limits::default()));
}

/// `d` as an archive, with the checksum of `d`.
pub fn compress(d: &[u8], opt: &Options) -> Vec<u8> {
    let mut c = Vec::new();
    compress_with(d, Kind::Unknown, opt, false, None, &mut c).expect("written to memory");
    c
}

/// [`compress`], with the formula the archive holds: that of `d` raw where it was not verified.
pub fn compress_formula(d: &[u8], opt: &Options) -> (Vec<u8>, Formula) {
    compress_formula_as(d, Kind::Unknown, opt)
}

/// [`compress_formula`] of a file of `kind`, as its name tells ([`kind_by_name`]), as a file in
/// an archive of many is taken.
pub fn compress_formula_as(d: &[u8], kind: Kind, opt: &Options) -> (Vec<u8>, Formula) {
    let mut c = Vec::new();
    let (_, f) = compress_with(d, kind, opt, true, None, &mut c).expect("written to memory");
    (c, f.expect("kept"))
}

/// [`compress_formula_as`] written to `out` as it is made, of the bytes written; telling `tell`
/// how far it is as [`compress_tree_to`] does, of `d` as content 0 in a batch of its own (the
/// compression told only once it is encoded, and once it is verified). Fails where `out` does.
pub fn compress_formula_to(
    d: &[u8],
    kind: Kind,
    opt: &Options,
    out: &mut dyn Sink,
    tell: &(dyn Fn(Event) + Sync),
) -> io::Result<(usize, Formula)> {
    let (tracker, notes) = progress::Tracker::new(vec![d.len() as u64]);
    let (n, f) = std::thread::scope(|s| {
        s.spawn(|| tracker.run(tell));
        // (the notes gone with it, the tracker done)
        compress_with(d, kind, opt, true, Some(notes), out)
    })?;
    Ok((n, f.expect("kept")))
}

/// Where [`compress_formula_to`] writes an archive: read back to be verified, and written again
/// from the start where it does not give the file back.
pub trait Sink: Write {
    /// The bytes written so far.
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>>;

    /// Drops the bytes written, to be written again from the start.
    fn restart(&mut self) -> io::Result<()>;

    /// Whether the bytes [`Sink::written`] gives are a mapping, read only, of a file: whose
    /// pages may be let go of as they are read ([`aqr_archive::Archive::let_go_read`]).
    fn mapped(&self) -> bool {
        false
    }

    /// Where nothing is written yet and bytes can be written at offsets from its start (a file):
    /// how, for blocks to be written as they are made and the formula block before them after
    /// ([`direct`]). The sink is then written to only through it, until [`Sink::placed`].
    fn place(&mut self) -> Option<Arc<Placing>> {
        None
    }

    /// Has the sink go on after the `len` bytes written through [`Sink::place`].
    fn placed(&mut self, len: u64) -> io::Result<()> {
        let _ = len;
        Ok(())
    }

    /// Where the archive written so far is an encrypted one: what reads it back, decrypting its
    /// payload as it is read ([`aqr_archive::open_decrypting`]) rather than all of it ([`Sink::written`]).
    fn sealed_back(&mut self) -> io::Result<Option<SealedBack>> {
        Ok(None)
    }
}

/// An encrypted archive written ([`Sink::sealed_back`]): its file mapped, and what unseals it.
pub struct SealedBack {
    map: memmap2::Mmap,
    opener: aqr_archive::envelope::Opener,
}

/// An archive written, read back to be verified: its bytes (a file mapped, read only, where
/// `mapped`: [`Sink::mapped`]), or an encrypted archive's payload decrypted as it is read.
#[derive(Clone, Copy)]
pub(crate) enum Back<'a> {
    Plain(&'a [u8], bool),
    Sealed(&'a aqr_archive::envelope::Decrypting<'a>),
}

impl<'a> Back<'a> {
    /// The archive opened, its pages (or chunks decrypted) let go of as its blocks are read.
    pub(crate) fn open<'n>(
        self,
        runner: &'n dyn aqr_archive::Runner,
    ) -> aqr_archive::Decoded<aqr_archive::Archive<'a, 'n>> {
        match self {
            Back::Plain(c, mapped) => {
                let mut a = aqr_archive::open(c, runner)?;
                if mapped {
                    // SAFETY: the archive's file mapped read only ([`Sink::mapped`])
                    unsafe { a.let_go_read() };
                }
                Ok(a)
            }
            Back::Sealed(d) => aqr_archive::open_decrypting(d, runner),
        }
    }
}

/// An archive written at offsets ([`Sink::place`]): a file, or the payload of an encrypted
/// archive, its chunks sealed as they are whole.
pub enum Placing {
    File(std::fs::File),
    Sealed(Arc<aqr_archive::envelope::Placed>),
}

impl Placing {
    /// Moves bytes `from..end` written back by `by` bytes (the payload of an encrypted archive
    /// sealed again under a new nonce, [`aqr_archive::envelope::Placed::moved_back`]).
    pub(crate) fn moved_back(&self, from: u64, end: u64, by: u64) -> io::Result<()> {
        let f = match self {
            Placing::File(f) => f,
            Placing::Sealed(p) => return p.moved_back(from, end, by),
        };
        let mut buf = vec![0u8; 8 << 20];
        let mut at = end;
        while at > from {
            let n = (at - from).min(buf.len() as u64);
            at -= n;
            aqr_common::fsio::read_exact_at(f, &mut buf[..n as usize], at)?;
            aqr_common::fsio::write_all_at(f, &buf[..n as usize], at + by)?;
        }
        Ok(())
    }
}

impl aqr_archive::sink::Place for Placing {
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()> {
        match self {
            Placing::File(f) => aqr_common::fsio::write_all_at(f, d, at),
            Placing::Sealed(p) => p.write_at(at, d),
        }
    }
}

/// The file an encrypted archive's chunks are written to at offsets ([`Placing::Sealed`]).
struct SealedFile(std::fs::File);

impl aqr_archive::envelope::At for SealedFile {
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()> {
        aqr_common::fsio::write_all_at(&self.0, d, at)
    }

    fn read_at(&self, at: u64, d: &mut [u8]) -> io::Result<()> {
        aqr_common::fsio::read_exact_at(&self.0, d, at)
    }
}

impl Sink for Vec<u8> {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        Ok(Box::new(&self[..]))
    }

    fn restart(&mut self) -> io::Result<()> {
        self.clear();
        Ok(())
    }
}

/// A file written from its start, opened to be read as well (it is mapped to be read back).
impl Sink for std::fs::File {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        // SAFETY: the file is the archive being written, changed by nothing else while it is
        // read back (the map borrowing the file)
        Ok(Box::new(unsafe { memmap2::Mmap::map(&*self)? }))
    }

    fn restart(&mut self) -> io::Result<()> {
        self.set_len(0)?;
        self.seek(io::SeekFrom::Start(0)).map(|_| ())
    }

    fn mapped(&self) -> bool {
        true
    }

    fn place(&mut self) -> Option<Arc<Placing>> {
        let empty = self.stream_position().ok()? == 0 && self.metadata().ok()?.len() == 0;
        empty.then(|| self.try_clone().ok().map(|f| Arc::new(Placing::File(f)))).flatten()
    }

    fn placed(&mut self, len: u64) -> io::Result<()> {
        // (the position shared with the clone written at offsets, on Windows)
        self.seek(io::SeekFrom::Start(len)).map(|_| ())
    }
}

/// An archive given mirror blocks as it is written to a sink.
impl<S: Sink + ?Sized> Sink for aqr_archive::Mirroring<&mut S> {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        self.get_mut().written()
    }

    fn restart(&mut self) -> io::Result<()> {
        aqr_archive::Mirroring::restart(self);
        self.get_mut().restart()
    }

    fn mapped(&self) -> bool {
        self.get_ref().mapped()
    }

    fn sealed_back(&mut self) -> io::Result<Option<SealedBack>> {
        self.get_mut().sealed_back()
    }
}

/// An encrypted archive being written to a file, read back decrypted (into memory not backed
/// by a file, so that nothing decrypted is written out), the pages of the file let go of as
/// they are decrypted.
impl<F: Write + io::Seek + std::borrow::Borrow<std::fs::File>> Sink for aqr_archive::envelope::Sealer<F> {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        self.finish()?;
        // SAFETY: as of a file above
        let c = unsafe { memmap2::Mmap::map(std::borrow::Borrow::<std::fs::File>::borrow(self.get_ref()))? };
        let u = self.unseal(&c).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut d = memmap2::MmapMut::map_anon(u.len().max(1))?;
        // SAFETY: of the file mapped read only above
        let read = |c: &[u8]| unsafe { aqr_common::pages::let_go(c.as_ptr(), c.len()) };
        u.decrypt_into_then(&mut d[..u.len()], aqr_common::pool::size(), &read)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(Box::new(d.make_read_only()?))
    }

    fn restart(&mut self) -> io::Result<()> {
        aqr_archive::envelope::Sealer::restart(self, |f| std::borrow::Borrow::<std::fs::File>::borrow(f).set_len(0))
    }

    /// The payload written at offsets, each chunk sealed once it is whole (the first, and those
    /// a room is left in, only once that is filled).
    fn place(&mut self) -> Option<Arc<Placing>> {
        let f = std::borrow::Borrow::<std::fs::File>::borrow(self.get_ref()).try_clone().ok()?;
        let p = aqr_archive::envelope::Sealer::place(self, Arc::new(SealedFile(f)))?;
        Some(Arc::new(Placing::Sealed(p)))
    }

    fn placed(&mut self, len: u64) -> io::Result<()> {
        aqr_archive::envelope::Sealer::placed(self, len)
    }

    fn sealed_back(&mut self) -> io::Result<Option<SealedBack>> {
        self.finish()?;
        // SAFETY: as of a file above
        let map = unsafe { memmap2::Mmap::map(std::borrow::Borrow::<std::fs::File>::borrow(self.get_ref()))? };
        Ok(Some(SealedBack { map, opener: self.opener() }))
    }
}

/// [`compress`] of a file of `kind`, with the formula of the archive where it is to be `kept`:
/// else it is dropped before the archive is verified. Where there are `notes`, it is told of as
/// a batch of content 0.
pub(crate) fn compress_with(
    d: &[u8],
    kind: Kind,
    opt: &Options,
    kept: bool,
    notes: Option<progress::Notes>,
    out: &mut dyn Sink,
) -> io::Result<(usize, Option<Formula>)> {
    use progress::Note;
    let opt = &verifying(opt);
    let note = |n: Note| {
        if let Some(notes) = &notes {
            notes.send(n)
        }
    };
    note(Note::Batch { batch: 0, bytes: d.len() as u64, verify: opt.verify, contents: vec![0] });
    compressing(opt);
    let _spill = spilling(opt);
    note(Note::Started(0));
    let watch = notes.as_ref().map(|notes| progress::Files::of(notes, [(0, d)]));
    let mut cx = Cx::new(d, opt);
    cx.pre = opt.verify.then(|| Arc::new(verify::Prevalidator::spawn(opt.threads, &[d])));
    // (the contents of an image or the like sealed into a sink as they are made; of a large file,
    // the sink's blocks written to `out` as they are, after room for the formula block)
    let direct = direct_room(d, kind, opt, &cx.enc).and_then(|room| Some((out.place()?, room)));
    let sink = match &direct {
        Some((place, room)) => {
            let at = (aqr_archive::HEADER_MAGIC.len() as u64) + room;
            aqr_archive::sink::Sink::direct(&cx.enc, archive::SLICE_MIN.get(), place.clone(), at, opt.threads.max(2))
        }
        None => aqr_archive::sink::Sink::new(&cx.enc, archive::SLICE_MIN.get()),
    };
    let order = Arc::new(seal::Order::new(sink));
    *cx.sealing.get_mut() = Some(seal::Sealing { order: order.clone(), slot: seal::ROOT });
    let _item = aqr_common::trace::item(Some(0));
    let analyzing = aqr_common::trace::span("analyze");
    let mut f = recompress_at(d, opt, 0, kind, &cx);
    crate::hints::settle(&mut f);
    // (what the file's parts hold of each other, as a batch's files: the same body in two ZIP
    // archives in a tar archive, say)
    f.share_slices(archive::SLICE_MIN.get());
    // (the root is one output: its parts flattened too)
    f.root.flatten();
    drop(watch);
    drop(analyzing);
    note(Note::Ended(0));
    let memo = cx.pre.take().map(verify::Prevalidator::finish_shared).unwrap_or_default();
    // (the root's slot taken, or never used)
    drop(cx.sealing.take());
    let sink = Arc::into_inner(order).expect("the order held only here").into_sink();
    let r = finish(f, d, opt, &cx, memo, Some((sink, direct)), kept, notes.as_ref(), out)?;
    note(Note::Written(0));
    Ok(r)
}

/// Where an archive is written directly ([`direct_room`]): through what, and the room before the
/// blocks.
type Direct = (Arc<Placing>, u64);

aqr_common::zopt! {
    /// Bytes of a file of tensors (aqr-tensors') from which on it is archived directly ([`direct_room`]).
    static DIRECT: Opt<usize> =
        Opt::size("direct-min", 1 << 30, "bytes of a file of tensors from which on it is archived directly");
}

/// [`DIRECT`], or what the tests set.
fn direct_min() -> usize {
    #[cfg(test)]
    if let Some(n) = tests::DIRECT_MIN.get() {
        return n;
    }
    DIRECT.get()
}

/// Where file `d` of `kind` (of [`direct_min`] bytes or more) is archived alone by the formula
/// of a recompressor telling how large its formula comes to at most (aqr-tensors'), each part of
/// it sealed as it is made: the room for its module and formula blocks to leave before the
/// blocks of the sink, which are then written to the archive as they are made rather than held
/// (`docs/streaming-encoder-design.md`, "출력에 바로 쓰기"); what the formula leaves of the room is
/// a spacer block after it. Not where the archive is mirrored (the mirror blocks' places are
/// not planned with the room).
fn direct_room(d: &[u8], kind: Kind, opt: &Options, enc: &aqr_archive::Encoder) -> Option<u64> {
    let tensors = [Format::Gguf, Format::Pth, Format::Safetensors];
    let taken = detect(d)
        .filter(|f| tensors.contains(f))
        .is_some_and(|f| (kind == Kind::Unknown || kind == Kind::Format(f.name())) && format_on(f, opt));
    if d.len() < direct_min() || opt.mirrors || !opt.recompresses() || !taken {
        return None;
    }
    let bound = aqr_tensors::formula::formula_bound(d)?;
    // (the data compressed at most: Zstandard's raw blocks, its frame and the block's header)
    let formula = bound + bound / 128 + 64;
    let modules = enc.module_len(aqr_tensors::formula::MODULE).ok()?;
    // (and the size hint block to be filled in, of an encrypted archive's payload)
    let hint = if opt.size_hint { aqr_archive::SIZE_HINT_PLACEHOLDER.len() } else { 0 };
    let room = hint + formula + modules + 2;
    #[cfg(test)]
    if let Some(n) = tests::DIRECT_ROOM.get() {
        return Some(n);
    }
    Some((room + room % 2) as u64)
}

/// The sealing that the parts of a formula take as they are made, where its sink writes its
/// blocks as they are ([`direct_room`]): the formula of the file itself, made as `sealing` is
/// given (taken by [`recompress_sealed`]).
pub(crate) fn sealing_parts(sealing: &Option<seal::Sealing>) -> Option<Arc<seal::Order>> {
    sealing.as_ref().filter(|s| s.slot == seal::ROOT && s.order.sink().is_direct()).map(|s| s.order.clone())
}

aqr_common::zopt! {
    /// Bytes of aqr-lzma's match finders' chains at once while compressing: two of a 64 MiB
    /// dictionary, the parts of a stream or the folders of a 7z archive side by side (decoding
    /// keeps to one, within its budget of memory).
    static LZMA_CHAINS: Opt<usize> =
        Opt::size("lzma:chains", 2 << 30, "bytes of the match finders' chains of aqr-lzma at once while compressing");
}

/// Lets aqr-lzma's match finders take [`LZMA_CHAINS`], as compressing starts, and has them keep
/// the plans of its applications to the application bound of `opt` (by the reach they look back,
/// which a stream's record tells).
pub(crate) fn compressing(opt: &Options) {
    aqr_lzma::mf::allow_chains(LZMA_CHAINS.get());
    let bound = |m| memory::bound_of(opt.application_bound, m);
    aqr_lzma::plan::set_bound(bound(aqr_lzma::formula::MODULE).min(bound(aqr_lzma::formula::LZMA2)));
}

/// The spill of [`Options::spill`] as the store of large buffers, while the guard is kept (and
/// the heap where it cannot be made).
pub(crate) fn spilling(opt: &Options) -> Option<aqr_formula::store::Guard> {
    let spill = spill::Spill::create(opt.spill.as_ref()?).ok()?;
    Some(aqr_formula::store::set(Some(Arc::new(spill))))
}

/// The archive of formula `f` of `d`, written to `out`, of the bytes written; with
/// [`Options::verify`], read back, decoded and compared with `d` (the outputs of applications
/// in `memo` taken from there), and where it is not `d` (a module's bug), the archive of `d` raw
/// written instead. The formula is returned where `kept` (of no bytes of the streams sunk);
/// what is no longer needed (the formula, the streams compressed) is dropped before the
/// decoding. The streams of `sink`, where `f`'s sunk streams are, are written after its own;
/// where it writes its blocks to `out` as they are made (through the place given, after the
/// room given: [`direct_room`]), `f` is sealed into it too, and its module and formula blocks put
/// into the room ([`written_direct`]). The `notes` are told of it encoded and verified. Fails
/// where `out` does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish(
    mut f: choose::Settled,
    d: &[u8],
    opt: &Options,
    cx: &Cx,
    memo: verify::Memo,
    sink: Option<(seal::Sink, Option<Direct>)>,
    kept: bool,
    notes: Option<&progress::Notes>,
    out: &mut dyn Sink,
) -> io::Result<(usize, Option<Formula>)> {
    let (sink, direct) = sink.map_or((None, None), |(s, d)| (Some(s), d));
    use progress::Note;
    let mut mirroring = None;
    let out: &mut dyn Sink = match opt.mirrors {
        true => mirroring.insert(aqr_archive::Mirroring::new(out, opt.size_hint)),
        false => out,
    };
    lzma::apply(&mut f, opt.threads);
    together::apply(&mut f);
    memory::declare(&mut f, &Native { threads: opt.threads }, &memo.1);
    memory::note_kept(&f, opt.application_bound, d.len() as u64);
    // the streams of a class together, as they compress smaller: as pieces of the streams, not
    // copied, their blocks compressed as they are written; then the sink's
    // (of a file mapped, let go of as it is read: of all of it, again, once for the checksum and
    // the content hash that verifying compares with)
    let read = through(d, opt.threads);
    let sum = Some(read.sum);
    let encoding = aqr_common::trace::span("encode");
    let n = match (sink, direct) {
        (Some(sink), Some((place, room))) => written_direct(&mut f, sink, &place, room, sum, &cx.enc, out)?,
        (sink, _) => {
            let sunk = sink.map(seal::Sink::close).transpose().map_err(io::Error::other)?.unwrap_or_default();
            let (mut merged, groups) = f.merged();
            merged.streams.extend(sunk_streams(&sunk));
            let pieces: Vec<Vec<&[u8]>> = groups
                .iter()
                .map(|g| g.iter().map(|&(i, a, b)| &f.streams[i].data[a as usize..b as usize]).collect())
                .collect();
            written(out, |w| cx.enc.encode_sunk_to(&merged, &pieces, &sunk, sum, w))?
        }
    };
    // (the input read through: a file mapped, its pages read again as verifying comes to them)
    aqr_formula::store::let_go(d);
    drop(encoding);
    notes.inspect(|n| n.send(Note::Encoded(0, 1, 1)));
    cx.enc.clear();
    let f = kept.then(|| f.into_inner());
    // (read back from its file, its blocks let go of as they are decoded: not all of the archive
    // resident by the end; not where it is decrypted into memory of its own)
    let mapped = out.mapped();
    let memo = std::cell::RefCell::new(memo);
    let decodes = |c: Back| {
        let _s = aqr_common::trace::span("verify");
        let files = |_: usize, range: Range<usize>| d.get(range).map(<[u8]>::to_vec);
        let m = verify::Cached::new(Native { threads: opt.threads }, std::mem::take(&mut *memo.borrow_mut()), &files);
        // (compared as it is written out, not held whole: a file of parts streams, see
        // `aqr_archive::Archive::write_file`)
        let runner = aqr_decompress::natives(&m);
        // (the bytes compared let go of as it goes, [`LET_GO`] at a time, where `d` is a file mapped)
        let (mut at, mut gone) = (0, 0);
        let let_go = LET_GO.get();
        let decodes = c.open(&runner).is_ok_and(|a| {
            let written = a.write_file(opt.threads, &mut |p: &[u8]| {
                let same = d.get(at..at + p.len()) == Some(p);
                at += p.len();
                if same && at - gone >= let_go {
                    aqr_formula::store::let_go(&d[gone..at]);
                    gone = at;
                }
                if same { Ok(()) } else { Err(aqr_archive::Failure::Invalid("not the file".into())) }
            });
            written.is_ok_and(|n| n == d.len() as u64)
        });
        aqr_formula::store::let_go(d);
        notes.inspect(|n| n.send(Note::Verified(0, d.len() as u64)));
        decodes
    };
    // by the hashes recorded, without running a module; else decoded
    let verified = |c: Back| match hashed(c, d, Some(&read), &memo, opt) {
        Some(same) => same,
        None => decodes(c),
    };
    // (an encrypted archive's payload decrypted as its blocks are read, its file's pages let go
    // of as they are decrypted: not all of it in memory, nor its file resident, at once)
    let verified_out = |out: &mut dyn Sink| -> io::Result<bool> {
        let Some(b) = out.sealed_back()? else { return Ok(verified(Back::Plain(&out.written()?, mapped))) };
        let Ok(u) = b.opener.unseal(&b.map) else { return Ok(false) };
        let mut p = u.decrypting();
        // SAFETY: of the archive's file mapped read only
        unsafe { p.let_go_read() };
        Ok(verified(Back::Sealed(&p)))
    };
    let r = match n {
        Ok(n) if !opt.verify || verified_out(out)? => (n, f),
        // a leaf of other bytes than `d` is not encoded with its checksum
        _ => {
            out.restart()?;
            // (of the bytes of `d`, not a copy of them, but for the formula kept)
            let mut raw = Formula::raw("raw", Vec::new());
            raw.root = Node::slice(0, 0, d.len() as u64);
            let n = written(out, |w| cx.enc.encode_pieces_to(&raw, &[vec![d]], sum, w))?;
            (n.expect("a stream of the whole"), kept.then(|| Formula::raw("raw", d.to_vec())))
        }
    };
    Ok((r.0 + mirroring.map_or(0, |m| m.added() as usize), r.1))
}

/// The streams of formula of a sink's streams `sunk`, after those of the formula's own: as
/// [`Formula::merged`] has them.
fn sunk_streams(sunk: &[aqr_archive::sink::Sunk]) -> impl Iterator<Item = Stream> + '_ {
    sunk.iter().enumerate().map(|(k, s)| Stream {
        class: s.class.clone(),
        kind: s.kind,
        content: false,
        apart: false,
        data: Bytes::sunk(k, s.len as usize),
    })
}

/// The archive of formula `f` whose blocks `sink` writes to `out` as they are made (through
/// `place`, from the room of `room` bytes after the header on, [`direct_room`]), of the bytes
/// written: `f` sealed into it, the room then its module and formula blocks and a spacer block
/// of the rest of it, and the footer after the blocks. Where they take more than the room (which
/// they are not to), the blocks are moved after them. A formula error (a block of the sink no
/// slice takes, as where a recompression gave up having sealed some of it) is given as such,
/// for the archive of the file as it is to be written instead.
fn written_direct(
    f: &mut choose::Settled,
    sink: seal::Sink,
    place: &Placing,
    room: u64,
    sum: Option<u64>,
    enc: &aqr_archive::Encoder,
    out: &mut dyn Sink,
) -> io::Result<aqr_formula::Result<usize>> {
    use aqr_archive::sink::Place;
    let start = aqr_archive::HEADER_MAGIC.len() as u64;
    #[cfg(test)]
    tests::DIRECTED.with(|n| n.set(n.get() + 1));
    let sealed = sink.seal(f);
    let (sunk, end) = sink.close_direct().map_err(io::Error::other)?;
    let head = sealed.and_then(|()| {
        let (mut merged, groups) = f.merged();
        if !groups.is_empty() {
            return Err(aqr_formula::Error::new("a stream of the formula not sealed"));
        }
        merged.streams.extend(sunk_streams(&sunk));
        enc.direct_head(&merged, &sunk, sum)
    });
    let head = match head {
        Ok(h) => h,
        Err(e) => return Ok(Err(e)),
    };
    // (a spacer of 2 bytes at least after the formula block: the room only shrinks)
    let need = head.len() as u64 + 2;
    let (blocks_at, end) = match need <= room {
        true => (start + room, end),
        false => {
            let by = need - room;
            place.moved_back(start + room, end, by)?;
            (start + need, end + by)
        }
    };
    let spacer = blocks_at - start - head.len() as u64;
    place.write_at(0, &aqr_archive::HEADER_MAGIC)?;
    place.write_at(start, &head)?;
    // (the spacer whole, its space zeroed too: an encrypted archive's chunks are sealed only
    // once all of their bytes are written)
    let mut kill = aqr_archive::kill(spacer as usize);
    kill.resize(spacer as usize, 0);
    place.write_at(start + head.len() as u64, &kill)?;
    let footer = aqr_archive::footer_bytes((end - start) as usize);
    place.write_at(end, &footer)?;
    let n = end + footer.len() as u64;
    out.placed(n)?;
    Ok(Ok(n as usize))
}

/// What verifying compares a file with, as it was read through before the archive was written
/// ([`through`]): its checksum ([`aqr_archive::checksum`]) and its content hash.
pub(crate) struct Through {
    pub(crate) sum: u64,
    pub(crate) ch: aqr_archive::chash::Ch,
}

/// [`Through`] of `d`, read once, its pages let go of as they are read ([`LET_GO`] at a time,
/// where it is a file mapped): the content hash of each [`through_len`] bytes on up to `threads`
/// threads (as many pages read in at once), on a worker of its own where one is free, while the
/// checksum is taken of the stretch before.
pub(crate) fn through(d: &[u8], threads: usize) -> Through {
    use aqr_archive::chash::Ch;
    let hashes = |part: &[u8]| -> Vec<Ch> {
        let pieces: Vec<&[u8]> = part.chunks(LET_GO.get()).collect();
        aqr_common::pool::par_map(pieces.len(), threads.max(1), |k| Ch::of(pieces[k]))
    };
    let mut sum = xxhash_rust::xxh64::Xxh64::new(0);
    let mut ch = Ch::EMPTY;
    let parts: Vec<&[u8]> = d.chunks(through_len()).collect();
    let mut ahead = parts.first().map(|p| hashes(p));
    aqr_common::pool::scope(1, |s| {
        for (k, part) in parts.iter().enumerate() {
            let next = parts.get(k + 1).copied();
            let next = match s.workers() {
                0 => {
                    sum.update(part);
                    next.map(hashes)
                }
                _ => {
                    let h = next.map(|p| s.spawn(move || hashes(p)));
                    sum.update(part);
                    h.map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                }
            };
            ch = ahead.take().expect("hashed ahead").into_iter().fold(ch, Ch::cat);
            aqr_formula::store::let_go(part);
            ahead = next;
        }
    });
    Through { sum: sum.digest(), ch }
}

/// Bytes of a file read through at once ([`through`]).
pub(crate) fn through_len() -> usize {
    16 * LET_GO.get()
}

/// Whether archive `c` of file `d` gives it back by its content hashes, worked out without
/// running a module by what `memo` recorded (`aqr_archive::Archive::hashed`): its outputs' hashes those
/// of the parts of `d` they are, which their checksums are of. Where the archive has an output
/// of all of `d`, it is compared with `read` of it, if given, and `d` is not read again. None
/// where any application is not recorded.
pub(crate) fn hashed(
    c: Back,
    d: &[u8],
    read: Option<&Through>,
    memo: &std::cell::RefCell<verify::Memo>,
    opt: &Options,
) -> Option<bool> {
    use aqr_archive::chash::Ch;
    let _s = aqr_common::trace::span("verify");
    let native = Native { threads: opt.threads };
    let runner = aqr_decompress::natives(&native);
    let Ok(a) = c.open(&runner) else { return Some(false) };
    let roots = match a.hashed(&*memo.borrow(), &aqr_decompress::identify, opt.threads) {
        Ok(Some(roots)) => roots,
        Ok(None) => return None,
        Err(_) => return Some(false),
    };
    if let (Some(read), [(h, sums)]) = (read, &roots[..])
        && h.len() == d.len() as u64
    {
        return Some(*h == read.ch && sums.iter().all(|&s| s == read.sum));
    }
    // (the hash of `d` made of those of its pieces, each read once for both, [`LET_GO`] at a
    // time, its pages then let go of: not all of a file mapped resident by the end)
    let (mut at, mut whole, mut of_d) = (0usize, Ch::EMPTY, Ch::EMPTY);
    let let_go = LET_GO.get();
    for (h, sums) in roots {
        let Some(part) = d.get(at..at.saturating_add(h.len() as usize)) else { return Some(false) };
        let mut sum = xxhash_rust::xxh64::Xxh64::new(0);
        for piece in part.chunks(let_go) {
            sum.update(piece);
            of_d = of_d.cat(Ch::of(piece));
            aqr_formula::store::let_go(piece);
        }
        // (the checksums of the archive's, [`aqr_archive::checksum`])
        if sums.iter().any(|&s| s != sum.digest()) {
            return Some(false);
        }
        (at, whole) = (at + part.len(), whole.cat(h));
    }
    let same = at == d.len() && whole == of_d;
    aqr_formula::store::let_go(d);
    Some(same)
}

aqr_common::zopt! {
    /// Bytes of the input compared in verifying ([`finish`]) between the pages let go of: few calls,
    /// and few pages more resident.
    static LET_GO: Opt<usize> =
        Opt::size("let-go", 4 << 20, "bytes of the input compared in verifying between letting its pages go");
}

/// What `write` writes to `out` (through a buffer), flushed.
fn written<T>(out: &mut dyn Sink, write: impl FnOnce(&mut dyn Write) -> io::Result<T>) -> io::Result<T> {
    let mut w = io::BufWriter::with_capacity(1 << 20, out);
    let t = write(&mut w)?;
    w.flush()?;
    Ok(t)
}
