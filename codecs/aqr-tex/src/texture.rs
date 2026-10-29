//! A texture's blocks coded (FORMAT.md, "The walk"): layer by layer, each layer's mipmaps from
//! the smallest, each surface by its parts in turn ([`crate::model`]). A large surface is coded
//! in stripes ([`stripes`]): the first goes on with the models of the texture and its stream,
//! each other one starts from a copy of them as they are before the surface, with a stream of
//! its own, and is decoded apart from the rest (on another thread, or as a task of the Wasm
//! module).

use aqr_common::arith::ArithDecoder;
use aqr_common::packed;

use crate::coder::{CATS, Dec, Enc, Side};
use crate::layout::{Format, Layout};
use crate::model::{Alpha, Colour, Plane, Recent, Value};
use crate::{Result, corrupt};

/// A surface is coded in a stripe per this many blocks of it, as rows of blocks, and in at most
/// [`MAX_STRIPES`] stripes: its stripes but the first learn their models anew from the copy, a
/// cost of about 0.3% (README).
pub const STRIPE_BLOCKS: usize = 16384;
pub const MAX_STRIPES: usize = 16;

/// The stripes of a surface of `blocks` blocks.
pub fn stripes(blocks: usize) -> usize {
    (blocks / STRIPE_BLOCKS).clamp(1, MAX_STRIPES)
}

/// The rows of blocks of stripe `k` of `n` of a surface of `bh` rows (none empty, as a surface
/// of n stripes has at least n rows: no row is of more than 16384 blocks).
pub fn rows(k: usize, n: usize, bh: usize) -> std::ops::Range<usize> {
    k * bh / n..(k + 1) * bh / n
}

/// The models of a texture, or of a stripe.
#[derive(Clone)]
struct Models {
    colour: Colour,
    v0: Value,
    v1: Value,
    alpha: Alpha,
    /// The cache of each part with endpoints: the colour, or the first value, and the second.
    r0: Recent,
    r1: Recent,
}

impl Models {
    fn new(f: Format) -> Models {
        Models {
            colour: Colour::new(f != Format::Bc1),
            v0: Value::new(),
            v1: Value::new(),
            alpha: Alpha::new(),
            r0: Recent::new(),
            r1: Recent::new(),
        }
    }
}

/// The planes of a surface's parts: of colours, and of the first and second values (or alpha).
#[derive(Default)]
struct Planes {
    c: Option<Plane<3>>,
    a: Option<Plane<1>>,
    b: Option<Plane<1>>,
}

impl Planes {
    /// The planes of the blocks `b` of `size` blocks.
    fn of(f: Format, b: &[u8], size: (usize, usize)) -> Planes {
        match f {
            Format::Bc1 => Planes { c: Some(Colour::plane(false, b, 8, size)), ..Default::default() },
            Format::Bc2 => {
                Planes { c: Some(Colour::plane(true, &b[8..], 16, size)), a: Some(Alpha::plane(b, 16, size)), b: None }
            }
            Format::Bc3 => {
                Planes { c: Some(Colour::plane(true, &b[8..], 16, size)), a: Some(Value::plane(b, 16, size)), b: None }
            }
            Format::Bc4 => Planes { a: Some(Value::plane(b, 8, size)), ..Default::default() },
            Format::Bc5 => {
                Planes { a: Some(Value::plane(b, 16, size)), b: Some(Value::plane(&b[8..], 16, size)), c: None }
            }
        }
    }

    /// The planes of the stripes of a surface, one above another.
    fn stack(stripes: Vec<Planes>) -> Planes {
        let (mut c, mut a, mut b) = (vec![], vec![], vec![]);
        for p in stripes {
            c.extend(p.c);
            a.extend(p.a);
            b.extend(p.b);
        }
        Planes {
            c: (!c.is_empty()).then(|| Plane::stack(c)),
            a: (!a.is_empty()).then(|| Plane::stack(a)),
            b: (!b.is_empty()).then(|| Plane::stack(b)),
        }
    }

    /// These planes, of the next smaller mipmap, upsampled to the rows of blocks `rows` of a
    /// surface `bw` blocks wide.
    fn upsampled(&self, bw: usize, rows: std::ops::Range<usize>) -> Planes {
        Planes {
            c: self.c.as_ref().map(|p| p.upsampled(bw, rows.clone())),
            a: self.a.as_ref().map(|p| p.upsampled(bw, rows.clone())),
            b: self.b.as_ref().map(|p| p.upsampled(bw, rows)),
        }
    }
}

/// Codes the blocks `b` of a stripe of `size` blocks of a surface, `up` being the planes of the
/// surface before, upsampled to the stripe.
fn stripe(
    s: &mut impl Side,
    m: &mut Models,
    f: Format,
    b: &mut [u8],
    size: (usize, usize),
    up: &Planes,
) -> Result<Planes> {
    let (c, a, pb) = (up.c.as_ref(), up.a.as_ref(), up.b.as_ref());
    Ok(match f {
        Format::Bc1 => Planes { c: Some(m.colour.surface(s, b, 8, size, c, &mut m.r0)?), ..Default::default() },
        Format::Bc2 => {
            let a = m.alpha.surface(s, b, 16, size, a);
            Planes { c: Some(m.colour.surface(s, &mut b[8..], 16, size, c, &mut m.r0)?), a: Some(a), b: None }
        }
        Format::Bc3 => {
            let a = m.v0.surface(s, b, 16, size, a, &mut m.r1)?;
            Planes { c: Some(m.colour.surface(s, &mut b[8..], 16, size, c, &mut m.r0)?), a: Some(a), b: None }
        }
        Format::Bc4 => Planes { a: Some(m.v0.surface(s, b, 8, size, a, &mut m.r0)?), ..Default::default() },
        Format::Bc5 => {
            let a = m.v0.surface(s, b, 16, size, a, &mut m.r0)?;
            Planes { a: Some(a), b: Some(m.v1.surface(s, &mut b[8..], 16, size, pb, &mut m.r1)?), c: None }
        }
    })
}

/// `f(i)` for each `i` of `0..n`, apart from the calling thread (on up to `threads − 1` workers
/// of the pool, or as tasks of the Wasm module, which see the memory as it is when they start),
/// as `here` runs on the calling thread; or all in turn, `here` first, where there are no
/// threads to take.
fn apart<T>(
    n: usize,
    threads: usize,
    f: impl Fn(usize) -> Vec<u8> + Sync,
    here: impl FnOnce() -> T,
) -> (Vec<Vec<u8>>, T) {
    #[cfg(target_family = "wasm")]
    if threads > 1 {
        let g = |i: usize, out: &mut Vec<u8>| *out = f(i);
        let mut t = aqr_api::task::spawn(n, &g);
        let mine = here();
        return ((0..n).map(|i| t.join(i)).collect(), mine);
    }
    #[cfg(not(target_family = "wasm"))]
    if threads > 1 {
        use aqr_common::pool;
        return pool::scope(threads - 1, |s| {
            if s.workers() == 0 {
                let mine = here();
                return ((0..n).map(&f).collect(), mine);
            }
            let f = &f;
            // (as many spawned as there are workers before `here`, as a spawn waits for one to
            // be idle; the rest after it)
            let mut hs: Vec<_> = (0..n.min(s.workers())).map(|i| s.spawn(move || f(i))).collect();
            let mine = here();
            hs.extend((hs.len()..n).map(|i| s.spawn(move || f(i))));
            let out = hs.into_iter().map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))).collect();
            (out, mine)
        });
    }
    let mine = here();
    ((0..n).map(f).collect(), mine)
}

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
    fn done(&self, s: Self::S, b: &[u8]) -> Vec<u8>;
}

/// Walks the blocks of `l` (`blocks` when encoding, which takes every choice from them; zeros
/// filled in when decoding): the first stripes by `main`, the others by `sides`, `back` taking
/// what comes back of each into its rows of the surface; `done` takes each surface's blocks, in
/// the order of the walk. A surface's blocks are made after its stripes coded apart are started
/// (so that, as tasks, they do not copy them).
fn walk<X: Sides>(
    main: &mut X::S,
    sides: &X,
    l: &Layout,
    blocks: Option<&[u8]>,
    threads: usize,
    mut back: impl FnMut(usize, Vec<u8>, &mut [u8]) -> Result<()>,
    mut done: impl FnMut(Vec<u8>),
) -> Result<()> {
    let (f, bb) = (l.format, l.format.block_bytes());
    let mut models = Models::new(f);
    let surfaces: Vec<_> = l.mip_surfaces().collect();
    let layer: usize = surfaces.iter().map(|s| s.blocks() * bb).sum();
    // (stripes as tasks have a copy of the memory of their own, the models' copy theirs to change)
    let own = cfg!(target_family = "wasm") && threads > 1;
    let mut next = 1;
    for k in 0..l.layers as usize {
        let mut ends = Vec::with_capacity(surfaces.len());
        let mut at = k * layer;
        for sf in &surfaces {
            at += sf.blocks() * bb;
            ends.push(at);
        }
        let mut prev = Planes::default();
        for (m, (sf, &end)) in surfaces.iter().zip(&ends).enumerate().rev() {
            let (bw, bh) = (sf.bw, sf.bh);
            let size = sf.blocks() * bb;
            let src = blocks.map(|b| &b[end - size..end]);
            let make = |r: std::ops::Range<usize>| match src {
                Some(b) => b[r].to_vec(),
                None => vec![0; r.len()],
            };
            let n = stripes(sf.blocks());
            if n == 1 {
                let mut b = make(0..size);
                prev = stripe(main, &mut models, f, &mut b, (bw, bh), &prev.upsampled(bw, 0..bh))?;
                done(b);
                continue;
            }
            let rows0 = rows(0, n, bh).end;
            let top = rows0 * bw * bb;
            let (copy, coarse) = (std::sync::Mutex::new(models.clone()), &prev);
            let job = |i: usize| -> Vec<u8> {
                let r = rows(i + 1, n, bh);
                let at = r.start * bw * bb;
                let mut b = make(at..at + r.len() * bw * bb);
                let (mut g, mut mine);
                let m: &mut Models = if own {
                    g = copy.lock().unwrap();
                    &mut g
                } else {
                    mine = copy.lock().unwrap().clone();
                    &mut mine
                };
                let Ok(mut s) = sides.side(next + i) else { return vec![] };
                match stripe(&mut s, m, f, &mut b, (bw, r.len()), &coarse.upsampled(bw, r)) {
                    Ok(_) => sides.done(s, &b),
                    Err(_) => vec![],
                }
            };
            let (main, models) = (&mut *main, &mut models);
            let here = || {
                let mut b = make(0..size);
                let p = stripe(main, models, f, &mut b[..top], (bw, rows0), &coarse.upsampled(bw, 0..rows0));
                (b, p)
            };
            let (outs, (mut b, mine)) = apart(n - 1, threads, job, here);
            drop(copy);
            let mut planes = vec![mine?];
            let mut at = top;
            for (i, out) in outs.into_iter().enumerate() {
                let r = rows(i + 1, n, bh);
                let chunk = &mut b[at..at + r.len() * bw * bb];
                back(next + i, out, chunk)?;
                // (the largest mipmap's planes predict nothing)
                if m > 0 {
                    planes.push(Planes::of(f, chunk, (bw, r.len())));
                }
                at += chunk.len();
            }
            next += n - 1;
            prev = Planes::stack(planes);
            done(b);
        }
    }
    Ok(())
}

/// The streams of the stripes of `l` coded apart.
fn apart_streams(l: &Layout) -> usize {
    l.mip_surfaces().map(|s| stripes(s.blocks()) - 1).sum::<usize>() * l.layers as usize
}

struct Encoders;

impl Sides for Encoders {
    type S = Enc;

    fn side(&self, _: usize) -> Result<Enc> {
        Ok(Enc::new())
    }

    fn done(&self, e: Enc, _: &[u8]) -> Vec<u8> {
        let mut out = e.c.finish();
        out.extend(e.cost.iter().flat_map(|c| c.to_le_bytes()));
        out
    }
}

struct Decoders<'a>(Vec<&'a [u8]>);

impl<'a> Sides for Decoders<'a> {
    type S = Dec<'a>;

    fn side(&self, j: usize) -> Result<Dec<'a>> {
        Ok(Dec { c: ArithDecoder::new(self.0.get(j).ok_or_else(corrupt)?) })
    }

    fn done(&self, d: Dec<'a>, b: &[u8]) -> Vec<u8> {
        if d.c.overrun() { vec![] } else { b.to_vec() }
    }
}

/// The coded form of `blocks`, the blocks of layout `l` (exactly `l.len()` bytes): the first
/// stream, and the streams of the stripes coded apart (FORMAT.md, "The walk") as a packed byte
/// sequence, or nothing where there are none; and the bits each part took (in 1/2^16 bits). It
/// is the same whatever `threads` is.
///
/// (The first stream is kept apart from the others, as it is: the streams of textures alike
/// start alike, which the archive finds where they are not behind headers of other lengths.)
pub fn encode(l: &Layout, blocks: &[u8], threads: usize) -> (Vec<u8>, Vec<u8>, [u64; CATS]) {
    assert_eq!(blocks.len() as u64, l.len());
    let mut main = Enc::new();
    let mut streams = vec![vec![]; apart_streams(l) + 1];
    let mut cost = [0u64; CATS];
    walk(
        &mut main,
        &Encoders,
        l,
        Some(blocks),
        threads,
        |j, mut out, _| {
            let tally = out.split_off(out.len() - TALLY);
            for (i, c) in cost.iter_mut().enumerate() {
                *c += u64::from_le_bytes(tally[8 * i..8 * i + 8].try_into().unwrap());
            }
            streams[j] = out;
            Ok(())
        },
        drop,
    )
    .expect("the encoder codes any blocks");
    for (c, x) in cost.iter_mut().zip(main.cost) {
        *c += x;
    }
    let apart = if streams.len() > 1 { packed::pack(&streams[1..]) } else { vec![] };
    (main.c.finish(), apart, cost)
}

/// The blocks of layout `l` from its first stream `main` and the streams coded apart `apart`
/// (as [`encode`] gives them), decoded on up to `threads` threads.
pub fn decode(l: &Layout, main: &[u8], apart: &[u8], threads: usize) -> Result<Vec<u8>> {
    let len = usize::try_from(l.len()).map_err(|_| corrupt())?;
    let n = apart_streams(l);
    let mut streams = vec![main];
    if n > 0 {
        streams.extend(packed::unpack(apart).ok_or_else(corrupt)?);
    } else if !apart.is_empty() {
        return Err(corrupt());
    }
    if streams.len() != n + 1 {
        return Err(corrupt());
    }
    let mut main = Dec { c: ArithDecoder::new(streams[0]) };
    let sides = Decoders(streams);
    // (each layer's surfaces from the smallest)
    let mut surfaces = Vec::new();
    walk(
        &mut main,
        &sides,
        l,
        None,
        threads,
        |_, out, chunk| {
            // (a stripe that failed gives nothing)
            if out.len() != chunk.len() {
                return Err(corrupt());
            }
            chunk.copy_from_slice(&out);
            Ok(())
        },
        |b| surfaces.push(b),
    )?;
    if main.c.overrun() {
        return Err(corrupt());
    }
    let mut b = Vec::with_capacity(len);
    let per = l.mip_surfaces().count();
    let mut surfaces = surfaces.into_iter();
    for _ in 0..l.layers {
        let layer: Vec<_> = surfaces.by_ref().take(per).collect();
        for s in layer.into_iter().rev() {
            b.extend_from_slice(&s);
        }
    }
    Ok(b)
}
