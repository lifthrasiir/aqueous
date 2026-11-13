//! A texture's blocks coded (FORMAT.md, "The walk"): layer by layer, each layer's mipmaps from
//! the smallest, each surface by its parts in turn ([`crate::model`]). A large surface is coded
//! in stripes ([`stripes`]): the first goes on with the models of the texture and its stream,
//! each other one starts from a copy of them as they are before the surface, with a stream of
//! its own, and is decoded apart from the rest (on another thread, or as a task of the Wasm
//! module).

use aqr_common::arith::ArithDecoder;
use aqr_common::packed;

use crate::bc7::Bc7;
use crate::bcn::Bcn;
use crate::coder::{CATS, Dec, Enc, Side};
use crate::layout::{Format, Group, Layout, Shape, Surface};
use crate::plane::across_rows;
use crate::{Result, corrupt};

/// A surface is coded in a stripe per this many blocks of it, as rows of blocks, and in at most
/// [`MAX_STRIPES`] stripes: its stripes but the first learn their models anew from the copy, a
/// cost of about 0.3% (README).
pub const STRIPE_BLOCKS: usize = 16384;
pub const MAX_STRIPES: usize = 16;

/// The stripes of a surface of `blocks` blocks (one where the family takes none).
pub fn stripes<F: Family>(blocks: usize) -> usize {
    match F::STRIPED {
        true => (blocks / STRIPE_BLOCKS).clamp(1, MAX_STRIPES),
        false => 1,
    }
}

/// The rows of blocks of stripe `k` of `n` of a surface of `bh` rows (none empty, as a surface
/// of n stripes has at least n rows: no row is of more than 16384 blocks).
pub fn rows(k: usize, n: usize, bh: usize) -> std::ops::Range<usize> {
    k * bh / n..(k + 1) * bh / n
}

/// A family of block formats, as the walk takes it: the models of their blocks' parts, the
/// planes the parts make as they decode, and the buffers of both.
pub trait Family: 'static {
    /// The group of formats it takes.
    const GROUP: crate::layout::Group;

    /// The shape of format `f`'s blocks (the same as [`Format::shape`], which a family may know
    /// without asking the format).
    #[inline(always)]
    fn shape(f: Format) -> Shape {
        f.shape()
    }
    /// Whether a large surface is coded in stripes ([`stripes`]).
    const STRIPED: bool = true;

    /// The models of a texture, or of a stripe.
    type Models: Clone + Send + Sync;
    /// The planes of a surface's parts.
    type Planes: Default + Send + Sync;
    /// Buffers of a surface's planes, kept to be used again.
    type Bufs: Send;
    /// Buffers that coding a surface, and upsampling its planes, work in.
    type Work: Send;

    fn models(f: Format) -> Self::Models;
    /// Bytes that [`Family::models`] takes of [`aqr_api::stack`].
    fn models_plan(f: Format) -> u64;
    /// Models `m` made those of `o`, in their vectors.
    fn copy_models(m: &mut Self::Models, o: &Self::Models);

    /// Room for planes of `px` pixels.
    fn bufs(f: Format, px: usize) -> Self::Bufs;
    /// Bytes that [`Family::bufs`] takes of [`aqr_api::stack`].
    fn bufs_plan(f: Format, px: usize) -> u64;
    /// The buffers of planes `p` given back.
    fn give(b: &mut Self::Bufs, p: Self::Planes);
    fn is_empty(p: &Self::Planes) -> bool;

    /// Room for surfaces of `blocks` blocks, upsamplings to `cols` columns of `across` values.
    fn work(f: Format, blocks: usize, cols: usize, across: usize) -> Self::Work;
    /// Bytes that [`Family::work`] takes of [`aqr_api::stack`].
    fn work_plan(f: Format, blocks: usize, cols: usize, across: usize) -> u64;
    /// The values an upsampling sums across of each pixel of a row (of its largest plane).
    fn across_values(f: Format) -> usize;

    /// The planes of the blocks `b` of `size` blocks, below those of `p`.
    fn planes_of(f: Format, b: &[u8], size: (usize, usize), p: &mut Self::Planes);
    /// Planes `p`, of the next smaller mipmap, upsampled to the rows `rows` (of pixels) of a
    /// surface `w` pixels wide, in `bufs`.
    fn upsampled(
        p: &Self::Planes,
        f: Format,
        w: usize,
        rows: std::ops::Range<usize>,
        bufs: &mut Self::Bufs,
        work: &mut Self::Work,
    ) -> Self::Planes;
    /// Codes the blocks `b` of a stripe of `size` blocks of a surface, `up` being the planes of
    /// the surface before, upsampled to the stripe; its planes made in `bufs`.
    #[allow(clippy::too_many_arguments)]
    fn stripe(
        s: &mut impl Side,
        m: &mut Self::Models,
        f: Format,
        b: &mut [u8],
        size: (usize, usize),
        up: &Self::Planes,
        bufs: &mut Self::Bufs,
        work: &mut Self::Work,
    ) -> Result<Self::Planes>;
}

/// The surface's planes `new` made those of the surface before (`prev`), the buffers of those
/// given back to `spare`: from the planes `prev` held, or from `idle` where it held none (the
/// first surface of a layer). So `spare` has every buffer again.
fn turn<F: Family>(prev: &mut F::Planes, new: F::Planes, spare: &mut F::Bufs, idle: &mut F::Bufs) {
    let old = std::mem::replace(prev, new);
    match F::is_empty(&old) {
        true => std::mem::swap(spare, idle),
        false => F::give(spare, old),
    }
}

/// `f(i)` for each `i` of `0..n`, apart from the calling thread (on up to `threads − 1` workers
/// of the pool, or as tasks of the Wasm module, which see the memory as it is when they start),
/// as `here` runs on the calling thread; then `each(mine, i, out)` of what `here` gave and each
/// result, in order (each as it is joined); or all in turn, `here` first, where there are no
/// threads to take.
fn apart<T>(
    n: usize,
    threads: usize,
    f: impl Fn(usize) -> Vec<u8> + Sync,
    here: impl FnOnce() -> T,
    mut each: impl FnMut(&mut T, usize, Vec<u8>) -> Result<()>,
) -> Result<T> {
    #[cfg(target_family = "wasm")]
    if threads > 1 {
        let g = |i: usize, out: &mut Vec<u8>| *out = f(i);
        // SAFETY: joined below (or as they are dropped), before `g` goes
        let mut t = unsafe { aqr_api::task::spawn(n, &g) };
        let mut mine = here();
        for i in 0..n {
            each(&mut mine, i, t.join(i))?;
        }
        return Ok(mine);
    }
    #[cfg(not(target_family = "wasm"))]
    if threads > 1 {
        use aqr_common::pool;
        return pool::scope(threads - 1, |s| {
            let mut mine;
            if s.workers() == 0 {
                mine = here();
                for i in 0..n {
                    each(&mut mine, i, f(i))?;
                }
                return Ok(mine);
            }
            let f = &f;
            // (as many spawned as there are workers before `here`, as a spawn waits for one to
            // be idle; the rest after it)
            let mut hs: Vec<_> = (0..n.min(s.workers())).map(|i| s.spawn(move || f(i))).collect();
            mine = here();
            hs.extend((hs.len()..n).map(|i| s.spawn(move || f(i))));
            for (i, h) in hs.into_iter().enumerate() {
                each(&mut mine, i, h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))?;
            }
            Ok(mine)
        });
    }
    let mut mine = here();
    for i in 0..n {
        each(&mut mine, i, f(i))?;
    }
    Ok(mine)
}

/// What the walk of a layout holds, as [`Sizes::of`] tells: the most pixels of the planes of a
/// surface, and of a stripe coded on the calling thread (its planes upsampled), its blocks, the
/// columns and values of an upsampling there; and whether any surface is in stripes.
struct Sizes {
    planes: usize,
    up: usize,
    blocks: usize,
    cols: usize,
    across: usize,
    striped: bool,
}

/// The work of a stripe of `rows` (rows of blocks) of surface `m` of `surfaces`, `bw` blocks
/// wide: its blocks, the columns and values of its upsampling (none of the smallest surface),
/// and the pixels of its planes upsampled.
fn stripe_work<F: Family>(
    f: Format,
    surfaces: &[Surface],
    m: usize,
    rows: std::ops::Range<usize>,
) -> (usize, usize, usize, usize) {
    let Shape { fw, fh, .. } = F::shape(f);
    let bw = surfaces[m].bw;
    let blocks = rows.len() * bw;
    let Some(before) = surfaces.get(m + 1) else { return (blocks, 0, 0, 0) };
    let ch = F::across_values(f);
    let across = across_rows(before.bh * fh, rows.start * fh, rows.len() * fh) * bw * fw * ch;
    (blocks, bw * fw, across, blocks * fw * fh)
}

impl Sizes {
    fn of<F: Family>(f: Format, surfaces: &[Surface]) -> Sizes {
        let mut z = Sizes { planes: 0, up: 0, blocks: 0, cols: 0, across: 0, striped: false };
        let px = F::shape(f).pixels();
        for (m, sf) in surfaces.iter().enumerate() {
            let n = stripes::<F>(sf.blocks());
            let r = if n == 1 { 0..sf.bh } else { rows(0, n, sf.bh) };
            // (the largest mipmap's planes predict nothing: only those of its first stripe)
            z.planes = z.planes.max(if n == 1 || m > 0 { sf.blocks() * px } else { r.len() * sf.bw * px });
            let (blocks, cols, across, up) = stripe_work::<F>(f, surfaces, m, r);
            z.blocks = z.blocks.max(blocks);
            z.cols = z.cols.max(cols);
            z.across = z.across.max(across);
            z.up = z.up.max(up);
            z.striped |= n > 1;
        }
        z
    }
}

/// The memory plan of decoding layout `l` on WebAssembly ([`aqr_api::stack`]), given `held`
/// bytes of the stack taken before ([`decode`]'s), the instance's memory as it starts (`initial`
/// bytes, whole pages): the most the instance takes, and the most a task does, in bytes. The instance takes the walk's buffers once, and as a task's
/// stripe is taken, a vector of its blocks; each task a copy of the instance's memory, with its
/// stripe's blocks, work, planes and planes upsampled on top.
pub fn plan<F: Family>(l: &Layout, initial: u64, held: u64) -> (u64, u64) {
    use aqr_api::stack::block;
    let pages = |n: u64| n.div_ceil(1 << 16) << 16;
    let (f, bb) = (l.format, F::shape(l.format).bytes as u64);
    let surfaces: Vec<Surface> = l.surfaces_in(F::shape(f)).collect();
    let z = Sizes::of::<F>(f, &surfaces);
    let walk = block(SURFACE_BYTES * surfaces.len() as u64)
        + F::models_plan(f) * (1 + z.striped as u64)
        + F::work_plan(f, z.blocks, z.cols, z.across)
        + 2 * F::bufs_plan(f, z.planes)
        + F::bufs_plan(f, z.up);
    let top = initial + held + walk;
    let (mut end, mut most) = (pages(top), 0);
    for _ in 0..l.layers {
        for (m, sf) in surfaces.iter().enumerate().rev() {
            let n = stripes::<F>(sf.blocks());
            if n == 1 {
                continue;
            }
            let tasks: Vec<u64> = (1..n)
                .map(|i| {
                    let (blocks, cols, across, up) = stripe_work::<F>(f, &surfaces, m, rows(i, n, sf.bh));
                    let own = block(blocks as u64 * bb)
                        + F::work_plan(f, blocks, cols, across)
                        + F::bufs_plan(f, up)
                        + F::bufs_plan(f, blocks * F::shape(f).pixels());
                    end.max(pages(top + own))
                })
                .collect();
            // (the handles taken, then each stripe's blocks as it is joined)
            let joined = (1..n).map(|i| rows(i, n, sf.bh).len() as u64 * sf.bw as u64 * bb).max().unwrap_or(0);
            end = end.max(pages(top + block(n as u64 - 1) + block(joined)));
            most = most.max(tasks.into_iter().max().unwrap_or(0));
        }
    }
    (end, most)
}

/// Bytes of a [`Surface`] on WebAssembly.
const SURFACE_BYTES: u64 = 8;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<Surface>() == SURFACE_BYTES as usize);

/// Bytes of the tally of an encoder, after the stream of a stripe coded apart.
const TALLY: usize = 8 * CATS;

/// What the walk needs of a side of the coder: the side of each stripe coded apart, and what
/// comes back of it.
trait Sides: Sync {
    type S: Side;
    /// The side of the `j`-th stripe coded apart (from 1).
    fn side(&self, j: usize) -> Result<Self::S>;
    /// What comes back of stripe `b` coded by `s`: its blocks (decoding), or its stream
    /// (encoding); nothing where it failed.
    fn done(&self, s: Self::S, b: Vec<u8>) -> Vec<u8>;
}

/// Walks the blocks of `l` in `out` (when encoding, a copy of `src`, the blocks, which takes
/// every choice from them; zeros filled in when decoding): the first stripes by `main`, the
/// others by `sides`, `back` taking what comes back of each into its rows of the surface. A
/// stripe coded apart takes its blocks of `src` or zeros, which come back.
#[allow(clippy::too_many_arguments)]
fn walk<F: Family, X: Sides>(
    main: &mut X::S,
    sides: &X,
    l: &Layout,
    src: Option<&[u8]>,
    out: &mut [u8],
    threads: usize,
    mut back: impl FnMut(usize, Vec<u8>, &mut [u8]) -> Result<()>,
) -> Result<()> {
    let (f, bb) = (l.format, F::shape(l.format).bytes);
    let surfaces: Vec<Surface> = l.surfaces_in(F::shape(f)).collect();
    let Shape { fw, fh, .. } = F::shape(f);
    let z = Sizes::of::<F>(f, &surfaces);
    let mut models = F::models(f);
    // (stripes as tasks have a copy of the memory of their own, the models' copy theirs to change)
    let own = cfg!(target_family = "wasm") && threads > 1;
    let mut copy = std::sync::Mutex::new(z.striped.then(|| F::models(f)));
    let mut work = F::work(f, z.blocks, z.cols, z.across);
    let (mut spare, mut idle) = (F::bufs(f, z.planes), F::bufs(f, z.planes));
    let mut up = F::bufs(f, z.up);
    let layer: usize = surfaces.iter().map(|s| s.blocks() * bb).sum();
    let mut next = 1;
    for k in 0..l.layers as usize {
        let mut prev = F::Planes::default();
        let mut end = (k + 1) * layer;
        for m in (0..surfaces.len()).rev() {
            let (bw, bh) = (surfaces[m].bw, surfaces[m].bh);
            let size = surfaces[m].blocks() * bb;
            let start = end - size;
            let b = &mut out[start..end];
            end = start;
            let n = stripes::<F>(surfaces[m].blocks());
            if n == 1 {
                let u = F::upsampled(&prev, f, bw * fw, 0..bh * fh, &mut up, &mut work);
                let new = F::stripe(main, &mut models, f, b, (bw, bh), &u, &mut spare, &mut work)?;
                F::give(&mut up, u);
                turn::<F>(&mut prev, new, &mut spare, &mut idle);
                continue;
            }
            let rows0 = rows(0, n, bh).end;
            let top = rows0 * bw * bb;
            F::copy_models(copy.get_mut().unwrap().as_mut().expect("a copy"), &models);
            let (copy, coarse) = (&copy, &prev);
            let job = |i: usize| -> Vec<u8> {
                let r = rows(i + 1, n, bh);
                let len = r.len() * bw * bb;
                let mut b = match src {
                    Some(s) => s[start + r.start * bw * bb..][..len].to_vec(),
                    None => vec![0; len],
                };
                let (mut g, mut mine);
                let models: &mut F::Models = if own {
                    g = copy.lock().unwrap();
                    g.as_mut().expect("a copy")
                } else {
                    mine = copy.lock().unwrap().clone().expect("a copy");
                    &mut mine
                };
                let Ok(mut s) = sides.side(next + i) else { return vec![] };
                let (blocks, cols, across, px) = stripe_work::<F>(f, &surfaces, m, r.clone());
                let mut work = F::work(f, blocks, cols, across);
                let (mut up, mut bufs) = (F::bufs(f, px), F::bufs(f, blocks * F::shape(f).pixels()));
                let u = F::upsampled(coarse, f, bw * fw, r.start * fh..r.end * fh, &mut up, &mut work);
                match F::stripe(&mut s, models, f, &mut b, (bw, r.len()), &u, &mut bufs, &mut work) {
                    Ok(_) => sides.done(s, b),
                    Err(_) => vec![],
                }
            };
            let (head, rest) = b.split_at_mut(top);
            let (main, models, spare, work, up) = (&mut *main, &mut models, &mut spare, &mut work, &mut up);
            let here = || -> Result<F::Planes> {
                let u = F::upsampled(coarse, f, bw * fw, 0..rows0 * fh, up, work);
                let p = F::stripe(main, models, f, head, (bw, rows0), &u, spare, work);
                F::give(up, u);
                p
            };
            let mut at = 0;
            let new = apart(n - 1, threads, job, here, |new, i, out| {
                let r = rows(i + 1, n, bh);
                let chunk = &mut rest[at..at + r.len() * bw * bb];
                back(next + i, out, chunk)?;
                // (the largest mipmap's planes predict nothing)
                if m > 0
                    && let Ok(p) = new
                {
                    F::planes_of(f, chunk, (bw, r.len()), p);
                }
                at += chunk.len();
                Ok(())
            })??;
            next += n - 1;
            turn::<F>(&mut prev, new, spare, &mut idle);
        }
        // (the layer's last planes given back)
        if !F::is_empty(&prev) {
            F::give(&mut idle, prev);
        }
    }
    Ok(())
}

/// The streams of the stripes of `l` coded apart.
fn apart_streams<F: Family>(l: &Layout) -> usize {
    l.surfaces_in(F::shape(l.format)).map(|s| stripes::<F>(s.blocks()) - 1).sum::<usize>() * l.layers as usize
}

struct Encoders;

impl Sides for Encoders {
    type S = Enc;

    fn side(&self, _: usize) -> Result<Enc> {
        Ok(Enc::new())
    }

    fn done(&self, e: Enc, _: Vec<u8>) -> Vec<u8> {
        let mut out = e.c.finish();
        out.extend(e.cost.iter().flat_map(|c| c.to_le_bytes()));
        out
    }
}

/// The streams of the stripes coded apart (the `j`-th the `j − 1`-th of them).
struct Decoders<'a>(Vec<&'a [u8]>);

impl<'a> Sides for Decoders<'a> {
    type S = Dec<'a>;

    fn side(&self, j: usize) -> Result<Dec<'a>> {
        Ok(Dec { c: ArithDecoder::new(self.0.get(j.wrapping_sub(1)).ok_or_else(corrupt)?) })
    }

    fn done(&self, d: Dec<'a>, b: Vec<u8>) -> Vec<u8> {
        if d.c.overrun() { vec![] } else { b }
    }
}

/// The coded form of `blocks`, the blocks of layout `l` (exactly `l.len()` bytes): the first
/// stream, and the streams of the stripes coded apart (FORMAT.md, "The walk") as a packed byte
/// sequence, or nothing where there are none; and the bits each part took (in 1/2^16 bits). It
/// is the same whatever `threads` is.
///
/// (The first stream is kept apart from the others, as it is: the streams of textures alike
/// start alike, which the archive finds where they are not behind headers of other lengths.)
pub fn encode<F: Family>(l: &Layout, blocks: &[u8], threads: usize) -> (Vec<u8>, Vec<u8>, [u64; CATS]) {
    assert_eq!(blocks.len() as u64, l.len_in(F::shape(l.format)));
    let mut main = Enc::new();
    let mut streams = vec![vec![]; apart_streams::<F>(l) + 1];
    let mut cost = [0u64; CATS];
    walk::<F, _>(&mut main, &Encoders, l, Some(blocks), &mut blocks.to_vec(), threads, |j, mut out, _| {
        let tally = out.split_off(out.len() - TALLY);
        for (i, c) in cost.iter_mut().enumerate() {
            *c += u64::from_le_bytes(tally[8 * i..8 * i + 8].try_into().unwrap());
        }
        streams[j] = out;
        Ok(())
    })
    .expect("the encoder codes any blocks");
    for (c, x) in cost.iter_mut().zip(main.cost) {
        *c += x;
    }
    let apart = if streams.len() > 1 { packed::pack(&streams[1..]) } else { vec![] };
    (main.c.finish(), apart, cost)
}

/// The blocks of layout `l` from its first stream `main` and the streams coded apart `apart`
/// (as [`encode`] gives them), decoded on up to `threads` threads: in a vector of their bytes
/// reserved first, then the streams' slices ([`decode_held`]), and the walk's ([`plan`]).
pub fn decode<F: Family>(l: &Layout, main: &[u8], apart: &[u8], threads: usize) -> Result<Vec<u8>> {
    let len = usize::try_from(l.len_in(F::shape(l.format))).map_err(|_| corrupt())?;
    let n = apart_streams::<F>(l);
    let mut out = vec![0; len];
    let streams = match n {
        0 if apart.is_empty() => Vec::new(),
        0 => return Err(corrupt()),
        _ => packed::unpack(apart).ok_or_else(corrupt)?,
    };
    if streams.len() != n {
        return Err(corrupt());
    }
    let mut main = Dec { c: ArithDecoder::new(main) };
    walk::<F, _>(&mut main, &Decoders(streams), l, None, &mut out, threads, |_, b, chunk| {
        // (a stripe that failed gives nothing)
        if b.len() != chunk.len() {
            return Err(corrupt());
        }
        chunk.copy_from_slice(&b);
        Ok(())
    })?;
    if main.c.overrun() {
        return Err(corrupt());
    }
    Ok(out)
}

/// Bytes of [`aqr_api::stack`] that [`decode`] takes before the walk, of layout `l`: the
/// blocks, and the streams' slices (8 bytes each on WebAssembly).
pub fn decode_held<F: Family>(l: &Layout) -> u64 {
    use aqr_api::stack::block;
    block(l.len_in(F::shape(l.format))) + block(8 * apart_streams::<F>(l) as u64)
}

/// Whether the formats of group `g` are taken.
pub fn taken(g: Group) -> bool {
    matches!(g, Group::Bcn | Group::Bc7 | Group::Bc6h | Group::Etc | Group::Pvrtc | Group::Astc)
}

/// [`encode`] of the family of `l`'s format.
pub fn encode_any(l: &Layout, blocks: &[u8], threads: usize) -> (Vec<u8>, Vec<u8>, [u64; CATS]) {
    match l.format.group() {
        Group::Bcn => encode::<Bcn>(l, blocks, threads),
        Group::Bc7 => encode::<Bc7>(l, blocks, threads),
        Group::Bc6h => encode::<crate::bc6h::Bc6h>(l, blocks, threads),
        Group::Etc => encode::<crate::etcm::Etc>(l, blocks, threads),
        Group::Pvrtc => encode::<crate::pvrtc::Pvrtc>(l, blocks, threads),
        Group::Astc => encode::<crate::astcm::Astc>(l, blocks, threads),
    }
}

/// [`decode`] of the family of `l`'s format.
pub fn decode_any(l: &Layout, main: &[u8], apart: &[u8], threads: usize) -> Result<Vec<u8>> {
    match l.format.group() {
        Group::Bcn => decode::<Bcn>(l, main, apart, threads),
        Group::Bc7 => decode::<Bc7>(l, main, apart, threads),
        Group::Bc6h => decode::<crate::bc6h::Bc6h>(l, main, apart, threads),
        Group::Etc => decode::<crate::etcm::Etc>(l, main, apart, threads),
        Group::Pvrtc => decode::<crate::pvrtc::Pvrtc>(l, main, apart, threads),
        Group::Astc => decode::<crate::astcm::Astc>(l, main, apart, threads),
    }
}

/// [`stripes`] of a surface of `blocks` blocks of format `f`.
pub fn stripes_of(f: Format, blocks: usize) -> usize {
    match f.group() {
        Group::Pvrtc => 1,
        _ => stripes::<Bcn>(blocks),
    }
}
