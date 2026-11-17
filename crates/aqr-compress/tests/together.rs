//! `-Z together:always`: the streams of a batch, of all classes, in one stream, the files given
//! back. (A test binary of its own: it sets the process's `-Z`.)

mod common;

/// [`common::files_given_back`], the streams of the batch merged in one stream of the class
/// `together`, holding the text files (t0.txt as same.txt, the first of its contents) and
/// what the zlib stream holds.
#[test]
fn files_given_back() {
    aqr_common::zopt::apply(&["together:always"]).unwrap();
    let r = common::files_given_back();
    let together = r
        .solid
        .iter()
        .any(|s| s.class == aqr_formula::aqr::TOGETHER && [1, 2, 8].iter().all(|c| s.contents.contains(c)));
    assert!(together, "{:?}", r.solid);
}
