//! ar, tar and cpio archives: aqr-bundle's formula, the contents of the members a content of
//! the application, recompressed in turn, each by its kind by its name ([`kind_by_name`]).

use aqr_formula::Formula;

use crate::kind_by_name;

pub(crate) fn bundle(d: &[u8]) -> Option<Formula> {
    aqr_bundle::formula::encode_with(d, &aqr_bundle::formula::Options { kind: kind_by_name, ..Default::default() })
}
