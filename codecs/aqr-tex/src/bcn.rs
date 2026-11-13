//! BC1 to BC5 as a [`Family`] of the walk ([`crate::texture`]): the parts of their blocks
//! ([`crate::model`]'s colours, values and BC2's alpha), their planes, their models.

use std::mem::take;

use crate::Result;
use crate::coder::Side;
use crate::layout::{Format, Group, Shape};
use crate::model::{Alpha, Colour, Plane, Recent, Value, Work};
use crate::texture::Family;

pub struct Bcn;

/// The models of a texture, or of a stripe.
#[derive(Clone)]
pub struct Models {
    colour: Colour,
    v0: Value,
    v1: Value,
    alpha: Alpha,
    /// The cache of each part with endpoints: the colour, or the first value, and the second.
    r0: Recent,
    r1: Recent,
}

/// The planes a format's parts have: of colours, of the first values (or alpha), of the second.
fn kinds(f: Format) -> [bool; 3] {
    match f {
        Format::Bc1 => [true, false, false],
        Format::Bc2 | Format::Bc3 => [true, true, false],
        Format::Bc4 => [false, true, false],
        Format::Bc5 => [false, true, true],
        _ => unreachable!("not of BC1 to BC5"),
    }
}

/// The planes of a surface's parts: of colours, and of the first and second values (or alpha).
#[derive(Default)]
pub struct Planes {
    c: Option<Plane<3>>,
    a: Option<Plane<1>>,
    b: Option<Plane<1>>,
}

/// Buffers of a surface's planes (of those of its format), kept to be used again.
#[derive(Default)]
pub struct Bufs {
    c: Vec<[u8; 3]>,
    a: Vec<[u8; 1]>,
    b: Vec<[u8; 1]>,
}

impl Family for Bcn {
    const GROUP: Group = Group::Bcn;

    #[inline(always)]
    fn shape(f: Format) -> Shape {
        let bytes = if matches!(f, Format::Bc1 | Format::Bc4) { 8 } else { 16 };
        Shape { fw: 4, fh: 4, least: 1, bytes }
    }

    type Models = Models;
    type Planes = Planes;
    type Bufs = Bufs;
    type Work = Work;

    fn models(f: Format) -> Models {
        Models {
            colour: Colour::new(f != Format::Bc1),
            v0: Value::new(),
            v1: Value::new(),
            alpha: Alpha::new(),
            r0: Recent::new(),
            r1: Recent::new(),
        }
    }

    fn models_plan(_: Format) -> u64 {
        Colour::plan() + 2 * Value::plan() + Alpha::plan() + 2 * Recent::plan()
    }

    fn copy_models(m: &mut Models, o: &Models) {
        m.colour.copy_from(&o.colour);
        m.v0.copy_from(&o.v0);
        m.v1.copy_from(&o.v1);
        m.alpha.copy_from(&o.alpha);
        m.r0.copy_from(&o.r0);
        m.r1.copy_from(&o.r1);
    }

    fn bufs(f: Format, px: usize) -> Bufs {
        let [c, a, b] = kinds(f).map(|k| if k { px } else { 0 });
        Bufs { c: Vec::with_capacity(c), a: Vec::with_capacity(a), b: Vec::with_capacity(b) }
    }

    fn bufs_plan(f: Format, px: usize) -> u64 {
        let [c, a, b] = kinds(f);
        let px = px as u64;
        let (c, a, b) = (c as u64 * 3 * px, a as u64 * px, b as u64 * px);
        aqr_api::stack::block(c) + aqr_api::stack::block(a) + aqr_api::stack::block(b)
    }

    fn give(bufs: &mut Bufs, p: Planes) {
        if let Some(c) = p.c {
            bufs.c = c.into_px();
        }
        if let Some(a) = p.a {
            bufs.a = a.into_px();
        }
        if let Some(b) = p.b {
            bufs.b = b.into_px();
        }
    }

    fn is_empty(p: &Planes) -> bool {
        p.c.is_none() && p.a.is_none() && p.b.is_none()
    }

    fn work(_: Format, blocks: usize, cols: usize, across: usize) -> Work {
        Work::with_capacity(blocks, cols, across)
    }

    fn work_plan(_: Format, blocks: usize, cols: usize, across: usize) -> u64 {
        Work::plan(blocks, cols, across)
    }

    fn across_values(f: Format) -> usize {
        if kinds(f)[0] { 3 } else { 1 }
    }

    fn planes_of(f: Format, b: &[u8], size: (usize, usize), p: &mut Planes) {
        let (c, a, pb) = (p.c.as_mut(), p.a.as_mut(), p.b.as_mut());
        match f {
            Format::Bc1 => Colour::plane(false, b, 8, size, c.expect("colours")),
            Format::Bc2 => {
                Colour::plane(true, &b[8..], 16, size, c.expect("colours"));
                Alpha::plane(b, 16, size, a.expect("alpha"));
            }
            Format::Bc3 => {
                Colour::plane(true, &b[8..], 16, size, c.expect("colours"));
                Value::plane(b, 16, size, a.expect("values"));
            }
            Format::Bc4 => Value::plane(b, 8, size, a.expect("values")),
            Format::Bc5 => {
                Value::plane(b, 16, size, a.expect("values"));
                Value::plane(&b[8..], 16, size, pb.expect("values"));
            }
            _ => unreachable!("not of BC1 to BC5"),
        }
    }

    fn upsampled(
        p: &Planes,
        _: Format,
        w: usize,
        rows: std::ops::Range<usize>,
        bufs: &mut Bufs,
        work: &mut Work,
    ) -> Planes {
        Planes {
            c: p.c.as_ref().map(|p| p.upsampled::<4>(w, rows.clone(), take(&mut bufs.c), &mut work.up)),
            a: p.a.as_ref().map(|p| p.upsampled::<4>(w, rows.clone(), take(&mut bufs.a), &mut work.up)),
            b: p.b.as_ref().map(|p| p.upsampled::<4>(w, rows, take(&mut bufs.b), &mut work.up)),
        }
    }

    fn stripe(
        s: &mut impl Side,
        m: &mut Models,
        f: Format,
        b: &mut [u8],
        size: (usize, usize),
        up: &Planes,
        bufs: &mut Bufs,
        work: &mut Work,
    ) -> Result<Planes> {
        let (c, a, pb) = (up.c.as_ref(), up.a.as_ref(), up.b.as_ref());
        Ok(match f {
            Format::Bc1 => Planes {
                c: Some(m.colour.surface(s, b, 8, size, c, &mut m.r0, (take(&mut bufs.c), work))?),
                ..Default::default()
            },
            Format::Bc2 => {
                let a = m.alpha.surface(s, b, 16, size, a, take(&mut bufs.a));
                let c = m.colour.surface(s, &mut b[8..], 16, size, c, &mut m.r0, (take(&mut bufs.c), work))?;
                Planes { c: Some(c), a: Some(a), b: None }
            }
            Format::Bc3 => {
                let a = m.v0.surface(s, b, 16, size, a, &mut m.r1, (take(&mut bufs.a), &mut *work))?;
                let c = m.colour.surface(s, &mut b[8..], 16, size, c, &mut m.r0, (take(&mut bufs.c), work))?;
                Planes { c: Some(c), a: Some(a), b: None }
            }
            Format::Bc4 => Planes {
                a: Some(m.v0.surface(s, b, 8, size, a, &mut m.r0, (take(&mut bufs.a), work))?),
                ..Default::default()
            },
            Format::Bc5 => {
                let a = m.v0.surface(s, b, 16, size, a, &mut m.r0, (take(&mut bufs.a), &mut *work))?;
                let pb = m.v1.surface(s, &mut b[8..], 16, size, pb, &mut m.r1, (take(&mut bufs.b), work))?;
                Planes { a: Some(a), b: Some(pb), c: None }
            }
            _ => unreachable!("not of BC1 to BC5"),
        })
    }
}
