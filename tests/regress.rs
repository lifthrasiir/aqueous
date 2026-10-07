//! Regression inputs. Each one must roundtrip, and must not trip the progressive test oracle
//! (JRC_VERIFY_PROGRESSIVE: a scan judged reproducible must re-encode identically), which
//! `compress` would otherwise report as an internal panic.

use jpeg_recons::container::{self, Timing};

fn check(name: &str, data: &[u8]) {
    // SAFETY: set before any compression; the tests in this file only ever set it to "1"
    unsafe { std::env::set_var("JRC_VERIFY_PROGRESSIVE", "1") };
    let (c, why) = container::compress_with(data, &mut Timing::default());
    assert_ne!(why.as_deref(), Some("internal panic"), "{name}: oracle failed");
    assert_eq!(container::decompress(&c).unwrap(), data, "{name}: roundtrip");
}

/// Derived from lepton_jpeg_rust's half_scan_rust55.jpg by fuzzing: a later scan re-codes
/// coefficient bits an earlier scan already coded (an invalid progressive scan script), so the
/// earlier scan cannot be re-emitted from the final coefficients.
#[test]
fn progressive_script_recode() {
    check("prog_script_recode", include_bytes!("data/prog_script_recode.jpg"));
}
