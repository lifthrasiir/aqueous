//! Regression inputs. Each one must roundtrip through the formula, and must not trip the
//! progressive test oracle (JRC_VERIFY_PROGRESSIVE: a scan judged reproducible must re-encode
//! identically), which `formula::encode` would otherwise report as not coded.

use aqr_formula::Apply;
use aqr_jpeg::formula::{self, MODULE};

fn module(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, 0).map_err(|e| aqr_formula::Error::new(e.0))
}

fn check(name: &str, data: &[u8]) {
    // SAFETY: set before any encoding; the tests in this file only ever set it to "1"
    unsafe { std::env::set_var("JRC_VERIFY_PROGRESSIVE", "1") };
    let f = formula::encode(data, 0).unwrap_or_else(|| panic!("{name}: not coded (oracle failed?)"));
    assert_eq!(f.eval(&module).unwrap(), data, "{name}: roundtrip");
}

/// Derived from lepton_jpeg_rust's half_scan_rust55.jpg by fuzzing: a later scan re-codes
/// coefficient bits an earlier scan already coded (an invalid progressive scan script), so the
/// earlier scan cannot be re-emitted from the final coefficients.
#[test]
fn progressive_script_recode() {
    check("prog_script_recode", include_bytes!("data/prog_script_recode.jpg"));
}
