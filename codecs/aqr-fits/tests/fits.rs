//! FITS files through the formula: images, and tile-compressed images that fpack made (of
//! synthetic images: `tests/data`), evaluated back through the modules; corrupted inputs.

use aqr_fits::formula::{self, IMAGE, RICE, TABLE};
use aqr_formula::{Apply, Formula, Module};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        let r = match a.module {
            m if m == IMAGE => formula::decode_image(&a.params, inputs, a.len),
            m if m == RICE => formula::decode_rice(&a.params, inputs, a.len),
            m if m == TABLE => formula::decode_table(&a.params, inputs, a.len),
            m => panic!("{m}"),
        };
        r.map_err(|e| aqr_formula::Error::new(e.0))
    })
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn modules(f: &Formula) -> Vec<Module> {
    f.applies().iter().map(|a| a.module).collect()
}

/// fpack's Rice tiles (rows, squares, planes of a cube; of 8, 16 and 32-bit integers and of
/// quantised floats) rebuilt; its GZIP_1 tiles kept as they are; images coded.
#[test]
fn fixtures() {
    for (name, want) in [
        ("i16.fits.fz", &[RICE][..]),
        ("i32.fits.fz", &[RICE]),
        ("u8.fits.fz", &[RICE]),
        ("f32.fits.fz", &[RICE]),
        ("i16-tiles.fits.fz", &[RICE]),
        ("cube16-tiles.fits.fz", &[RICE]),
        ("i16-gzip.fits.fz", &[]),
        ("i16.fits", &[IMAGE]),
        ("f32.fits", &[IMAGE]),
    ] {
        let d = data(name);
        let f = formula::encode(&d, 1).unwrap();
        assert_eq!(modules(&f), want, "{name}");
        assert!(eval(&f).unwrap() == d, "{name}");
    }
}

/// A file cut short keeps its whole HDUs; what is not FITS is turned down.
#[test]
fn cut() {
    let d = data("i16.fits.fz");
    for len in [d.len() - 1, d.len() - 2880, 2880 * 3 + 100] {
        let f = formula::encode(&d[..len], 1).unwrap();
        assert!(eval(&f).unwrap() == d[..len], "{len}");
    }
    assert!(formula::encode(b"SIMPLE  = not quite", 1).is_none());
    assert!(formula::encode(&d[2880..], 1).is_none());
}

/// Corrupted inputs of the modules fail or give other bytes, never panic.
#[test]
fn corrupted() {
    for name in ["i16.fits.fz", "i16-tiles.fits.fz", "i16.fits"] {
        let d = data(name);
        let f = formula::encode(&d, 1).unwrap();
        for s in 0..f.streams.len() {
            for at in [0usize, 1, 7, 100, 1000] {
                let mut g = f.clone();
                let Some(b) = g.streams[s].data.get_mut(at) else { continue };
                *b ^= 0x5a;
                let _ = eval(&g);
                g.streams[s].data.truncate(at);
                let _ = eval(&g);
            }
        }
    }
}
