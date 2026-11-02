//! Times and names as archives of other formats keep them, made the tree's: seconds since 1970,
//! and names in the code page of the system that archived them (FILETIME and MS-DOS's times are
//! aqr-foreign's).

use aqr_formula::aqr::Time;

/// Seconds since 1970 (UTC).
pub fn unix(secs: i64) -> Time {
    Time::new(secs, 0)
}

/// Name `b` as UTF-8, of the code page of the system's console (`CP_OEMCP`, as archivers of
/// MS-DOS and Windows write names): `None` where it is not of it, or the system has none.
#[cfg(windows)]
pub fn oem(b: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Globalization::{CP_OEMCP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    if b.is_empty() || b.len() > i32::MAX as usize {
        return None;
    }
    let mut w = vec![0u16; b.len()];
    // SAFETY: a buffer of as many units as there are bytes (no character takes more)
    let n = unsafe {
        MultiByteToWideChar(CP_OEMCP, MB_ERR_INVALID_CHARS, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), w.len() as i32)
    };
    (n > 0).then(|| String::from_utf16_lossy(&w[..n as usize]).into_bytes())
}

#[cfg(not(windows))]
pub fn oem(_: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Name `b` as the archive gives it, not said to be UTF-8: as it is where it is UTF-8 (as
/// archivers of Unix write it) or ASCII, else of the system's code page where it has one (on
/// Windows), else as it is.
pub fn legacy_name(b: &[u8]) -> Vec<u8> {
    if std::str::from_utf8(b).is_ok() {
        return b.to_vec();
    }
    oem(b).unwrap_or_else(|| b.to_vec())
}
