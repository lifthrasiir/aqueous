//! Blend modes of the renderer (`composite::render`): separable modes against their definitions
//! (Photoshop's and the W3C's compositing spec), non-separable modes by their properties,
//! partial opacity and backdrop, and modes the renderer does not know.

mod common;

use aqr_psd::composite::{Report, render};
use aqr_psd::packbits::NO_TILES;
use common::*;

const W: usize = 3;
const H: usize = 2;

/// A layer of the whole canvas of one color, and of one alpha (where given).
fn flat(color: [u8; 3], alpha: Option<u8>) -> Layer {
    let mut channels: Vec<(i16, Vec<u8>)> = (0..3).map(|k| (k as i16, vec![color[k]; W * H])).collect();
    if let Some(a) = alpha {
        channels.insert(0, (-1, vec![a; W * H]));
    }
    Layer { top: 0, left: 0, w: W, h: H, channels, mask: None }
}

/// The file of `backdrop` and over it `source` blended by `mode` at `opacity`, rendered; the
/// first pixel's RGB (all pixels are the same) and the report.
fn blended(backdrop: Layer, source: Layer, mode: &[u8; 4], opacity: u8) -> ([u8; 3], Report) {
    let mut file = psd(W, H, &[backdrop, source], NO_TILES, 0, &[0, 1]);
    let norm: Vec<usize> = (0..file.len() - 8).filter(|&i| &file[i..i + 8] == b"8BIMnorm").collect();
    assert_eq!(norm.len(), 2);
    file[norm[1] + 4..norm[1] + 8].copy_from_slice(mode);
    file[norm[1] + 8] = opacity;
    let p = aqr_psd::parse(&file).unwrap();
    let mut rep = Report::default();
    let rgb = render(&p, &file, false, &mut rep).unwrap();
    assert_eq!(rgb.len(), W * H * 3);
    for px in rgb.chunks(3) {
        assert_eq!(px, &rgb[..3], "{}: a flat canvas", String::from_utf8_lossy(mode));
    }
    ([rgb[0], rgb[1], rgb[2]], rep)
}

fn opaque(b: [u8; 3], s: [u8; 3], mode: &[u8; 4]) -> [u8; 3] {
    let (out, rep) = blended(flat(b, None), flat(s, None), mode, 255);
    assert!(rep.blend_fallback.is_empty(), "{mode:?} is known");
    out
}

fn close(a: u8, b: f64, what: &str) {
    assert!((a as f64 - b * 255.0).abs() <= 1.0, "{what}: {a} against {}", b * 255.0);
}

/// The separable modes on a few pairs of values (both sides of every branch), against the
/// definitions on [0, 1].
#[test]
fn separable_modes_follow_their_definitions() {
    type F = fn(f64, f64) -> f64;
    let modes: &[(&[u8; 4], F)] = &[
        (b"dark", |b, s| b.min(s)),
        (b"lite", |b, s| b.max(s)),
        (b"mul ", |b, s| b * s),
        (b"scrn", |b, s| b + s - b * s),
        (b"over", |b, s| if b <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        (b"hLit", |b, s| if s <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        (b"sLit", |b, s| {
            if s <= 0.5 {
                2.0 * b * s + b * b * (1.0 - 2.0 * s)
            } else {
                2.0 * b * (1.0 - s) + b.sqrt() * (2.0 * s - 1.0)
            }
        }),
        (b"div ", |b, s| {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }),
        (b"idiv", |b, s| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }),
        (b"lddg", |b, s| (b + s).min(1.0)),
        (b"lbrn", |b, s| (b + s - 1.0).max(0.0)),
        (b"diff", |b, s| (b - s).abs()),
        (b"smud", |b, s| b + s - 2.0 * b * s),
        (b"fsub", |b, s| (b - s).max(0.0)),
        (b"fdiv", |b, s| if s <= 0.0 { if b > 0.0 { 1.0 } else { 0.0 } } else { (b / s).min(1.0) }),
        (b"lLit", |b, s| (b + 2.0 * s - 1.0).clamp(0.0, 1.0)),
        (b"vLit", |b, s| {
            if s <= 0.5 {
                if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / (2.0 * s)).min(1.0) }
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (2.0 * (1.0 - s))).min(1.0)
            }
        }),
        (b"pLit", |b, s| if s <= 0.5 { b.min(2.0 * s) } else { b.max(2.0 * s - 1.0) }),
        (b"hMix", |b, s| if b + s >= 1.0 { 1.0 } else { 0.0 }),
    ];
    // (the values of the three channels at once: low, middle and high on either side, and the
    // ends of the range)
    let values: [[u8; 3]; 8] = [
        [204, 102, 51],
        [102, 204, 153],
        [0, 255, 128],
        [255, 0, 64],
        [30, 60, 90],
        [200, 220, 240],
        [0, 0, 255],
        [255, 255, 0],
    ];
    for (mode, f) in modes {
        let name = String::from_utf8_lossy(*mode).into_owned();
        for b in values {
            for s in values {
                let out = opaque(b, s, mode);
                for k in 0..3 {
                    close(
                        out[k],
                        f(b[k] as f64 / 255.0, s[k] as f64 / 255.0),
                        &format!("{name} {b:?} {s:?} channel {k}"),
                    );
                }
            }
        }
    }
}

fn lum(c: [u8; 3]) -> f64 {
    (0.3 * c[0] as f64 + 0.59 * c[1] as f64 + 0.11 * c[2] as f64) / 255.0
}

fn sat(c: [u8; 3]) -> f64 {
    (*c.iter().max().unwrap() as f64 - *c.iter().min().unwrap() as f64) / 255.0
}

/// The darker and lighter color modes pick a whole color by luminosity.
#[test]
fn color_picking_modes() {
    let (dark, light) = ([40, 30, 200], [180, 170, 60]);
    assert!(lum(dark) < lum(light));
    assert_eq!(opaque(dark, light, b"dkCl"), dark);
    assert_eq!(opaque(light, dark, b"dkCl"), dark);
    assert_eq!(opaque(dark, light, b"lgCl"), light);
    assert_eq!(opaque(light, dark, b"lgCl"), light);
}

/// The non-separable modes take hue, saturation, color (hue and saturation) or luminosity from
/// the source and the rest from the backdrop; the result has the luminosity that is kept, up to
/// the clipping of colors out of the gamut.
#[test]
fn non_separable_modes_keep_what_they_should() {
    let colors: [[u8; 3]; 6] =
        [[100, 60, 80], [20, 200, 90], [230, 40, 40], [128, 128, 128], [250, 240, 10], [5, 10, 230]];
    for b in colors {
        for s in colors {
            let hue = opaque(b, s, b"hue ");
            let sat_m = opaque(b, s, b"sat ");
            let colr = opaque(b, s, b"colr");
            let lum_m = opaque(b, s, b"lum ");
            let tol = |a: f64, b: f64, what: &str| assert!((a - b).abs() < 0.012, "{what} {b:?}: {a} {b}");
            // luminosity from the backdrop in hue, saturation, color (not where clipped)
            for (name, out) in [("hue", hue), ("sat", sat_m), ("colr", colr)] {
                if out.iter().all(|&v| v > 0 && v < 255) {
                    tol(lum(out), lum(b), name);
                }
            }
            if lum_m.iter().all(|&v| v > 0 && v < 255) {
                tol(lum(lum_m), lum(s), "lum");
            }
            if sat(s) > 0.0 && hue.iter().all(|&v| v > 0 && v < 255) {
                tol(sat(hue), sat(b), "hue's saturation");
            }
            if sat(b) > 0.0 && sat_m.iter().all(|&v| v > 0 && v < 255) {
                tol(sat(sat_m), sat(s), "sat's saturation");
            }
        }
    }
    // gray backdrop: no saturation to give to hue, and the luminosity of the source to lum
    assert_eq!(opaque([128; 3], [255, 0, 0], b"hue "), [128; 3]);
    let g = opaque([128; 3], [255, 0, 0], b"lum ")[0];
    assert!(opaque([128; 3], [255, 0, 0], b"lum ").iter().all(|&v| v == g) && (g as f64 - 76.5).abs() <= 1.0);
    // gray source: no saturation to give to sat
    let o = opaque([200, 100, 50], [90; 3], b"sat ");
    assert!(o.iter().all(|&v| v == o[0]) && (lum([200, 100, 50]) * 255.0 - o[0] as f64).abs() <= 1.0);
    // the colors out of the gamut are pulled in on either side (luminosity 0 and 1)
    assert_eq!(opaque([0; 3], [255, 0, 0], b"colr"), [0; 3]);
    assert_eq!(opaque([255; 3], [255, 0, 0], b"colr"), [255; 3]);
    let o = opaque([255; 3], [0, 0, 255], b"colr");
    assert_eq!(o, [255; 3]);
}

/// Every channel order of the source gives the result of that of the sorted values: the
/// saturation set on a color is the same for all its permutations.
#[test]
fn saturation_is_set_for_every_order_of_channels() {
    let b = [100, 60, 80];
    let vals = [50u8, 120, 200];
    let perms = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    for p in perms {
        let s = [vals[p[0]], vals[p[1]], vals[p[2]]];
        let out = opaque(b, s, b"hue ");
        assert!((sat(out) - sat(b)).abs() < 0.012, "{s:?}: {out:?}");
        assert!((lum(out) - lum(b)).abs() < 0.012, "{s:?}: {out:?}");
        // the order of the channels is that of the source
        let rank = |c: [u8; 3]| [0, 1, 2].map(|i| (0..3).filter(|&j| c[j] < c[i] || (c[j] == c[i] && j < i)).count());
        assert_eq!(rank(out), rank(s), "{s:?}: {out:?}");
    }
    // a source of one value has no hue to set
    let out = opaque(b, [90; 3], b"hue ");
    assert!(out.iter().all(|&v| v == out[0]));
}

/// A source of opacity below 1 and a backdrop of alpha below 1 mix by the compositing formula.
#[test]
fn blend_with_partial_opacity_and_backdrop() {
    let (b, s) = ([204u8, 102, 51], [102u8, 204, 153]);
    let f = |b: f64, s: f64| b * s; // multiply
    // opacity 128 over an opaque backdrop: the blend and the backdrop by the opacity
    let af = 128.0 / 255.0;
    let (out, _) = blended(flat(b, None), flat(s, None), b"mul ", 128);
    for k in 0..3 {
        let (cb, cs) = (b[k] as f64 / 255.0, s[k] as f64 / 255.0);
        close(out[k], f(cb, cs) * af + cb * (1.0 - af), "opacity");
    }
    // alpha 128 over an opaque backdrop: the same through the layer's own alpha
    let (out2, _) = blended(flat(b, None), flat(s, Some(128)), b"mul ", 255);
    for k in 0..3 {
        assert!((out[k] as i32 - out2[k] as i32).abs() <= 1, "{out:?} {out2:?}");
    }
    // an opaque source over a half transparent backdrop: opaque, the source where nothing is
    // below it mixed in the blend by the backdrop's alpha
    let ab = 128.0 / 255.0;
    let (out, _) = blended(flat(b, Some(128)), flat(s, None), b"mul ", 255);
    // (matted on white: the result is opaque, so nothing is matted)
    for k in 0..3 {
        let (cb, cs) = (b[k] as f64 / 255.0, s[k] as f64 / 255.0);
        close(out[k], (1.0 - ab) * cs + ab * f(cb, cs), "backdrop's alpha");
    }
    // both half: the result is half transparent, matted on white by the render
    let (out, _) = blended(flat(b, Some(128)), flat(s, Some(128)), b"scrn", 255);
    let oa = af + ab * (1.0 - af);
    for k in 0..3 {
        let (cb, cs) = (b[k] as f64 / 255.0, s[k] as f64 / 255.0);
        let sm = (1.0 - ab) * cs + ab * (cb + cs - cb * cs);
        let c = (sm * af + cb * ab * (1.0 - af)) / oa;
        let oa8 = (oa * 255.0).round();
        let matted = (c * 255.0).round() * oa8 / 255.0 + 255.0 * (255.0 - oa8) / 255.0;
        assert!((out[k] as f64 - matted).abs() <= 2.0, "channel {k}: {} against {matted}", out[k]);
    }
    // a transparent source changes nothing; a source over nothing is itself (a blend of nothing)
    let (out, _) = blended(flat(b, None), flat(s, Some(0)), b"mul ", 255);
    assert_eq!(out, b);
    let (out, _) = blended(flat(b, Some(0)), flat(s, None), b"mul ", 255);
    assert_eq!(out, s);
}

/// Modes the renderer does not know are rendered as normal and reported (with the fallback
/// count by mode), as are `norm` and `pass` not.
#[test]
fn unknown_modes_fall_back_to_normal() {
    let (b, s) = ([204u8, 102, 51], [102u8, 204, 153]);
    for mode in [b"diss", b"zzzz"] {
        let (out, rep) = blended(flat(b, None), flat(s, None), mode, 255);
        assert_eq!(out, s, "rendered as normal");
        assert_eq!(rep.blend_fallback.get(String::from_utf8_lossy(mode).as_ref()), Some(&1));
    }
    for mode in [b"norm", b"pass"] {
        let (out, rep) = blended(flat(b, None), flat(s, None), mode, 255);
        assert_eq!(out, s);
        assert!(rep.blend_fallback.is_empty());
    }
}
