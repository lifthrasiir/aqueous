//! Approximate re-rendering of the composite image from the layers.
//!
//! This is a predictor, not a renderer: it only needs to be close to what Photoshop stored.
//! Supported: pixel layers with the separable and non-separable blend modes, opacity, fill
//! opacity, user masks, clipping and groups. Rounding to 8 bits after each layer follows
//! Photoshop closely for normal blending. Anything else (dissolve, adjustment layers, layer
//! effects) is rendered as normal, or skipped, and counted in [`Report`].

use std::collections::BTreeMap;

use crate::pixels;
use crate::psd::{Layer, Psd, Rect, Result};

#[derive(Debug, Default)]
pub struct Report {
    /// Layers rendered with the normal blend mode in place of their own, by mode.
    pub blend_fallback: BTreeMap<String, usize>,
    /// Layers without pixels that are not group markers (adjustment and fill layers).
    pub skipped: usize,
    /// Layers with layer effects, rendered without them.
    pub effects: usize,
}

/// Premultiplied RGBA canvas in 0..1, with the bounding box of what was drawn on it.
#[derive(Clone)]
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
    bbox: Option<Bounds>,
}

/// A pixel rectangle `[x0, x1) x [y0, y1)` within the canvas.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Bounds {
    /// `r` clipped to a `w` x `h` canvas; None if empty.
    fn of(r: &Rect, w: usize, h: usize) -> Option<Bounds> {
        let c = |v: i32, m: usize| (v.max(0) as usize).min(m);
        let b = Bounds { x0: c(r.left, w), y0: c(r.top, h), x1: c(r.right, w), y1: c(r.bottom, h) };
        (b.x0 < b.x1 && b.y0 < b.y1).then_some(b)
    }
    fn union(a: Option<Bounds>, b: Bounds) -> Bounds {
        match a {
            None => b,
            Some(a) => Bounds { x0: a.x0.min(b.x0), y0: a.y0.min(b.y0), x1: a.x1.max(b.x1), y1: a.y1.max(b.y1) },
        }
    }
}

/// Rounds a premultiplied pixel to straight 8-bit color and alpha, as Photoshop keeps them.
fn quantize(p: &mut [f32; 4]) {
    let a8 = (p[3] * 255.0).round();
    if a8 == 0.0 {
        *p = [0.0; 4];
        return;
    }
    let a = a8 / 255.0;
    for k in 0..3 {
        p[k] = (p[k] / p[3] * 255.0).round().clamp(0.0, 255.0) / 255.0 * a;
    }
    p[3] = a;
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Canvas { w, h, px: vec![[0.0; 4]; w * h], bbox: None }
    }
    fn over(&mut self, src: &Canvas, opacity: f32, mode: &[u8; 4]) {
        let Some(b) = src.bbox else { return };
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                let o = y * self.w + x;
                let s = src.px[o];
                if s[3] > 0.0 {
                    composite_px(&mut self.px[o], [s[0] / s[3], s[1] / s[3], s[2] / s[3]], s[3] * opacity, mode);
                }
            }
        }
        self.bbox = Some(Bounds::union(self.bbox, b));
    }
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

/// Blend function B(backdrop, source) of a mode, on straight colors; None if unsupported.
fn blend(mode: &[u8; 4], b: [f32; 3], s: [f32; 3]) -> Option<[f32; 3]> {
    let sep = |f: fn(f32, f32) -> f32| Some([f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])]);
    match mode {
        b"norm" | b"pass" => Some(s),
        b"dark" => sep(|b, s| b.min(s)),
        b"lite" => sep(|b, s| b.max(s)),
        b"mul " => sep(|b, s| b * s),
        b"scrn" => sep(|b, s| b + s - b * s),
        b"over" => sep(|b, s| if b <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        b"hLit" => sep(|b, s| if s <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        b"sLit" => sep(|b, s| {
            if s <= 0.5 { 2.0 * b * s + b * b * (1.0 - 2.0 * s) } else { 2.0 * b * (1.0 - s) + b.sqrt() * (2.0 * s - 1.0) }
        }),
        b"div " => sep(|b, s| if b == 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        b"idiv" => sep(|b, s| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        b"lddg" => sep(|b, s| (b + s).min(1.0)),
        b"lbrn" => sep(|b, s| (b + s - 1.0).max(0.0)),
        b"diff" => sep(|b, s| (b - s).abs()),
        b"smud" => sep(|b, s| b + s - 2.0 * b * s),
        b"fsub" => sep(|b, s| (b - s).max(0.0)),
        b"fdiv" => sep(|b, s| if s <= 0.0 { if b > 0.0 { 1.0 } else { 0.0 } } else { (b / s).min(1.0) }),
        b"lLit" => sep(|b, s| (b + 2.0 * s - 1.0).clamp(0.0, 1.0)),
        b"vLit" => sep(|b, s| {
            if s <= 0.5 {
                if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / (2.0 * s)).min(1.0) }
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (2.0 * (1.0 - s))).min(1.0)
            }
        }),
        b"pLit" => sep(|b, s| if s <= 0.5 { b.min(2.0 * s) } else { b.max(2.0 * s - 1.0) }),
        b"hMix" => sep(|b, s| if b + s >= 1.0 { 1.0 } else { 0.0 }),
        b"dkCl" => Some(if lum(s) < lum(b) { s } else { b }),
        b"lgCl" => Some(if lum(s) > lum(b) { s } else { b }),
        b"hue " => Some(set_lum(set_sat(s, sat(b)), lum(b))),
        b"sat " => Some(set_lum(set_sat(b, sat(s)), lum(b))),
        b"colr" => Some(set_lum(s, lum(b))),
        b"lum " => Some(set_lum(b, lum(s))),
        _ => None,
    }
}

/// Composites straight color `s` with coverage `a` onto premultiplied `dst` with `mode`.
fn composite_px(dst: &mut [f32; 4], s: [f32; 3], a: f32, mode: &[u8; 4]) {
    let ab = dst[3];
    let s = if ab > 0.0 && mode != b"norm" {
        let cb = [dst[0] / ab, dst[1] / ab, dst[2] / ab];
        let m = blend(mode, cb, s).unwrap_or(s);
        [0, 1, 2].map(|k| (1.0 - ab) * s[k] + ab * m[k])
    } else {
        s
    };
    for k in 0..3 {
        dst[k] = s[k] * a + dst[k] * (1.0 - a);
    }
    dst[3] = a + dst[3] * (1.0 - a);
    quantize(dst);
}

fn section(l: &Layer, d: &[u8]) -> u32 {
    l.tag(b"lsct")
        .and_then(|t| d.get(t.data.start..t.data.start + 4))
        .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()))
}

fn has_effects(l: &Layer) -> bool {
    l.tag(b"lfx2").is_some() || l.tag(b"lmfx").is_some()
}

/// Layer pixels within their bounds on the canvas: straight color and coverage (alpha times
/// mask), row-major over the bounds.
struct LayerPixels {
    b: Bounds,
    rgb: [Vec<u8>; 3],
    alpha: Vec<f32>,
}

impl LayerPixels {
    fn at(&self, x: usize, y: usize) -> Option<usize> {
        let b = &self.b;
        (x >= b.x0 && x < b.x1 && y >= b.y0 && y < b.y1).then(|| (y - b.y0) * (b.x1 - b.x0) + x - b.x0)
    }
}

/// Channel `j` of layer `i`, decoded.
pub type Channels<'a> = dyn Fn(usize, usize) -> Result<Vec<u8>> + 'a;

fn layer_pixels(psd: &Psd, chan: &Channels, i: usize) -> Result<Option<LayerPixels>> {
    let l = &psd.layer_info.layers[i];
    let (cw, ch) = (psd.width as usize, psd.height as usize);
    let find = |id: i16| l.channels.iter().position(|c| c.id == id);
    let (Some(r), Some(g), Some(b)) = (find(0), find(1), find(2)) else { return Ok(None) };
    let Some(bb) = Bounds::of(&l.rect, cw, ch) else {
        return Ok(Some(LayerPixels { b: Bounds { x0: 0, y0: 0, x1: 0, y1: 0 }, rgb: Default::default(), alpha: Vec::new() }));
    };
    let lw = l.rect.width() as usize;
    let (bw, bh) = (bb.x1 - bb.x0, bb.y1 - bb.y0);
    let planes = [chan(i, r)?, chan(i, g)?, chan(i, b)?];
    let a = find(-1).map(|j| chan(i, j)).transpose()?;
    let mut rgb = [vec![0u8; bw * bh], vec![0u8; bw * bh], vec![0u8; bw * bh]];
    let mut alpha = vec![1f32; bw * bh];
    let (ox, oy) = ((bb.x0 as i64 - l.rect.left as i64) as usize, (bb.y0 as i64 - l.rect.top as i64) as usize);
    for y in 0..bh {
        let src = (oy + y) * lw + ox;
        for k in 0..3 {
            rgb[k][y * bw..(y + 1) * bw].copy_from_slice(&planes[k][src..src + bw]);
        }
        if let Some(a) = &a {
            for x in 0..bw {
                alpha[y * bw + x] = a[src + x] as f32 / 255.0;
            }
        }
    }
    // User mask, unless disabled; outside its rectangle, its default color.
    if let (Some(mk), Some(j)) = (&l.mask, find(-2)) {
        if mk.flags & 2 == 0 {
            let c = &l.channels[j];
            let m = chan(i, j)?;
            let def = mk.default_color as f32 / 255.0;
            let (mw, mh) = (c.width as i64, c.height as i64);
            for y in 0..bh {
                for x in 0..bw {
                    let mx = (bb.x0 + x) as i64 - mk.rect.left as i64;
                    let my = (bb.y0 + y) as i64 - mk.rect.top as i64;
                    let v = if mx >= 0 && my >= 0 && mx < mw && my < mh { m[(my * mw + mx) as usize] as f32 / 255.0 } else { def };
                    alpha[y * bw + x] *= v;
                }
            }
        }
    }
    Ok(Some(LayerPixels { b: bb, rgb, alpha }))
}

fn fill_opacity(l: &Layer, d: &[u8]) -> f32 {
    l.tag(b"iOpa").and_then(|t| d.get(t.data.start)).map_or(1.0, |&b| b as f32 / 255.0)
}

/// Renders the layers of `psd` (whose bytes are `d`); returns interleaved 8-bit RGB matted on
/// white, plus alpha when `with_alpha`.
pub fn render(psd: &Psd, d: &[u8], with_alpha: bool, rep: &mut Report) -> Result<Vec<u8>> {
    render_with(psd, d, &|i, j| pixels::layer_channel(psd, d, i, j), with_alpha, rep)
}

/// As [`render`], with the layers' channels from `chan`.
pub fn render_with(psd: &Psd, d: &[u8], chan: &Channels, with_alpha: bool, rep: &mut Report) -> Result<Vec<u8>> {
    let (w, h) = (psd.width as usize, psd.height as usize);
    let layers = &psd.layer_info.layers;
    // Layers are stored bottom to top: a group's divider record (section type 3) first, its
    // folder record (type 1 or 2) last. Match them.
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
    let direct = |f: usize| f != usize::MAX && !layers[f].hidden() && &layers[f].blend == b"pass" && layers[f].opacity == 255;
    let mut stack: Vec<Canvas> = vec![Canvas::new(w, h)];
    let mut skip_folder = vec![false; layers.len()];
    let mut clip_base: Option<LayerPixels> = None;
    for (i, l) in layers.iter().enumerate() {
        match section(l, d) {
            3 => {
                if direct(folder_of[i]) {
                    skip_folder[folder_of[i]] = true;
                } else {
                    stack.push(Canvas::new(w, h));
                }
                clip_base = None;
                continue;
            }
            1 | 2 => {
                clip_base = None;
                if skip_folder[i] || stack.len() == 1 {
                    continue;
                }
                let group = stack.pop().unwrap();
                if !l.hidden() {
                    let mode = if &l.blend == b"pass" { b"norm" } else { &l.blend };
                    if blend(mode, [0.0; 3], [0.0; 3]).is_none() {
                        *rep.blend_fallback.entry(String::from_utf8_lossy(&l.blend).into()).or_default() += 1;
                    }
                    stack.last_mut().unwrap().over(&group, l.opacity as f32 / 255.0, mode);
                }
                continue;
            }
            _ => {}
        }
        let Some(px) = layer_pixels(psd, chan, i)? else {
            rep.skipped += 1;
            continue;
        };
        let mut alpha = px.alpha;
        if l.clipping != 0 {
            if let Some(base) = &clip_base {
                let bw = px.b.x1 - px.b.x0;
                for y in px.b.y0..px.b.y1 {
                    for x in px.b.x0..px.b.x1 {
                        alpha[(y - px.b.y0) * bw + x - px.b.x0] *= base.at(x, y).map_or(0.0, |q| base.alpha[q]);
                    }
                }
            }
        } else {
            clip_base = Some(LayerPixels { b: px.b, rgb: Default::default(), alpha: alpha.clone() });
        }
        if l.hidden() || px.b.x0 == px.b.x1 {
            continue;
        }
        if blend(&l.blend, [0.0; 3], [0.0; 3]).is_none() {
            *rep.blend_fallback.entry(String::from_utf8_lossy(&l.blend).into()).or_default() += 1;
        }
        if has_effects(l) {
            rep.effects += 1;
        }
        let op = l.opacity as f32 / 255.0 * fill_opacity(l, d);
        let top = stack.last_mut().unwrap();
        let bw = px.b.x1 - px.b.x0;
        for y in px.b.y0..px.b.y1 {
            for x in px.b.x0..px.b.x1 {
                let q = (y - px.b.y0) * bw + x - px.b.x0;
                let a = alpha[q] * op;
                if a == 0.0 {
                    continue;
                }
                let s = [0, 1, 2].map(|k| px.rgb[k][q] as f32 / 255.0);
                composite_px(&mut top.px[y * w + x], s, a, &l.blend);
            }
        }
        top.bbox = Some(Bounds::union(top.bbox, px.b));
    }
    while stack.len() > 1 {
        let g = stack.pop().unwrap();
        stack.last_mut().unwrap().over(&g, 1.0, b"norm");
    }
    let c = &stack[0];
    let n = if with_alpha { 4 } else { 3 };
    let mut out = Vec::with_capacity(c.w * c.h * n);
    let q = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    // Colors are matted on white even when the alpha is stored.
    for p in &c.px {
        for k in 0..3 {
            out.push(q(p[k] + (1.0 - p[3])));
        }
        if with_alpha {
            out.push(q(p[3]));
        }
    }
    Ok(out)
}
