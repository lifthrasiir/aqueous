//! Streams compressed by Zstandard: the level planned by a sample, the frames made and stored
//! as the spec has them (external frames, checksums).

use super::*;

/// The data compressed on this thread told of as a sweep, until dropped ([`external`]); the one
/// before, if any, then as it was.
pub(crate) struct Swept(Option<aqr_common::progress::Sweep>);

impl Swept {
    pub(crate) fn over(s: aqr_common::progress::Sweep) -> Swept {
        Swept(SWEEP.with(|t| t.borrow_mut().replace(s)))
    }
}

impl Drop for Swept {
    fn drop(&mut self) {
        SWEEP.with(|t| *t.borrow_mut() = self.0.take());
    }
}

/// The output that Zstandard is given at a time where it is told of how far it is ([`Swept`]),
/// so that it hands back now and then, for data of more than [`SWEPT_MIN`] bytes: up to 512
/// KiB, Zstandard compresses on the calling thread, which makes other output with less room
/// than it may take.
const SWEPT_WINDOW: usize = 256 << 10;
const SWEPT_MIN: usize = 1 << 20;

thread_local! {
    /// The Zstandard context of a thread compressing streams for [`Encoder::precompress`].
    pub(crate) static THREAD_CCTX: RefCell<CCtx<'static>> = RefCell::new(CCtx::create());
}

/// A compression of a sample at a level (or of all the data, given none).
type Compress<'a> = dyn FnMut(Option<&[u8]>, i32) -> Result<Compressed> + 'a;

/// What to make of `data` at `level`: `compress` compresses a sample at a level (or all of
/// `data`, given none), as [`compressed`] does.
pub(crate) fn plan_by(level: Level, how: How, data: &[&[u8]], compress: &mut Compress) -> Result<Plan> {
    let mut trial = |sample: &[u8], level| compress(Some(sample), level);
    let n = total(data);
    // (where all of it is at hand already, compressed at the probe level now)
    let probe_all = |trial: &mut dyn FnMut(&[u8], i32) -> Result<Compressed>| match n <= SAMPLE.get() {
        true => trial(&data.concat(), PROBE_LEVEL.get()).map(|c| match c.filter(|c| saves(c.len(), n)) {
            Some(c) => Plan::Done(Some(c), PROBE_LEVEL.get()),
            None => Plan::Done(None, STORED),
        }),
        false => Ok(Plan::Probe),
    };
    match level {
        Level::STORED => return Ok(Plan::Done(None, STORED)),
        _ if how == How::Light => return probe_all(&mut trial),
        _ => {}
    }
    match level {
        Level::Zstd(l) if n <= SAMPLE.get() => Ok(Plan::Done(trial(&data.concat(), l)?, l)),
        Level::Zstd(l) => {
            let sample = sampled(data);
            let c = trial(&sample, l)?.map_or(sample.len(), |c| c.len());
            Ok(Plan::Level(l, scaled(c, sample.len(), n)))
        }
        Level::Of(level) => {
            let p = Pick::of(level);
            let sample = sampled(data);
            // (planned before, of the same data: its trials not made again)
            let k = (n >= SMALL.get()).then(|| (key_how(data, how), level));
            let before = k.and_then(|k| PLANNED.lock().unwrap().as_ref().and_then(|m| m.get(&k).copied()));
            if let Some(b) = before {
                return Ok(match b {
                    Planned::Probe => Plan::Probe,
                    Planned::Stored => Plan::Done(None, STORED),
                    Planned::Done(l) => Plan::Done(compress(Some(&sample), l)?, l),
                    Planned::Level(l, size) => Plan::Level(l, size),
                });
            }
            let plan = plan_of(&p, how, n, &sample, compress)?;
            if let Some(k) = k {
                let b = match plan {
                    Plan::Probe => Planned::Probe,
                    Plan::Done(None, STORED) => Planned::Stored,
                    Plan::Done(_, l) => Planned::Done(l),
                    Plan::Level(l, size) => Planned::Level(l, size),
                };
                let mut m = PLANNED.lock().unwrap();
                let m = m.get_or_insert_with(HashMap::new);
                if m.len() >= PLANNED_MAX {
                    m.clear();
                }
                m.insert(k, b);
            }
            Ok(plan)
        }
    }
}

/// What [`plan_by`] made of data of at least [`SMALL`] bytes at a level of [`Level::Of`], by
/// the data's [`key_how`] and the level: the same data met again (a file in two images, of
/// two encoders) planned alike without its trials, the sample of all of it compressed at the
/// level planned. A plan is a function of the data alone, so that what is planned does not
/// depend on which was met first. (Of the Windows 7 SP1 DVD's trials, 15% of the bytes were
/// of data met before.)
#[derive(Clone, Copy)]
enum Planned {
    Probe,
    Stored,
    Done(i32),
    Level(i32, usize),
}

static PLANNED: std::sync::Mutex<Option<HashMap<(StreamKey, u8), Planned>>> = std::sync::Mutex::new(None);

/// Plans [`PLANNED`] holds at most, before it is cleared.
const PLANNED_MAX: usize = 1 << 20;

/// [`plan_by`] at a level of [`Level::Of`] picking `p`, of `n` bytes of data of which `sample` is
/// the sample, by `compress`.
fn plan_of(p: &Pick, how: How, n: usize, sample: &[u8], compress: &mut Compress) -> Result<Plan> {
    let len = |c: &Option<Vec<u8>>| c.as_ref().map_or(sample.len(), Vec::len);
    let probe = compress(Some(sample), PROBE_LEVEL.get())?;
    if !saves(len(&probe), sample.len()) {
        return match sample.len() < n {
            true => Ok(Plan::Probe),
            false => Ok(Plan::Done(None, STORED)),
        };
    }
    if n < SMALL.get() {
        return Ok(Plan::Done(compress(None, p.base.0)?, p.base.0));
    }
    let mut tried = Vec::new();
    for level in [p.base.0].into_iter().chain(p.higher.iter().map(|h| h.0)).chain(p.lower.iter().copied()) {
        tried.push((level, if level == PROBE_LEVEL.get() { probe.clone() } else { compress(Some(sample), level)? }));
    }
    let lens: Vec<usize> = tried.iter().map(|(_, c)| len(c)).collect();
    let (higher, lower) = lens[1..].split_at(p.higher.len());
    let level = pick(p, sample.len(), lens[0], higher, lower);
    let best = tried.into_iter().find(|t| t.0 == level).expect("a level tried");
    let size = scaled(len(&best.1), sample.len(), n);
    // (dense data at the dense level above any picked: where larger than a sample, as
    // a sample does not tell; else where it is smaller, as the sample is all of it)
    match how {
        How::Dense(dense) if p.dense.is_some() && sample.len() < n && level >= p.base.0 => Ok(Plan::Level(dense, size)),
        How::Dense(dense) if p.dense.is_some() && sample.len() == n && level >= p.base.0 => {
            let c = compress(Some(sample), dense)?;
            Ok(if len(&c) < len(&best.1) { Plan::Done(c, dense) } else { Plan::Done(best.1, best.0) })
        }
        _ if sample.len() == n => Ok(Plan::Done(best.1, best.0)),
        _ => Ok(Plan::Level(best.0, size)),
    }
}

/// The zstd level that `p` picks for a sample of `n` bytes, by the bytes it compresses to at
/// its base, its higher and its lower levels (in their order): above the base by the time to
/// compress, else below where smaller by 1/32 at least.
pub(crate) fn pick(p: &Pick, n: usize, base: usize, higher: &[usize], lower: &[usize]) -> i32 {
    let cost = |len: usize, speed: f64| len as f64 + p.rate * n as f64 / speed;
    let margin = LOWER_MARGIN.get();
    let (mut best, mut least) = (p.base.0, cost(base, p.base.1));
    for (&(level, speed), &len) in p.higher.iter().zip(higher) {
        if cost(len, speed) < least {
            (best, least) = (level, cost(len, speed));
        }
    }
    if best == p.base.0
        && let Some((&level, _)) = p.lower.iter().zip(lower).find(|&(_, &len)| len <= base - base / margin)
    {
        best = level;
    }
    best
}

/// Bytes of `pieces`, one after the other.
pub(crate) fn total(pieces: &[&[u8]]) -> usize {
    pieces.iter().map(|p| p.len()).sum()
}

/// [`SAMPLE`] bytes of `data` in [`SAMPLE_CHUNKS`] chunks spread evenly over it, or all of it.
pub(crate) fn sampled<'a>(data: &[&'a [u8]]) -> Cow<'a, [u8]> {
    let n = total(data);
    let (sample, chunks) = (SAMPLE.get(), SAMPLE_CHUNKS.get());
    if let [d] = data
        && n <= sample
    {
        return (*d).into();
    }
    if n <= sample {
        return data.concat().into();
    }
    let chunk = sample / chunks;
    let step = (n - chunk) / (chunks - 1);
    (0..chunks).flat_map(|i| cut(data, i * step, chunk)).flatten().copied().collect::<Vec<_>>().into()
}

/// The [`StreamKey`] of `data`.
pub(crate) fn key(data: &[&[u8]]) -> StreamKey {
    let mut h = Xxh3::new();
    data.iter().for_each(|p| h.update(p));
    (h.digest128(), total(data))
}

/// [`key`] of `data` compressed as `how` tells: a dense or light stream is kept apart from the
/// same bytes compressed otherwise (as a file compared with its formula, say).
pub(crate) fn key_how(data: &[&[u8]], how: How) -> StreamKey {
    let (h, n) = key(data);
    let h = match how {
        How::Usual => h,
        How::Dense(l) => !h ^ (l as u128) << 64,
        How::Light => h.rotate_left(64),
    };
    (h, n)
}

/// [`checksum`] of `data`.
pub(crate) fn checksum_of(data: &[&[u8]]) -> u64 {
    let mut h = Xxh64::new(0);
    data.iter().for_each(|p| h.update(p));
    h.digest()
}

/// The length of the archive of `d` as it is ([`Encoder::raw_len`]), compressed as `c`
/// ([`compressed`]) if given ([`archived_raw_len`]'s reference).
#[cfg(test)]
pub(crate) fn archived_raw(d: &[u8], c: Option<&[u8]>) -> usize {
    let mut n = Count(0);
    n.put(&HEADER_MAGIC);
    content_block(&mut n, true, &[d], c);
    let blocks = n.0 - HEADER_MAGIC.len();
    footer(&mut n, blocks);
    n.0
}

aqr_common::zopt! {
    /// Data longer than this that [`Encoder::smaller`] compresses to compare with a formula is only
    /// counted, not kept to be archived if it is (compressed again then): what compressing it holds
    /// is then a window of its output, whatever its length. (Below, it is kept, of the store.)
    pub(crate) static RAW_KEPT_MAX: Opt<usize> = Opt::size("raw-kept", 0, "bytes of data compared with a formula that are kept compressed, not only counted")
        .derived(|| 4 * BLOCK, "4 blocks");
}

/// [`archived_raw`] of `d` compressed to `c` bytes ([`compressed_len`]) if given, the compressed
/// bytes not at hand.
pub(crate) fn archived_raw_len(d: &[u8], c: Option<usize>) -> usize {
    static ZEROS: [u8; 1 << 16] = [0; 1 << 16];
    let mut n = Count(0);
    n.put(&HEADER_MAGIC);
    match c {
        // (bytes standing in for the compressed ones, which a count does not read)
        Some(c) => {
            let parts: Vec<&[u8]> = (0..c).step_by(ZEROS.len()).map(|i| &ZEROS[..(c - i).min(ZEROS.len())]).collect();
            block_of(&mut n, LEAF_ZSTD, &parts);
        }
        None => block_of(&mut n, LEAF_RAW, &[d, &[0; 8]]),
    }
    let blocks = n.0 - HEADER_MAGIC.len();
    footer(&mut n, blocks);
    n.0
}

/// Appends a block of `data`: compressed as `c` ([`compressed`]) if given, else uncompressed
/// (leaf or non-leaf).
pub(crate) fn content_block(out: &mut impl Out, leaf: bool, data: &[&[u8]], c: Option<&[u8]>) {
    match c {
        Some(c) => block(out, if leaf { LEAF_ZSTD } else { NON_LEAF_ZSTD }, c),
        None => {
            let sum = checksum_of(data).to_le_bytes();
            block_of(out, if leaf { LEAF_RAW } else { NON_LEAF_RAW }, &[data, &[&sum]].concat())
        }
    }
}

/// The compressed external structure of `data` (Zstandard, XXH64), even where the Zstandard
/// blocks are longer than `data`: for blocks that are always compressed. At level [`STORED`],
/// raw Zstandard blocks.
pub(crate) fn always_compressed(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Vec<u8>> {
    match level {
        STORED => Ok(stored_external(data)),
        _ => Ok(external(c, data, level, threads, usize::MAX)?.0),
    }
}

/// The zstd level that [`always_compressed`] takes for raw Zstandard blocks (zstd's own level 0
/// is its default level).
pub(crate) const STORED: i32 = 0;

/// The largest Zstandard block (RFC 8878, `Block_Maximum_Size`).
const ZSTD_BLOCK_MAX: usize = 128 << 10;

/// The compressed external structure of `data` (Zstandard, XXH64) of raw Zstandard blocks.
pub(crate) fn stored_external(data: &[&[u8]]) -> Vec<u8> {
    let data = data.concat();
    let chunks: Vec<&[u8]> = match data.len() {
        0 => vec![&[]],
        _ => data.chunks(ZSTD_BLOCK_MAX).collect(),
    };
    let mut out = Vec::with_capacity(data.len() + 3 * chunks.len() + 16);
    // longer by the block headers: 0, then by how much
    out.push(0);
    varint::put(&mut out, 3 * chunks.len() as u64);
    // the window descriptor (a mantissa of 0): as large as a block at least (Block_Maximum_Size
    // is the smaller of the window and 128 KiB), of 1 KiB at least
    let block = data.len().clamp(1 << 10, ZSTD_BLOCK_MAX);
    out.push(((block.next_power_of_two().trailing_zeros() - 10) << 3) as u8);
    for (i, c) in chunks.iter().enumerate() {
        // Last_Block, Block_Type 0 (raw), Block_Size
        let head = (c.len() as u32) << 3 | (i + 1 == chunks.len()) as u32;
        out.extend_from_slice(&head.to_le_bytes()[..3]);
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&checksum_of(&[&data]).to_le_bytes());
    out
}

aqr_common::zopt! {
    /// Zstandard's jobs of multithreaded compression, and how much of the window each job takes
    /// over from the data before it (9: all of it). Data is cut in jobs however many the threads,
    /// so that it compresses the same on any; this loses 0.1-0.2% against compressing it in one
    /// piece, at levels 3 and 4.
    pub(crate) static ZSTD_JOB: Opt<usize> = Opt::size("zstd:job", 16 << 20, "bytes of a job of multithreaded compression (changes the output)");
}

aqr_common::zopt! {
    /// Threads compressing a stream of more than two of Zstandard's jobs in
    /// [`Encoder::precompress`], which the rest would wait for on one (10gb's enwik batch: a stream
    /// of 0.5 GB, 5.3 s at level 9). Each of Zstandard's workers holds a context of the level and
    /// window of its own (all of them, 10, took 0.2-0.3 GB more of 10gb's peak).
    pub(crate) static LARGE_THREADS: Opt<usize> = Opt::value("zstd:large-threads", 4, "threads on a stream of more than two jobs, ahead of the rest");
}

aqr_common::zopt! {
    /// Bytes of data above which it is compressed with long-distance matching, over a window of
    /// all of it up to [`WINDOW_LOG_MAX`]. Below, the window is the level's own (at level 3, 2 MiB
    /// of most data), which misses what repeats from farther: at 8 MiB, of the files of an archive
    /// of Deskflow.app, the rests of its two programs (a stream of 8.3 MB that a stream class
    /// put together) archived to 3.07 MB, and to 2.28 MB with a window of all of it and
    /// long-distance matching. From 1 MiB on rather than 8 MiB, the applications of
    /// `docs/macos-app-todo.md` archive to 1088.91 -> 1080.60 MB (-0.76%; a window alone, without
    /// long-distance matching, 1085.29 MB) and 10gb to 2464.75 -> 2462.14 MB (-0.11%), the
    /// instructions retired 0.4% more.
    pub(crate) static LONG: Opt<Option<usize>> = Opt::size_or_off("zstd:ldm", Some(1 << 20), "bytes above which long-distance matching is on; no-ldm: never");
}

/// The largest window, as a power of two, that every decoder takes (spec, "Zstandard
/// compression method").
pub const WINDOW_LOG_MAX: u32 = 27;
aqr_common::zopt! {
    /// How much of the window each job takes over from the data before it (9: all of it).
    pub(crate) static ZSTD_OVERLAP: Opt<u32> = Opt::value("zstd:overlap", 9, "log of the overlap of jobs with the data before them (9: all of it)");
}

/// The compressed external structure of `data` (Zstandard, XXH64), unless the Zstandard
/// blocks would be longer than `data`. It is compressed on up to `threads` threads (those of
/// the pool there are, [`aqr_common::pool`]), always in
/// Zstandard's multithreaded mode: the output does not depend on how many (up to 1 MiB of data,
/// it is also that of the single-threaded mode), nor on what context `c` compressed before.
pub(crate) fn compressed(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Option<Vec<u8>>> {
    compressed_to(c, data, level, threads, usize::MAX)
}

aqr_common::zopt_namespace!(
    "kin",
    "streams that compress after others of their class, kept compressible (aqr-formula, aqr-compress)"
);

aqr_common::zopt! {
    /// The most bytes of the samples of a stream's kin put before its own sample, to tell
    /// whether it compresses after them ([`Encoder::compresses_after_each`]): four streams'
    /// samples.
    pub(crate) static KIN_PREFIX: Opt<usize> = Opt::size("kin:prefix", 4 << 20, "bytes of the kin's samples put before a stream's sample at most")
        .derived(|| 4 * SAMPLE.get(), "four samples (zstd:sample)");
    /// The zstd level of the trial: the base level (level 1 caught half of what WinUHA's
    /// installers gain, keeping few positions far back).
    pub(crate) static KIN_LEVEL: Opt<i32> = Opt::value("kin:level", 3, "zstd level of the trial of a stream after its kin");
}

/// The bytes `d` compresses to at `level` after `prefix` (a raw prefix, all of it within the
/// window: as where they are one after another in a stream), or None where they would be more
/// than `d`.
pub(crate) fn after_prefix_len(d: &[u8], prefix: &[u8], level: i32) -> Result<Option<usize>> {
    let _s = aqr_common::trace::span("zstd");
    let zstd = zstd_error;
    let mut c = CCtx::create();
    let window = (64 - ((prefix.len() + d.len()).max(2) as u64 - 1).leading_zeros()).clamp(10, WINDOW_LOG_MAX);
    for p in [
        CParameter::CompressionLevel(level),
        CParameter::ContentSizeFlag(false),
        CParameter::ChecksumFlag(false),
        CParameter::WindowLog(window),
    ] {
        c.set_parameter(p).map_err(zstd)?;
    }
    c.ref_prefix(prefix).map(drop).map_err(zstd)?;
    let mut out = Vec::with_capacity(zstd_safe::compress_bound(d.len()));
    let mut input = InBuffer::around(d);
    loop {
        let pos = out.len();
        let end = ZSTD_EndDirective::ZSTD_e_end;
        let left = c.compress_stream2(&mut OutBuffer::around_pos(&mut out, pos), &mut input, end).map_err(zstd)?;
        if left == 0 {
            break;
        }
        out.reserve(left.max(4096));
    }
    Ok((out.len() < d.len()).then_some(out.len()))
}

/// The bytes a stream of `n` bytes compresses to, where a sample of `of` bytes of it compresses
/// to `to`.
pub(crate) fn scaled(to: usize, of: usize, n: usize) -> usize {
    (to as f64 * n as f64 / of.max(1) as f64) as usize
}

/// The most memory a context is kept with ([`compressed`]): it holds what the largest data
/// and highest level it compressed took, and a new one gives that back.
const CCTX_KEPT: usize = 4 << 20;

/// [`compressed`], expected to give about `size` bytes: room is made for a little more, and
/// for as much as Zstandard may give only where that does not do (the output does not depend
/// on the room it has). A stream is not given room for its own size again for nothing.
pub(crate) fn compressed_to(
    c: &mut CCtx,
    data: &[&[u8]],
    level: i32,
    threads: usize,
    size: usize,
) -> Result<Option<Vec<u8>>> {
    let (c, longer) = external(c, data, level, threads, size)?;
    Ok((!longer).then_some(c))
}

/// The compressed external structure of `data` as [`compressed_to`] makes it, and whether its
/// Zstandard blocks are longer than `data`.
fn external(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize, size: usize) -> Result<(Vec<u8>, bool)> {
    external_with(c, data, level, threads, size, dict::With::None)
}

fn zstd_error(e: zstd_safe::ErrorCode) -> Error {
    Error::new(format!("zstd: {}", zstd_safe::get_error_name(e)))
}

/// Sets context `c` to compress `data` at `level` on up to `threads` threads, with `dict`:
/// the workers of the pool lent while it compresses, and the length of `data`.
fn begin<'a>(
    c: &mut CCtx<'a>,
    data: &[&[u8]],
    level: i32,
    threads: usize,
    dict: dict::With<'a>,
) -> Result<(aqr_common::pool::Reservation, usize)> {
    let zstd = zstd_error;
    // A context compressed with before keeps its workers and buffers, not its parameters.
    c.reset(ResetDirective::SessionAndParameters).map_err(zstd)?;
    // Zstandard's workers counted in aqr-common's pool: as many workers of it reserved, but
    // for the calling thread, which waits while they compress (one job a worker at most).
    let jobs = total(data).div_ceil(ZSTD_JOB.get()).max(1);
    let lent = aqr_common::pool::reserve(threads.min(jobs).saturating_sub(1));
    let threads = lent.len() + 1;
    for p in [
        CParameter::CompressionLevel(level),
        CParameter::ContentSizeFlag(false),
        CParameter::ChecksumFlag(false),
        CParameter::DictIdFlag(false),
        CParameter::NbWorkers(threads.max(1) as u32),
        CParameter::JobSize(ZSTD_JOB.get() as u32),
        CParameter::OverlapSizeLog(ZSTD_OVERLAP.get()),
    ] {
        c.set_parameter(p).map_err(zstd)?;
    }
    // Compressed as data in one piece is (of a size known, ended with its last bytes: ended
    // apart, data ending at the end of a job ends with an empty block).
    let len = total(data);
    // Long data matched from afar, over a window of all of it as far as decoders must take.
    if LONG.get().is_some_and(|long| len > long) {
        let window = (64 - (len as u64 - 1).leading_zeros()).min(WINDOW_LOG_MAX);
        c.set_parameter(CParameter::WindowLog(window)).map_err(zstd)?;
        c.set_parameter(CParameter::EnableLongDistanceMatching(true)).map_err(zstd)?;
    }
    match dict {
        dict::With::None => {}
        dict::With::Tables(d) => c.ref_cdict(d).map(drop).map_err(zstd)?,
        dict::With::Raw(d) => c.ref_prefix(d).map(drop).map_err(zstd)?,
    }
    c.set_pledged_src_size(Some(len as u64)).map_err(zstd)?;
    Ok((lent, len))
}

/// The length of what [`compressed`] gives of `data` at `level` (`None` where it does not
/// compress), its output made a window at a time and counted rather than held: the output does
/// not depend on the room it has (Zstandard's multithreaded mode).
pub(crate) fn compressed_len(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Option<usize>> {
    let _s = aqr_common::trace::span("zstd");
    let zstd = zstd_error;
    let (_lent, len) = begin(c, data, level, threads, dict::With::None)?;
    let pieces: Vec<&[u8]> = data.iter().copied().filter(|p| !p.is_empty()).collect();
    let swept = SWEEP.with(|s| s.borrow().is_some());
    let (mut window, mut told) = (Vec::with_capacity(SWEPT_WINDOW), 0);
    // the frame's header, and the bytes of the frame
    let (mut header, mut n) = (Vec::with_capacity(6), 0usize);
    for i in 0..pieces.len().max(1) {
        let (piece, last) = (pieces.get(i).copied().unwrap_or_default(), i + 1 >= pieces.len());
        let end = if last { ZSTD_EndDirective::ZSTD_e_end } else { ZSTD_EndDirective::ZSTD_e_continue };
        let mut input = InBuffer::around(piece);
        loop {
            window.clear();
            let left = c.compress_stream2(&mut OutBuffer::around(&mut window), &mut input, end).map_err(zstd)?;
            let more = (6 - header.len()).min(window.len());
            header.extend_from_slice(&window[..more]);
            n += window.len();
            if swept {
                let consumed = c.get_frame_progression().consumed;
                SWEEP.with(|s| s.borrow_mut().as_mut().map(|s| s.add(consumed.saturating_sub(told))));
                told = consumed;
            }
            if if last { left == 0 } else { input.pos() == piece.len() } {
                break;
            }
        }
    }
    if c.sizeof() > CCTX_KEPT {
        *c = CCtx::create();
    }
    // (as [`external_with`] puts the header in place: a frame header of no flags, then the
    // window descriptor)
    let [m0, m1, m2, m3, 0, _] = header[..] else {
        return Err(Error::new("zstd: unexpected frame header"));
    };
    assert_eq!([m0, m1, m2, m3], ZSTD_MAGIC);
    let blocks = n - 6;
    if blocks > len {
        return Ok(None);
    }
    let head = match len - blocks {
        0 => 1 + varint::len(0),
        delta => varint::len(delta as u64),
    } + 1;
    Ok(Some(head + blocks + 8))
}

/// [`external`], compressed with dictionary `dict` (one a context of a lifetime as long as
/// `c`'s may refer to).
pub(crate) fn external_with<'a>(
    c: &mut CCtx<'a>,
    data: &[&[u8]],
    level: i32,
    threads: usize,
    size: usize,
    dict: dict::With<'a>,
) -> Result<(Vec<u8>, bool)> {
    let _s = aqr_common::trace::span("zstd");
    #[cfg(test)]
    tests::COMPRESSED.with(|n| n.set(n.get() + 1));
    let zstd = zstd_error;
    let (_lent, len) = begin(c, data, level, threads, dict)?;
    let pieces: Vec<&[u8]> = data.iter().copied().filter(|p| !p.is_empty()).collect();
    // the frame, then its checksum
    let bound = zstd_safe::compress_bound(len).saturating_add(8);
    let room = size.saturating_add(size / 8).saturating_add(4096).saturating_add(8);
    let mut frame = Vec::with_capacity(room.min(bound));
    // (told of, the bytes compressed so far after each call, and the output a window at a time
    // where Zstandard's workers make it: on this thread, it depends on the room it has)
    let swept = SWEEP.with(|s| s.borrow().is_some());
    let windowed = swept && len > SWEPT_MIN;
    let (mut window, mut told) = (Vec::with_capacity(if windowed { SWEPT_WINDOW } else { 0 }), 0);
    for i in 0..pieces.len().max(1) {
        let (piece, last) = (pieces.get(i).copied().unwrap_or_default(), i + 1 >= pieces.len());
        let end = if last { ZSTD_EndDirective::ZSTD_e_end } else { ZSTD_EndDirective::ZSTD_e_continue };
        let mut input = InBuffer::around(piece);
        loop {
            let left = if windowed {
                window.clear();
                let left = c.compress_stream2(&mut OutBuffer::around(&mut window), &mut input, end).map_err(zstd)?;
                frame.extend_from_slice(&window);
                left
            } else {
                if frame.len() == frame.capacity() {
                    frame.reserve_exact(bound.saturating_sub(frame.len()).max(4096));
                }
                let pos = frame.len();
                c.compress_stream2(&mut OutBuffer::around_pos(&mut frame, pos), &mut input, end).map_err(zstd)?
            };
            if swept {
                let consumed = c.get_frame_progression().consumed;
                SWEEP.with(|s| s.borrow_mut().as_mut().map(|s| s.add(consumed.saturating_sub(told))));
                told = consumed;
            }
            if if last { left == 0 } else { input.pos() == piece.len() } {
                break;
            }
        }
    }
    if c.sizeof() > CCTX_KEPT {
        *c = CCtx::create();
    }
    // a frame header of no flags, then the window descriptor
    let [m0, m1, m2, m3, 0, window, blocks @ ..] = &frame[..] else {
        return Err(Error::new("zstd: unexpected frame header"));
    };
    assert_eq!([*m0, *m1, *m2, *m3], ZSTD_MAGIC);
    let longer = blocks.len() > len;
    // the header put in place of the frame's, in the same vector: the original size by how much
    // longer it is, or `0` then by how much shorter
    let mut head = Vec::with_capacity(12);
    match len - blocks.len().min(len) {
        0 => {
            head.push(0);
            varint::put(&mut head, (blocks.len() - len) as u64);
        }
        delta => varint::put(&mut head, delta as u64),
    }
    head.push(*window);
    let sum = checksum_of(data).to_le_bytes();
    let Some(at) = 6usize.checked_sub(head.len()) else {
        // (a varint of 6 bytes: 32 GiB that compress to nothing)
        return Ok(([&head[..], &frame[6..], &sum].concat(), longer));
    };
    frame[at..6].copy_from_slice(&head);
    frame.drain(..at);
    frame.extend_from_slice(&sum);
    Ok((frame, longer))
}
