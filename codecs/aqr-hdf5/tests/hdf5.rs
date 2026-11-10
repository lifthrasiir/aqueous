//! HDF5 files through the formula (fixtures that h5py wrote of synthetic data: `tests/data`),
//! evaluated back through aqr-flate's module; corrupted inputs.

use aqr_hdf5::formula::{self, Options};
use aqr_hdf5::hdf5;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The formula of `d` (with `opt`) evaluated back, and the chunks it rebuilds.
fn roundtrip(d: &[u8], opt: &Options) -> Option<usize> {
    roundtrip_floats(d, opt).map(|r| r.0)
}

/// [`roundtrip`], and the applications of the float model.
fn roundtrip_floats(d: &[u8], opt: &Options) -> Option<(usize, usize)> {
    let e = formula::encode_with(d, opt)?;
    let floats = e.formula.applies().iter().filter(|a| a.module == formula::FLOAT).count();
    let out = e
        .formula
        .eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| formula::apply(a, ins, 1).map_err(aqr_formula::Error::new));
    assert!(out.ok().as_deref() == Some(d), "not rebuilt");
    Some((e.chunks, floats))
}

/// The datasets and chunks found, and the deflated chunks rebuilt: superblocks 0 and 3,
/// symbol tables and dense groups, B-tree, fixed array and single chunk indexes (not the
/// extensible array of `grow`), shuffle and Fletcher-32 before and after deflate; a user block,
/// and chunks whose mask skips deflate, kept. The float64 datasets (`temp`, shuffled, Fletcher-32
/// after deflate; `single`, shuffled) by the float model, but not without it.
#[test]
fn fixtures() {
    for (name, datasets, chunks, rebuilt, floats) in
        [("sb0.h5", 3, 13, 13, 1), ("sb3-dense.h5", 14, 53, 53, 1), ("userblock.h5", 1, 4, 2, 0)]
    {
        let d = data(name);
        assert!(hdf5::superblock(&d).is_some(), "{name}");
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let h = hdf5::parse(&d).unwrap();
        assert_eq!((h.datasets.len(), h.chunks.len()), (datasets, chunks), "{name}");
        for profile in [aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE] {
            let opt = Options { profile, ..Options::default() };
            assert_eq!(roundtrip_floats(&d, &opt), Some((rebuilt, floats)), "{name}, profile {profile}");
        }
        let opt = Options { floats: false, ..Options::default() };
        assert_eq!(roundtrip_floats(&d, &opt), Some((rebuilt, 0)), "{name}");
    }
    assert!(formula::starts(&data("sb0.h5")) && !formula::starts(&data("userblock.h5")));
}

/// Where the adaptive profile is tried, a test of the formula of one sample's stream alone that
/// fails ends the trial before the other chunks are made (`over`); one that passes, or no test,
/// gives the formula of all.
#[test]
fn checked() {
    let d = data("sb0.h5");
    let opt = Options::default();
    let all = formula::encode_with(&d, &opt).unwrap();
    assert!(all.adaptive && !all.over && all.chunks > 1);
    let seen = std::cell::RefCell::new(Vec::new());
    let kept = formula::encode_checked(&d, &opt, &|f| (seen.borrow_mut().push(f.applies().len()), true).1).unwrap();
    assert!(!kept.over && kept.chunks == all.chunks && kept.formula.root == all.formula.root);
    assert!(!seen.borrow().is_empty() && seen.borrow().iter().all(|&n| n == 1));
    let over = formula::encode_checked(&d, &opt, &|_| false).unwrap();
    assert!(over.over && over.adaptive && over.chunks == 0);
    // (a profile asked for that is not tried against: nothing to give up)
    let plain = Options { trial: false, ..Options::default() };
    assert!(!formula::encode_checked(&d, &plain, &|_| false).unwrap().over);
}

/// Within the limit of data, the chunks up to it are rebuilt and the rest kept.
#[test]
fn limit() {
    let d = data("sb0.h5");
    assert_eq!(roundtrip(&d, &Options { limit: 3 * 8192, ..Options::default() }), Some(3));
    assert_eq!(roundtrip(&d, &Options { limit: 0, ..Options::default() }), None);
}

/// Bytes changed anywhere, and the file cut short: the formula, where there is one, still
/// gives the input back.
#[test]
fn corrupted() {
    for name in ["sb0.h5", "sb3-dense.h5", "userblock.h5"] {
        let d = data(name);
        for (k, p) in (0..d.len()).step_by(61).enumerate() {
            let mut c = d.clone();
            c[p] ^= 1 << (k % 8);
            roundtrip(&c, &Options::default());
        }
        for n in (0..d.len()).step_by(997) {
            roundtrip(&d[..n], &Options::default());
        }
    }
}

/// The float module on inputs that are not its own: an error, never a panic.
#[test]
fn float_module_corrupted() {
    let p =
        aqr_hdf5::module::Floats { chunks: 4, elements: 512, wide: true, be: false, shuffle: true, shift: 0, order: 8 };
    let len = p.out_len().unwrap();
    let coefs = vec![0x3f; 8 * 8];
    for model in [&[][..], &[0xff; 100], &[0x12; 3000]] {
        let raw = vec![0xa5; 5000];
        let _ = aqr_hdf5::module::float(&p.to_bytes(), &[&coefs, model, &raw], len);
    }
    assert!(aqr_hdf5::module::float(&p.to_bytes(), &[&coefs[..8], &[0; 100], &[]], len).is_err());
    assert!(aqr_hdf5::module::float(&p.to_bytes()[..3], &[&coefs, &[0; 100], &[]], len).is_err());
    assert!(aqr_hdf5::module::float(&p.to_bytes(), &[&coefs, &[0; 10], &[]], len + 8).is_err());
}
