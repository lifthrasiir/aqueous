//! HDF5 files through the formula (fixtures that h5py wrote of synthetic data: `tests/data`),
//! evaluated back through aqr-flate's module; corrupted inputs.

use aqr_hdf5::formula::{self, Options};
use aqr_hdf5::hdf5;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The formula of `d` (with `opt`) evaluated back, and the chunks it rebuilds.
fn roundtrip(d: &[u8], opt: &Options) -> Option<usize> {
    let e = formula::encode_with(d, opt)?;
    let out = e
        .formula
        .eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| formula::apply(a, ins, 1).map_err(aqr_formula::Error::new));
    assert!(out.ok().as_deref() == Some(d), "not rebuilt");
    Some(e.chunks)
}

/// The datasets and chunks found, and the deflated chunks rebuilt: superblocks 0 and 3,
/// symbol tables and dense groups, B-tree, fixed array and single chunk indexes (not the
/// extensible array of `grow`), shuffle and Fletcher-32 before and after deflate; a user block,
/// and chunks whose mask skips deflate, kept.
#[test]
fn fixtures() {
    for (name, datasets, chunks, rebuilt) in
        [("sb0.h5", 3, 13, 13), ("sb3-dense.h5", 14, 53, 53), ("userblock.h5", 1, 4, 2)]
    {
        let d = data(name);
        assert!(hdf5::superblock(&d).is_some(), "{name}");
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let h = hdf5::parse(&d).unwrap();
        assert_eq!((h.datasets.len(), h.chunks.len()), (datasets, chunks), "{name}");
        for profile in [aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE] {
            let opt = Options { profile, ..Options::default() };
            assert_eq!(roundtrip(&d, &opt), Some(rebuilt), "{name}, profile {profile}");
        }
    }
    assert!(formula::starts(&data("sb0.h5")) && !formula::starts(&data("userblock.h5")));
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
