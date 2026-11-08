//! The match finder's reach that the encoder chooses under a memory bound. The bound is the
//! process's, so this is a test file of its own (one test).

use aqr_lzma::lzma::Props;

/// A bound on the application's memory lowers the reach of the match finder the encoder chooses,
/// to the largest power of two that the plan fits; none keeps the dictionary's.
#[test]
fn finder_for_the_bound() {
    use aqr_lzma::plan::{Own, finder_for, set_bound};
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let n = 200 << 20;
    let dict = 64u32 << 20;
    let fin = |own| finder_for(n, dict, own);
    set_bound(u64::MAX);
    assert_eq!(fin(Own::Lzma(props)), 0);
    set_bound(2 << 30);
    assert_eq!(fin(Own::Lzma(props)), 0);
    assert_eq!(fin(Own::Lzma2), 0);
    set_bound(256 << 20);
    // (the rings: 12 bytes for each of 2^23 is 96 MiB, the tables 12.5 MiB, the window 72 MiB: 186
    // MiB with the rest; for 2^24 the rings alone are 192 MiB)
    assert_eq!(fin(Own::Lzma(props)), 23);
    assert_eq!(fin(Own::Lzma2), 23);
    // (data shorter than the reach: nothing more to hold)
    assert_eq!(finder_for(10 << 20, dict, Own::Lzma2), 0);
    // (a bound nothing fits)
    set_bound(1 << 20);
    assert_eq!(fin(Own::Lzma2), 0);
    set_bound(u64::MAX);
}
