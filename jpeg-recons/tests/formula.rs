//! Round trips through the formula: `formula::encode`, then `Formula::eval` with this crate's
//! module.

use jpeg_recons::container;
use jpeg_recons::formula::{self, MODULE};
use recons_formula::{Apply, Formula};

fn module(a: &Apply, inputs: &[&[u8]]) -> recons_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, 0).map_err(|e| recons_formula::Error::new(e.0))
}

fn roundtrip(name: &str, data: &[u8]) -> Formula {
    let f = formula::encode(data, 0).unwrap_or_else(|| panic!("{name}: not coded"));
    assert_eq!(f.eval(&module).unwrap(), data, "{name}: roundtrip");
    f
}

/// `outer` with a segment (APP1) holding each of `inner` right after its SOI.
fn wrap(outer: &[u8], inner: &[&[u8]]) -> Vec<u8> {
    let mut v = outer[..2].to_vec();
    for i in inner {
        v.extend_from_slice(&[0xFF, 0xE1]);
        v.extend_from_slice(&(i.len() as u16 + 2).to_be_bytes());
        v.extend_from_slice(i);
    }
    v.extend_from_slice(&outer[2..]);
    v
}

const GRADIENT: &[u8] = include_bytes!("data/gradient.jpg");
const PROGRESSIVE: &[u8] = include_bytes!("data/prog_script_recode.jpg");

#[test]
fn plain() {
    for (name, data) in [("sequential", GRADIENT), ("progressive", PROGRESSIVE)] {
        let f = roundtrip(name, data);
        assert_eq!(f.applies().len(), 1);
        assert_eq!(f.applies()[0].params, [(data == PROGRESSIVE) as u8]);
        // the container codes it as well
        assert_eq!(container::decompress(&container::compress(data)).unwrap(), data);
    }
}

#[test]
fn not_coded() {
    assert!(formula::encode(b"", 0).is_none());
    assert!(formula::encode(b"not a jpeg at all", 0).is_none());
    assert!(formula::encode(&GRADIENT[..GRADIENT.len() / 2], 0).is_none());
    assert!(formula::encode(&GRADIENT[..100], 0).is_none());
}

#[test]
fn embedded() {
    let one = wrap(GRADIENT, &[GRADIENT]);
    let f = roundtrip("one", &one);
    assert_eq!(f.applies().len(), 2);
    // a repeat is a back reference: the child appears once
    let two = wrap(GRADIENT, &[GRADIENT, GRADIENT]);
    let f = roundtrip("repeat", &two);
    assert_eq!(f.applies().len(), 2);
    let mixed = wrap(GRADIENT, &[GRADIENT, PROGRESSIVE, GRADIENT, PROGRESSIVE]);
    let f = roundtrip("mixed", &mixed);
    // (the progressive one is too small to pay off)
    assert_eq!(f.applies().len(), 2);
    // not embedded JPEGs: too small, or not coded
    let small = wrap(GRADIENT, &[&GRADIENT[..700]]);
    assert_eq!(roundtrip("small", &small).applies().len(), 1);
    // the same formulas as the container makes
    for d in [&one, &two, &mixed] {
        assert_eq!(container::decompress(&container::compress(d)).unwrap(), *d);
    }
}

#[test]
fn depth() {
    // main image at depth 0, nested ones down to depth 3
    let mut v = GRADIENT.to_vec();
    for level in 1..=5 {
        v = wrap(GRADIENT, &[&v]);
        let f = roundtrip(&format!("level {level}"), &v);
        assert_eq!(f.applies().len(), (level + 1).min(4), "level {level}");
    }
}

#[test]
fn threads() {
    let v = wrap(GRADIENT, &[GRADIENT, PROGRESSIVE]);
    let f = formula::encode(&v, 2).unwrap();
    assert_eq!(f, formula::encode(&v, 0).unwrap());
    for t in [1, 3] {
        let m = |a: &Apply, i: &[&[u8]]| {
            formula::decode(&a.params, i, a.len, t).map_err(|e| recons_formula::Error::new(e.0))
        };
        assert_eq!(f.eval(&m).unwrap(), v);
    }
}

#[test]
fn corrupt_inputs() {
    let f = formula::encode(GRADIENT, 0).unwrap();
    let a = f.applies()[0].clone();
    let ins: Vec<Vec<u8>> = a
        .inputs
        .iter()
        .map(|n| Formula { streams: f.streams.clone(), root: n.clone() }.eval(&module).unwrap())
        .collect();
    let refs: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
    assert_eq!(formula::decode(&a.params, &refs, a.len, 0).unwrap(), GRADIENT);
    assert!(formula::decode(&a.params, &refs, a.len + 1, 0).is_err());
    assert!(formula::decode(&[2], &refs, a.len, 0).is_err());
    assert!(formula::decode(&a.params, &refs[1..], a.len, 0).is_err());
    // flipping any byte of any slot must not panic
    for k in 0..refs.len() {
        for i in (0..ins[k].len()).step_by(7) {
            let mut c = ins.clone();
            c[k][i] ^= 0x55;
            let r: Vec<&[u8]> = c.iter().map(|v| &v[..]).collect();
            let _ = formula::decode(&a.params, &r, a.len, 0);
        }
    }
}

/// `JPEG_BENCH=file cargo test --release --test formula -- --ignored --nocapture`: decode time of
/// the formula module against the container's.
#[test]
#[ignore]
fn decode_speed() {
    let Ok(path) = std::env::var("JPEG_BENCH") else { return };
    let data = std::fs::read(path).unwrap();
    let f = formula::encode(&data, 0).unwrap();
    let c = container::compress(&data);
    let a = f.applies()[0].clone();
    let ins: Vec<Vec<u8>> = a
        .inputs
        .iter()
        .map(|n| Formula { streams: f.streams.clone(), root: n.clone() }.eval(&module).unwrap())
        .collect();
    let refs: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
    let time = |f: &dyn Fn() -> Vec<u8>| {
        assert_eq!(f(), data);
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(f());
        }
        t.elapsed().as_secs_f64() / 20.0 * 1e3
    };
    let t_formula = time(&|| formula::decode(&a.params, &refs, a.len, 0).unwrap());
    let t_container = time(&|| container::decompress(&c).unwrap());
    println!("{} bytes: formula {t_formula:.2} ms, container {t_container:.2} ms", data.len());
}
