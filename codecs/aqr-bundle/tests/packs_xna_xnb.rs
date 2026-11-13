//! xna_xnb on files written here from XNA's content format: textures (Color and DXT1, with
//! mipmaps), a sprite font's texture, a sound's samples; their formulas given back.

use aqr_bundle::packs::unity_assets::{Pixels, Raw};
use aqr_bundle::packs::xna_xnb::{Asset, asset, encode_with, files, starts};

/// A 7-bit encoded integer.
fn v7(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// An XNB file of the type readers `readers` and the primary object `object` (its reader's
/// index plus 1 first).
fn xnb(readers: &[&str], object: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    v7(&mut b, readers.len() as u32);
    for r in readers {
        let name = format!("Microsoft.Xna.Framework.Content.{r}, Microsoft.Xna.Framework.Graphics");
        v7(&mut b, name.len() as u32);
        b.extend(name.as_bytes());
        b.extend(0u32.to_le_bytes());
    }
    v7(&mut b, 0);
    b.extend(object);
    let mut d = b"XNBw\x05\x01".to_vec();
    d.extend(((10 + b.len()) as u32).to_le_bytes());
    d.extend(b);
    d
}

/// A texture's data: format, width, height, mipmaps, then each mipmap's size and bytes (of a
/// gradient, so that the coders have something to code).
fn texture(format: u32, w: u32, h: u32, sizes: &[usize]) -> Vec<u8> {
    let mut t = Vec::new();
    for v in [format, w, h, sizes.len() as u32] {
        t.extend(v.to_le_bytes());
    }
    for &n in sizes {
        t.extend((n as u32).to_le_bytes());
        t.extend((0..n).map(|i| (i / 4 + i % 4 * 50) as u8));
    }
    t
}

fn eval(f: &aqr_formula::Formula) -> Vec<u8> {
    f.eval(&|a: &aqr_formula::Apply, inputs: &[&[u8]]| {
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        let r = if a.module == aqr_tex::formula::MODULE {
            aqr_tex::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| e.0.to_string())
        } else if a.module == aqr_raw::formula::pcm::MODULE {
            aqr_raw::formula::pcm::decode(&a.params, &res, inputs, a.len, 1).map_err(|e| e.0.to_string())
        } else {
            aqr_raw::formula::raster::decode(&a.params, inputs, a.len, 1).map_err(|e| e.0.to_string())
        };
        r.map_err(aqr_formula::Error::new)
    })
    .unwrap()
}

#[test]
fn assets() {
    // Color 64 x 32, 3 mipmaps
    let color = texture(0, 64, 32, &[64 * 32 * 4, 32 * 16 * 4, 16 * 8 * 4]);
    let mut o = vec![1];
    o.extend(&color);
    let d = xnb(&["Texture2DReader"], &o);
    assert!(starts(&d));
    let Some(Asset::Texture(mips)) = asset(&d) else { panic!("a texture") };
    assert_eq!(mips[1].1, Pixels::Raw(Raw { channels: 4, width: 32, height: 16, mips: 1, layers: 1 }));
    assert_eq!(files(&d).unwrap().len(), 3);
    let f = encode_with(&d, 1).unwrap();
    assert!(eval(&f) == d);
    // DXT1 30 x 18 (8 x 5 blocks), 2 mipmaps, in a sprite font
    let dxt = texture(4, 30, 18, &[8 * 5 * 8, 4 * 3 * 8]);
    let mut o = vec![1, 2];
    o.extend(&dxt);
    o.extend(b"glyphs and kerning");
    let d = xnb(&["SpriteFontReader", "Texture2DReader"], &o);
    let Some(Asset::Texture(mips)) = asset(&d) else { panic!("a font's texture") };
    assert_eq!(mips.len(), 2);
    assert!(eval(&encode_with(&d, 1).unwrap()) == d);
    // a sound: WAVEFORMATEX of PCM, 16 bits, stereo
    let mut o = vec![1];
    o.extend(18u32.to_le_bytes());
    for v in [1u16, 2] {
        o.extend(v.to_le_bytes());
    }
    o.extend(44100u32.to_le_bytes());
    o.extend((44100u32 * 4).to_le_bytes());
    for v in [4u16, 16, 0] {
        o.extend(v.to_le_bytes());
    }
    let samples: Vec<u8> =
        (0..40000u32).flat_map(|i| (((i as f64 * 0.01).sin() * 8000.0) as i16).to_le_bytes()).collect();
    o.extend((samples.len() as u32).to_le_bytes());
    o.extend(&samples);
    o.extend([0; 12]);
    let d = xnb(&["SoundEffectReader"], &o);
    assert!(matches!(asset(&d), Some(Asset::Sound(..))));
    assert!(eval(&encode_with(&d, 1).unwrap()) == d);
}

/// Not taken: a size other than the format's, another surface format, a compressed file, a
/// file of another size than its header's, data cut short.
#[test]
fn not_taken() {
    let mut o = vec![1];
    o.extend(texture(0, 64, 32, &[64 * 32 * 4 - 4]));
    assert!(asset(&xnb(&["Texture2DReader"], &o)).is_none());
    let mut o = vec![1];
    o.extend(texture(1, 4, 4, &[32]));
    assert!(asset(&xnb(&["Texture2DReader"], &o)).is_none());
    let mut o = vec![1];
    o.extend(texture(0, 8, 8, &[256]));
    let mut d = xnb(&["Texture2DReader"], &o);
    assert!(asset(&d).is_some());
    for n in 0..d.len() {
        assert!(asset(&d[..n]).is_none(), "{n}");
    }
    d[5] |= 0x80;
    assert!(asset(&d).is_none());
}
