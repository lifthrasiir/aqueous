//! Times as archives of other formats keep them, made the tree's: seconds since 1970 (FILETIME and
//! MS-DOS's times are aqr-foreign's; names of no declared encoding, `build::Undeclared`).

use aqr_formula::aqr::Time;

/// Seconds since 1970 (UTC).
pub fn unix(secs: i64) -> Time {
    Time::new(secs, 0)
}
