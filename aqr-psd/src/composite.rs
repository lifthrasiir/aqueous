//! Approximate re-rendering of the composite image from the layers, row by row.
//!
//! This is a predictor, not a renderer: it only needs to be close to what Photoshop stored.
//! Supported: pixel layers with the separable and non-separable blend modes, opacity, fill
//! opacity, user masks, clipping and groups. Like Photoshop, every step leaves straight 8-bit
//! color and alpha; normal blending is integer arithmetic. Anything else (dissolve,
//! adjustment layers, layer effects) is rendered as normal, or skipped, and counted in
//! [`Report`].
//!
//! Every pixel only depends on the same pixel of the layers, so rows are rendered one at a
//! time: memory is a few rows (per group nesting level), whatever the document size.

use std::collections::BTreeMap;

use crate::psd::{Layer, Psd, Result};
use crate::rows::PlaneRows;

#[derive(Debug, Default)]
pub struct Report {
    /// Layers rendered with the normal blend mode in place of their own, by mode.
    pub blend_fallback: BTreeMap<String, usize>,
    /// Layers without pixels that are not group markers (adjustment and fill layers).
    pub skipped: usize,
    /// Layers with layer effects, rendered without them.
    pub effects: usize,
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    c.map(|v| {
        let mut v = v;
        if n < 0.0 {
            v = l + (v - l) * l / (l - n);
        }
        if x > 1.0 {
            v = l + (v - l) * (1.0 - l) / (x - l);
        }
        v
    })
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (mut lo, mut mid, mut hi) = (0, 1, 2);
    if c[lo] > c[mid] {
        std::mem::swap(&mut lo, &mut mid);
    }
    if c[mid] > c[hi] {
        std::mem::swap(&mut mid, &mut hi);
    }
    if c[lo] > c[mid] {
        std::mem::swap(&mut lo, &mut mid);
    }
    let mut out = [0.0; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    }
    out
}

/// A blend function B(backdrop, source) on straight colors.
type BlendFn = fn([f32; 3], [f32; 3]) -> [f32; 3];

#[inline(always)]
fn sep(b: [f32; 3], s: [f32; 3], f: impl Fn(f32, f32) -> f32) -> [f32; 3] {
    [f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])]
}

/// The blend function of a mode; None if unsupported, `Some(None)` for normal.
fn blend_fn(mode: &[u8; 4]) -> Option<Option<BlendFn>> {
    Some(Some(match mode {
        b"norm" | b"pass" => return Some(None),
        b"dark" => |b, s| sep(b, s, f32::min),
        b"lite" => |b, s| sep(b, s, f32::max),
        b"mul " => |b, s| sep(b, s, |b, s| b * s),
        b"scrn" => |b, s| sep(b, s, |b, s| b + s - b * s),
        b"over" => |b, s| sep(b, s, |b, s| if b <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        b"hLit" => |b, s| sep(b, s, |b, s| if s <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        b"sLit" => |b, s| {
            sep(b, s, |b, s| {
                if s <= 0.5 {
                    2.0 * b * s + b * b * (1.0 - 2.0 * s)
                } else {
                    2.0 * b * (1.0 - s) + b.sqrt() * (2.0 * s - 1.0)
                }
            })
        },
        b"div " => |b, s| {
            sep(b, s, |b, s| {
                if b == 0.0 {
                    0.0
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            })
        },
        b"idiv" => |b, s| {
            sep(b, s, |b, s| {
                if b >= 1.0 {
                    1.0
                } else if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - b) / s).min(1.0)
                }
            })
        },
        b"lddg" => |b, s| sep(b, s, |b, s| (b + s).min(1.0)),
        b"lbrn" => |b, s| sep(b, s, |b, s| (b + s - 1.0).max(0.0)),
        b"diff" => |b, s| sep(b, s, |b, s| (b - s).abs()),
        b"smud" => |b, s| sep(b, s, |b, s| b + s - 2.0 * b * s),
        b"fsub" => |b, s| sep(b, s, |b, s| (b - s).max(0.0)),
        b"fdiv" => |b, s| sep(b, s, |b, s| if s <= 0.0 { if b > 0.0 { 1.0 } else { 0.0 } } else { (b / s).min(1.0) }),
        b"lLit" => |b, s| sep(b, s, |b, s| (b + 2.0 * s - 1.0).clamp(0.0, 1.0)),
        b"vLit" => |b, s| {
            sep(b, s, |b, s| {
                if s <= 0.5 {
                    if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / (2.0 * s)).min(1.0) }
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (2.0 * (1.0 - s))).min(1.0)
                }
            })
        },
        b"pLit" => |b, s| sep(b, s, |b, s| if s <= 0.5 { b.min(2.0 * s) } else { b.max(2.0 * s - 1.0) }),
        b"hMix" => |b, s| sep(b, s, |b, s| if b + s >= 1.0 { 1.0 } else { 0.0 }),
        b"dkCl" => |b, s| if lum(s) < lum(b) { s } else { b },
        b"lgCl" => |b, s| if lum(s) > lum(b) { s } else { b },
        b"hue " => |b, s| set_lum(set_sat(s, sat(b)), lum(b)),
        b"sat " => |b, s| set_lum(set_sat(b, sat(s)), lum(b)),
        b"colr" => |b, s| set_lum(s, lum(b)),
        b"lum " => |b, s| set_lum(b, lum(s)),
        _ => return None,
    }))
}

/// Composites straight color `s` with coverage `a` (0..255) onto straight `dst` with blend
/// function `mode` (None: normal).
#[inline(always)]
fn composite_px(dst: &mut [u8; 4], s: [u8; 3], a: u32, mode: Option<BlendFn>) {
    if a == 0 {
        return;
    }
    let da = dst[3] as u32;
    if (mode.is_none() && a == 255) || da == 0 {
        *dst = [s[0], s[1], s[2], a as u8];
        return;
    }
    if let Some(f) = mode {
        return composite_mode(dst, s, a, f);
    }
    // Weights a and da (1 - a), both scaled by 255.
    let ws = a * 255;
    let wd = da * (255 - a);
    let tot = ws + wd;
    let oa = (tot + 127) / 255;
    if oa == 0 {
        *dst = [0; 4];
        return;
    }
    for k in 0..3 {
        dst[k] = ((s[k] as u32 * ws + dst[k] as u32 * wd + tot / 2) / tot) as u8;
    }
    dst[3] = oa as u8;
}

/// [`composite_px`] for the other blend modes, in floating point.
fn composite_mode(dst: &mut [u8; 4], s: [u8; 3], a: u32, f: BlendFn) {
    let ab = dst[3] as f32 / 255.0;
    let cb = [0, 1, 2].map(|k| dst[k] as f32 / 255.0);
    let sf = s.map(|v| v as f32 / 255.0);
    let m = f(cb, sf);
    let sm = [0, 1, 2].map(|k| (1.0 - ab) * sf[k] + ab * m[k]);
    let af = a as f32 / 255.0;
    let oa = af + ab * (1.0 - af);
    let oa8 = (oa * 255.0).round();
    if oa8 == 0.0 {
        *dst = [0; 4];
        return;
    }
    for k in 0..3 {
        let c = (sm[k] * af + cb[k] * ab * (1.0 - af)) / oa;
        dst[k] = (c * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = oa8 as u8;
}

fn section(l: &Layer, d: &[u8]) -> u32 {
    l.tag(b"lsct")
        .and_then(|t| d.get(t.data.start..t.data.start + 4))
        .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()))
}

fn has_effects(l: &Layer) -> bool {
    l.tag(b"lfx2").is_some() || l.tag(b"lmfx").is_some()
}

fn fill_opacity(l: &Layer, d: &[u8]) -> f32 {
    l.tag(b"iOpa").and_then(|t| d.get(t.data.start)).map_or(1.0, |&b| b as f32 / 255.0)
}

/// One step of rendering a row.
enum Op {
    /// Start an isolated group: draw on a new canvas.
    Push,
    /// End it: composite its canvas onto the one below.
    Pop { opacity: u32, mode: Option<BlendFn>, hidden: bool },
    /// Forget the clipping base.
    ClearClip,
    /// Draw a layer (or only take its coverage as clipping base, when hidden).
    Layer(usize),
}

/// What a pixel layer needs per row.
struct LayerRows<'a> {
    top: i64,
    left: i64,
    w: usize,
    h: usize,
    rgb: [PlaneRows<'a>; 3],
    alpha: Option<PlaneRows<'a>>,
    /// The user mask: rows, rectangle (top, left, width, height), default value.
    mask: Option<(PlaneRows<'a>, i64, i64, usize, usize, u8)>,
    clip: bool,
    /// Whether clipped layers follow (this one's coverage clips them).
    base: bool,
    hidden: bool,
    mode: Option<BlendFn>,
    /// Opacity times fill opacity, 0..255.
    opacity: u32,
}

/// A canvas row with the range drawn on.
struct Row {
    px: Vec<[u8; 4]>,
    x0: usize,
    x1: usize,
}

/// Renders the composite row by row.
pub struct RowRenderer<'a> {
    w: usize,
    h: usize,
    y: usize,
    ops: Vec<Op>,
    layers: Vec<Option<LayerRows<'a>>>,
    stack: Vec<Row>,
    /// Row buffers: color planes, alpha and mask of a layer.
    buf: [Vec<u8>; 5],
    clip: Vec<u8>,
    clip_range: (usize, usize),
}

impl<'a> RowRenderer<'a> {
    /// A renderer of the layers of `psd`, whose bytes (at least up to the composite image
    /// data) are `d`.
    pub fn new(psd: &'a Psd, d: &'a [u8], rep: &mut Report) -> Result<Self> {
        let (w, h) = (psd.width as usize, psd.height as usize);
        let layers = &psd.layer_info.layers;
        // Layers are stored bottom to top: a group's divider record (section type 3) first,
        // its folder record (type 1 or 2) last. Match them.
        let mut folder_of = vec![usize::MAX; layers.len()];
        let mut open = Vec::new();
        for (i, l) in layers.iter().enumerate() {
            match section(l, d) {
                3 => open.push(i),
                1 | 2 => {
                    if let Some(j) = open.pop() {
                        folder_of[j] = i;
                    }
                }
                _ => {}
            }
        }
        // Pass-through groups at full opacity are rendered straight into their parent; their
        // folder record then has nothing to do.
        let direct = |f: usize| {
            f != usize::MAX && !layers[f].hidden() && &layers[f].blend == b"pass" && layers[f].opacity == 255
        };
        let mut ops = Vec::new();
        let mut rows = Vec::with_capacity(layers.len());
        let mut skip_folder = vec![false; layers.len()];
        let mut depth = 0usize;
        for (i, l) in layers.iter().enumerate() {
            rows.push(None);
            match section(l, d) {
                3 => {
                    if direct(folder_of[i]) {
                        skip_folder[folder_of[i]] = true;
                    } else {
                        ops.push(Op::Push);
                        depth += 1;
                    }
                    ops.push(Op::ClearClip);
                    continue;
                }
                1 | 2 => {
                    ops.push(Op::ClearClip);
                    if skip_folder[i] || depth == 0 {
                        continue;
                    }
                    depth -= 1;
                    let mode = blend_fn(&l.blend);
                    if !l.hidden() && mode.is_none() {
                        *rep.blend_fallback.entry(String::from_utf8_lossy(&l.blend).into()).or_default() += 1;
                    }
                    ops.push(Op::Pop { opacity: l.opacity as u32, mode: mode.flatten(), hidden: l.hidden() });
                    continue;
                }
                _ => {}
            }
            let find = |id: i16| l.channels.iter().position(|c| c.id == id);
            let (Some(r), Some(g), Some(b)) = (find(0), find(1), find(2)) else {
                rep.skipped += 1;
                continue;
            };
            let (lw, lh) = (l.rect.width() as usize, l.rect.height() as usize);
            if lw == 0 || lh == 0 {
                continue;
            }
            let ch = |j: usize| PlaneRows::channel(psd, d, i, j);
            let mut mask = None;
            if let (Some(mk), Some(j)) = (&l.mask, find(-2))
                && mk.flags & 2 == 0
            {
                let c = &l.channels[j];
                mask = Some((
                    ch(j)?,
                    mk.rect.top as i64,
                    mk.rect.left as i64,
                    c.width as usize,
                    c.height as usize,
                    mk.default_color,
                ));
            }
            if !l.hidden() {
                if blend_fn(&l.blend).is_none() {
                    *rep.blend_fallback.entry(String::from_utf8_lossy(&l.blend).into()).or_default() += 1;
                }
                if has_effects(l) {
                    rep.effects += 1;
                }
            }
            let mode = blend_fn(&l.blend).flatten();
            rows[i] = Some(LayerRows {
                top: l.rect.top as i64,
                left: l.rect.left as i64,
                w: lw,
                h: lh,
                rgb: [ch(r)?, ch(g)?, ch(b)?],
                alpha: find(-1).map(ch).transpose()?,
                mask,
                clip: l.clipping != 0,
                base: false,
                hidden: l.hidden(),
                mode,
                opacity: (l.opacity as f32 * fill_opacity(l, d)).round() as u32,
            });
            ops.push(Op::Layer(i));
        }
        for _ in 0..depth {
            ops.push(Op::Pop { opacity: 255, mode: None, hidden: false });
        }
        // Clipping bases; layers clipped to a hidden base are hidden too, as in Photoshop.
        let mut base: Option<usize> = None;
        for op in &ops {
            match *op {
                Op::Layer(i) => {
                    let clip = rows[i].as_ref().unwrap().clip;
                    if clip {
                        if let Some(b) = base {
                            rows[b].as_mut().unwrap().base = true;
                            if rows[b].as_ref().unwrap().hidden {
                                rows[i].as_mut().unwrap().hidden = true;
                            }
                        }
                    } else {
                        base = Some(i);
                    }
                }
                _ => base = None,
            }
        }
        // Hidden layers that clip nothing have nothing to do.
        ops.retain(|op| match *op {
            Op::Layer(i) => {
                let l = rows[i].as_ref().unwrap();
                !l.hidden || l.base
            }
            _ => true,
        });
        let row = || Row { px: vec![[0; 4]; w], x0: w, x1: 0 };
        Ok(RowRenderer {
            w,
            h,
            y: 0,
            ops,
            layers: rows,
            stack: vec![row()],
            buf: Default::default(),
            clip: vec![0; w],
            clip_range: (0, 0),
        })
    }

    /// Skips the next row (cheaply: the layers' rows are skipped when the next one is drawn).
    pub fn skip_row(&mut self) {
        self.y += 1;
    }

    /// Renders the next row into planar `out` rows: R, G, B matted on white, and alpha if
    /// there are four.
    pub fn next_row(&mut self, out: &mut [&mut [u8]]) -> Result<()> {
        let (w, y) = (self.w, self.y);
        if y >= self.h {
            return Err(crate::psd::Error("past the last row".into()));
        }
        let mut level = 0;
        let base = &mut self.stack[0];
        base.px[base.x0.min(w)..base.x1.max(base.x0.min(w))].fill([0; 4]);
        (base.x0, base.x1) = (w, 0);
        self.clip_range = (0, 0);
        for op in &self.ops {
            match op {
                Op::ClearClip => self.clip_range = (0, 0),
                Op::Push => {
                    level += 1;
                    if self.stack.len() <= level {
                        self.stack.push(Row { px: vec![[0; 4]; w], x0: w, x1: 0 });
                    }
                    let r = &mut self.stack[level];
                    if r.x0 < r.x1 {
                        r.px[r.x0..r.x1].fill([0; 4]);
                    }
                    (r.x0, r.x1) = (w, 0);
                }
                &Op::Pop { opacity, mode, hidden } => {
                    let (lo, hi) = self.stack.split_at_mut(level);
                    let (dst, src) = (&mut lo[level - 1], &hi[0]);
                    level -= 1;
                    if hidden || src.x0 >= src.x1 {
                        continue;
                    }
                    for x in src.x0..src.x1 {
                        let s = src.px[x];
                        let a = (s[3] as u32 * opacity + 127) / 255;
                        composite_px(&mut dst.px[x], [s[0], s[1], s[2]], a, mode);
                    }
                    dst.x0 = dst.x0.min(src.x0);
                    dst.x1 = dst.x1.max(src.x1);
                }
                &Op::Layer(i) => {
                    let l = self.layers[i].as_mut().unwrap();
                    let ly = y as i64 - l.top;
                    if ly < 0 || ly >= l.h as i64 {
                        if !l.clip {
                            self.clip_range = (0, 0);
                        }
                        continue;
                    }
                    // Bring the cursors to this row (rows above the canvas are skipped).
                    let ly = ly as usize;
                    let [rb, gb, bb, ab, mb] = &mut self.buf;
                    let decode =
                        |p: &mut PlaneRows, out: &mut Vec<u8>, rows: usize, row: usize, n: usize| -> Result<()> {
                            while p.rows_left() > rows - row {
                                p.skip()?;
                            }
                            if out.len() < n {
                                out.resize(n, 0);
                            }
                            p.next(&mut out[..n])
                        };
                    // The layer's columns on the canvas (possibly none).
                    let x0 = l.left.clamp(0, w as i64) as usize;
                    let x1 = (l.left + l.w as i64).clamp(x0 as i64, w as i64) as usize;
                    let lx0 = (x0 as i64 - l.left) as usize;
                    let lx1 = (x1 as i64 - l.left) as usize;
                    // Coverage first (alpha), to skip the color rows where it is nil.
                    match &mut l.alpha {
                        Some(a) => decode(a, ab, l.h, ly, l.w)?,
                        None => {
                            ab.resize(ab.len().max(l.w), 0);
                            ab[..l.w].fill(255);
                        }
                    }
                    if l.clip {
                        let (c0, c1) = self.clip_range;
                        if c0.max(x0) >= c1.min(x1) {
                            ab[lx0..lx1].fill(0);
                        }
                    }
                    let mut seen = lx0 < lx1 && ab[lx0..lx1].iter().any(|&v| v != 0);
                    // Coverage: alpha times mask, into the alpha buffer (layer coordinates).
                    if let Some((m, mt, ml, mw, mh, def)) = &mut l.mask {
                        let my = y as i64 - *mt;
                        let inside = my >= 0 && my < *mh as i64;
                        if inside {
                            decode(m, mb, *mh, my as usize, *mw)?;
                        }
                        if seen {
                            // The mask's columns within the layer's, and the default outside.
                            let (m0, m1) = if inside {
                                let a = (*ml).clamp(x0 as i64, x1 as i64) as usize;
                                (a, (*ml + *mw as i64).clamp(a as i64, x1 as i64) as usize)
                            } else {
                                (x0, x0)
                            };
                            let mul = |a: &mut u8, v: u8| *a = ((*a as u32 * v as u32 + 127) / 255) as u8;
                            let d = *def;
                            if d != 255 {
                                for x in (x0..m0).chain(m1..x1) {
                                    mul(&mut ab[x + lx0 - x0], d);
                                }
                            }
                            if m0 < m1 {
                                let (la, ma) = (m0 + lx0 - x0, (m0 as i64 - *ml) as usize);
                                for (a, &v) in ab[la..la + (m1 - m0)].iter_mut().zip(&mb[ma..ma + (m1 - m0)]) {
                                    if v != 255 {
                                        mul(a, v);
                                    }
                                }
                            }
                        }
                    }
                    if l.clip {
                        if seen {
                            let (c0, c1) = self.clip_range;
                            for x in x0..x1 {
                                let lx = x + lx0 - x0;
                                let v = if x >= c0 && x < c1 { self.clip[x] as u32 } else { 0 };
                                ab[lx] = ((ab[lx] as u32 * v + 127) / 255) as u8;
                            }
                        }
                    } else if l.base {
                        if seen {
                            self.clip[x0..x1].copy_from_slice(&ab[lx0..lx1]);
                            self.clip_range = (x0, x1);
                        } else {
                            self.clip_range = (0, 0);
                        }
                    }
                    seen = seen && !l.hidden && ab[lx0..lx1].iter().any(|&v| v != 0);
                    let [pr, pg, pb] = &mut l.rgb;
                    if !seen {
                        // Nothing to draw on this row: the color rows are skipped, not decoded.
                        for p in [pr, pg, pb] {
                            while p.rows_left() > l.h - ly {
                                p.skip()?;
                            }
                        }
                        continue;
                    }
                    decode(pr, rb, l.h, ly, l.w)?;
                    decode(pg, gb, l.h, ly, l.w)?;
                    decode(pb, bb, l.h, ly, l.w)?;
                    let dst = &mut self.stack[level];
                    let op = l.opacity;
                    let mode = l.mode;
                    for x in x0..x1 {
                        let lx = x + lx0 - x0;
                        let a = ab[lx] as u32;
                        if a == 0 {
                            continue;
                        }
                        let a = if op == 255 { a } else { (a * op + 127) / 255 };
                        composite_px(&mut dst.px[x], [rb[lx], gb[lx], bb[lx]], a, mode);
                    }
                    dst.x0 = dst.x0.min(x0);
                    dst.x1 = dst.x1.max(x1);
                }
            }
        }
        // Colors are matted on white even when the alpha is stored.
        let base = &self.stack[0];
        for x in 0..w {
            let p = base.px[x];
            let a = p[3] as u32;
            for k in 0..3 {
                out[k][x] = match a {
                    255 => p[k],
                    0 => 255,
                    _ => ((p[k] as u32 * a + 255 * (255 - a) + 127) / 255) as u8,
                };
            }
            if out.len() > 3 {
                out[3][x] = p[3];
            }
        }
        self.y += 1;
        Ok(())
    }
}

/// Renders the layers of `psd` (whose bytes are `d`); returns interleaved 8-bit RGB matted on
/// white, plus alpha when `with_alpha`.
pub fn render(psd: &Psd, d: &[u8], with_alpha: bool, rep: &mut Report) -> Result<Vec<u8>> {
    let (w, h) = (psd.width as usize, psd.height as usize);
    let n = if with_alpha { 4 } else { 3 };
    let mut r = RowRenderer::new(psd, d, rep)?;
    let mut planes = vec![vec![0u8; w]; n];
    let mut out = Vec::with_capacity(w * h * n);
    for _ in 0..h {
        let mut refs: Vec<&mut [u8]> = planes.iter_mut().map(|p| p.as_mut_slice()).collect();
        r.next_row(&mut refs)?;
        for x in 0..w {
            for p in &planes {
                out.push(p[x]);
            }
        }
    }
    Ok(out)
}
